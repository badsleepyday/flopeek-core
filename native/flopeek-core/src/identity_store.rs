use crate::identity::{public_file_node_id, public_symbol_node_id};
use crate::identity_v2::{
    EdgeIdentityInput, EdgeUid, EvidenceIdentityInput, NodeUid, ProjectUid, RevisionIdentityInput,
    SemanticIdentity, SemanticIdentityInput, edge_uid, evidence_uid, revision_hash,
    semantic_identity,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const LEGACY_PUBLIC_ID_SCHEME: &str = "legacy-js-v1";
const PARSER_SYMBOL_ID_SCHEME: &str = "parser-symbol-v2";
const EXTERNAL_IMPORT_ROOT_ID_SCHEME: &str = "external-import-root-v1";

fn conversion_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

fn invalid_query(message: impl Into<String>) -> rusqlite::Error {
    conversion_error(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

fn decode_hex_32(field: &str, value: &str) -> rusqlite::Result<[u8; 32]> {
    let value = value
        .strip_prefix("sha256:")
        .or_else(|| value.strip_prefix("blake3:"))
        .unwrap_or(value);
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_query(format!(
            "{field} must be a 32-byte hexadecimal digest"
        )));
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(conversion_error)?;
    }
    Ok(bytes)
}

fn ensure_project(
    transaction: &Transaction<'_>,
    project_pk: i64,
    public_project_id: &str,
) -> rusqlite::Result<ProjectUid> {
    let existing = transaction
        .query_row(
            "SELECT project_uid FROM projects_v2 WHERE project_pk = ?1",
            [project_pk],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        let uid = ProjectUid::from_slice(&existing).map_err(conversion_error)?;
        transaction.execute(
            "UPDATE projects_v2 SET public_project_id = ?1 WHERE project_pk = ?2",
            params![public_project_id, project_pk],
        )?;
        return Ok(uid);
    }
    let uid = ProjectUid::new_v7();
    let created_at_ms = transaction.query_row(
        "SELECT created_at_ms FROM projects WHERE project_pk = ?1",
        [project_pk],
        |row| row.get::<_, i64>(0),
    )?;
    transaction.execute(
        "INSERT INTO projects_v2(project_pk, project_uid, public_project_id, identity_status, created_at_ms)
         VALUES (?1, ?2, ?3, 'local', ?4)",
        params![project_pk, uid.as_bytes().as_slice(), public_project_id, created_at_ms],
    )?;
    Ok(uid)
}

#[derive(Debug, Clone, Copy)]
struct PublicNode<'a> {
    public_id: &'a str,
    kind: &'a str,
    node_type: Option<&'a str>,
    path: Option<&'a str>,
    label: Option<&'a str>,
    language: Option<&'a str>,
    signature: Option<&'a str>,
    evidence: Option<&'a Value>,
    object: &'a Map<String, Value>,
}

#[derive(Debug)]
struct ResolvedNode<'a> {
    fact: PublicNode<'a>,
    uid: NodeUid,
    node_pk: Option<i64>,
    owner_public_id: Option<String>,
    semantic: Option<SemanticIdentity>,
}

fn string_field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    object.get(name).and_then(Value::as_str)
}

impl PublicNode<'_> {
    fn metadata(&self) -> Value {
        let mut metadata = self.object.clone();
        for field in [
            "id",
            "kind",
            "type",
            "path",
            "label",
            "language",
            "signature",
            "evidence",
            "manualDescription",
            "hierarchy",
        ] {
            metadata.remove(field);
        }
        Value::Object(metadata)
    }
}

fn public_nodes(payload: &Value) -> rusqlite::Result<Vec<PublicNode<'_>>> {
    let values = payload
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_query("identity v2 requires a public graph nodes array"))?;
    let mut ids = BTreeSet::new();
    values
        .iter()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| invalid_query("public graph nodes must be objects"))?;
            let public_id = string_field(object, "id")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid_query("public graph nodes require non-empty IDs"))?;
            if !ids.insert(public_id) {
                return Err(invalid_query(format!(
                    "duplicate public graph node ID {public_id}"
                )));
            }
            let kind = string_field(object, "kind")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid_query(format!("node {public_id} requires a kind")))?;
            Ok(PublicNode {
                public_id,
                kind,
                node_type: string_field(object, "type"),
                path: string_field(object, "path"),
                label: string_field(object, "label"),
                language: string_field(object, "language"),
                signature: string_field(object, "signature"),
                evidence: object.get("evidence"),
                object,
            })
        })
        .collect()
}

fn source_hashes(
    structural_batch: Option<&Value>,
    changed_record_paths: Option<&BTreeSet<String>>,
) -> rusqlite::Result<BTreeMap<String, [u8; 32]>> {
    let mut hashes = BTreeMap::new();
    for record in structural_batch
        .and_then(|batch| batch.get("records"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let parsed;
        let Some(object) = (match record {
            Value::String(raw) if changed_record_paths.is_some() => {
                parsed = serde_json::from_str::<Value>(raw).map_err(conversion_error)?;
                parsed.as_object()
            }
            value => value.as_object(),
        }) else {
            continue;
        };
        let (Some(path), Some(hash)) = (
            object.get("relativePath").and_then(Value::as_str),
            object.get("sourceHash").and_then(Value::as_str),
        ) else {
            continue;
        };
        if changed_record_paths.is_some_and(|paths| !paths.contains(path)) {
            continue;
        }
        hashes.insert(path.to_string(), decode_hex_32("sourceHash", hash)?);
    }
    Ok(hashes)
}

fn content_hashes(
    transaction: &Transaction<'_>,
    project_pk: i64,
) -> rusqlite::Result<BTreeMap<String, [u8; 32]>> {
    let mut statement = transaction.prepare(
        "SELECT path, content_hash FROM inventory_files WHERE project_pk = ?1 ORDER BY path",
    )?;
    statement
        .query_map([project_pk], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (path, hash) = row?;
            Ok((path, decode_hex_32("content_hash", &hash)?))
        })
        .collect()
}

fn existing_external_ids(
    transaction: &Transaction<'_>,
    project_pk: i64,
) -> rusqlite::Result<HashMap<String, (i64, NodeUid)>> {
    let mut statement = transaction.prepare(
        "SELECT external.external_id, nodes.node_pk, nodes.node_uid
         FROM node_external_ids_v2 AS external
         JOIN nodes_v2 AS nodes ON nodes.node_pk = external.node_pk
         WHERE external.project_pk = ?1 AND external.scheme = ?2
           AND external.last_graph_version IS NULL",
    )?;
    statement
        .query_map(params![project_pk, LEGACY_PUBLIC_ID_SCHEME], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?
        .map(|row| {
            let (external_id, node_pk, uid) = row?;
            Ok((
                external_id,
                (
                    node_pk,
                    NodeUid::from_slice(&uid).map_err(conversion_error)?,
                ),
            ))
        })
        .collect()
}

fn unique_file_move_candidate(
    transaction: &Transaction<'_>,
    project_pk: i64,
    content_hash: Option<&[u8; 32]>,
    source_hash: Option<&[u8; 32]>,
    claimed: &HashSet<i64>,
) -> rusqlite::Result<Option<(i64, NodeUid)>> {
    if content_hash.is_none() && source_hash.is_none() {
        return Ok(None);
    }
    let mut statement = transaction.prepare(
        "SELECT nodes.node_pk, nodes.node_uid, revisions.content_blake3, revisions.source_sha256
         FROM nodes_v2 AS nodes
         JOIN node_revisions_v2 AS revisions ON revisions.node_pk = nodes.node_pk
         WHERE nodes.project_pk = ?1 AND nodes.kind = 'file'
           AND revisions.last_graph_version IS NULL",
    )?;
    let candidates = statement
        .query_map([project_pk], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
                row.get::<_, Option<Vec<u8>>>(3)?,
            ))
        })?
        .filter_map(|row| match row {
            Ok((node_pk, uid, candidate_content, candidate_source))
                if !claimed.contains(&node_pk)
                    && (content_hash.is_some_and(|hash| {
                        candidate_content.as_deref() == Some(hash.as_slice())
                    }) || source_hash.is_some_and(|hash| {
                        candidate_source.as_deref() == Some(hash.as_slice())
                    })) =>
            {
                Some(
                    NodeUid::from_slice(&uid)
                        .map(|uid| (node_pk, uid))
                        .map_err(conversion_error),
                )
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((candidates.len() == 1).then(|| candidates[0]))
}

fn lexical_owners(payload: &Value, nodes: &[PublicNode]) -> HashMap<String, String> {
    let kinds = nodes
        .iter()
        .map(|node| (node.public_id, node.kind))
        .collect::<HashMap<_, _>>();
    let mut candidates = HashMap::<String, BTreeSet<String>>::new();
    for edge in payload
        .get("edges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(object) = edge.as_object() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) != Some("contains") {
            continue;
        }
        let (Some(source), Some(target)) = (
            object.get("source").and_then(Value::as_str),
            object.get("target").and_then(Value::as_str),
        ) else {
            continue;
        };
        // Files remain ownerless roots in v11. Package->file containment is a
        // placement, not lexical entity ownership.
        if kinds.get(target) == Some(&"file") {
            continue;
        }
        candidates
            .entry(target.to_string())
            .or_default()
            .insert(source.to_string());
    }
    candidates
        .into_iter()
        .filter_map(|(child, parents)| {
            (parents.len() == 1).then(|| (child, parents.into_iter().next().unwrap()))
        })
        .collect()
}

fn evidence_range(
    evidence: Option<&Value>,
) -> (Option<i64>, Option<i64>, Option<i64>, Option<i64>) {
    let range = evidence.and_then(|value| value.get("range"));
    (
        range
            .and_then(|value| value.get("start"))
            .and_then(|value| value.get("line"))
            .and_then(Value::as_i64),
        range
            .and_then(|value| value.get("start"))
            .and_then(|value| value.get("column"))
            .and_then(Value::as_i64),
        range
            .and_then(|value| value.get("end"))
            .and_then(|value| value.get("line"))
            .and_then(Value::as_i64),
        range
            .and_then(|value| value.get("end"))
            .and_then(|value| value.get("column"))
            .and_then(Value::as_i64),
    )
}

fn close_version(graph_version: i64) -> i64 {
    graph_version.saturating_sub(1).max(0)
}

pub(crate) fn sync_identity_v2(
    transaction: &Transaction<'_>,
    project_pk: i64,
    public_project_id: &str,
    graph_version: i64,
    payload: &Value,
    structural_batch: Option<&Value>,
) -> rusqlite::Result<()> {
    let project_uid = ensure_project(transaction, project_pk, public_project_id)?;
    let facts = public_nodes(payload)?;
    let owners = lexical_owners(payload, &facts);
    let source_hashes = source_hashes(structural_batch, None)?;
    let identity_store_empty = transaction.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM nodes_v2 WHERE project_pk = ?1)",
        [project_pk],
        |row| row.get::<_, bool>(0),
    )?;
    let content_hashes = if identity_store_empty {
        BTreeMap::new()
    } else {
        content_hashes(transaction, project_pk)?
    };
    let external_ids = existing_external_ids(transaction, project_pk)?;
    // The first promotion starts from an empty identity store. Avoid parsing
    // guarded SQL and probing empty revision tables for every public node.
    let mut cold_node_insert = identity_store_empty
        .then(|| {
            transaction.prepare(
                "INSERT INTO nodes_v2(project_pk, node_uid, kind, language, ecosystem,
                   lexical_owner_pk, current_semantic_hash, current_canonical_identity,
                   first_seen_graph_version, last_seen_graph_version, status)
                 VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, NULL, 'active')",
            )
        })
        .transpose()?;
    let mut cold_external_insert = identity_store_empty
        .then(|| {
            transaction.prepare(
                "INSERT INTO node_external_ids_v2(project_pk, node_pk, scheme, external_id,
                   first_graph_version, last_graph_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            )
        })
        .transpose()?;
    let mut cold_revision_insert = identity_store_empty
        .then(|| {
            transaction.prepare(
                "INSERT INTO node_revisions_v2(node_pk, first_graph_version, last_graph_version,
                   semantic_hash, canonical_identity, revision_hash, source_sha256, content_blake3,
                   path, qualified_name, display_name, signature, lexical_owner_pk,
                   start_line, start_column, end_line, end_column, metadata_json)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            )
        })
        .transpose()?;
    // Reserve every exact current external-ID match before considering move
    // candidates. Otherwise a lexically earlier same-content copy could steal
    // the durable UID that still belongs to a later exact legacy ID.
    let mut claimed = facts
        .iter()
        .filter_map(|fact| {
            external_ids
                .get(fact.public_id)
                .map(|(node_pk, _)| *node_pk)
        })
        .collect::<HashSet<_>>();
    let mut resolved = Vec::with_capacity(facts.len());

    // Resolve durable UIDs without deriving them from graph order. Exact legacy
    // IDs win. A file move may reuse one unique exact content candidate. Every
    // other new entity receives UUIDv7 and remains local-store scoped.
    for fact in facts {
        let existing = if let Some(existing) = external_ids.get(fact.public_id).copied() {
            Some(existing)
        } else if fact.kind == "file" && !identity_store_empty {
            let content = fact.path.and_then(|path| content_hashes.get(path));
            let source = fact.path.and_then(|path| source_hashes.get(path));
            unique_file_move_candidate(transaction, project_pk, content, source, &claimed)?
        } else {
            None
        };
        let (node_pk, uid) = existing
            .map(|(node_pk, uid)| (Some(node_pk), uid))
            .unwrap_or_else(|| (None, NodeUid::new_v7()));
        if let Some(node_pk) = node_pk {
            claimed.insert(node_pk);
        }
        let owner_public_id = owners.get(fact.public_id).cloned();
        resolved.push(ResolvedNode {
            fact,
            uid,
            node_pk,
            owner_public_id,
            semantic: None,
        });
    }

    let mut index = resolved
        .iter()
        .enumerate()
        .map(|(index, node)| (node.fact.public_id.to_string(), index))
        .collect::<HashMap<_, _>>();
    let mut order = (0..resolved.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| usize::from(resolved[*index].owner_public_id.is_some()));

    // Resolve semantic identities through one prepared lookup. Preparing the
    // same statement for every public node made a cold promotion pay SQLite
    // parse/compile work thousands of times before any identity row changed.
    let mut semantic_candidates = if identity_store_empty {
        None
    } else {
        Some(transaction.prepare(
            "SELECT node_pk, node_uid, current_canonical_identity
             FROM nodes_v2 WHERE project_pk = ?1 AND current_semantic_hash = ?2",
        )?)
    };
    for node_index in order {
        let owner_uid = resolved[node_index]
            .owner_public_id
            .as_ref()
            .and_then(|owner| index.get(owner))
            .map(|owner_index| resolved[*owner_index].uid);
        let fact = &resolved[node_index].fact;
        let discriminator = fact.node_type.filter(|value| {
            !value.is_empty()
                && value.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_' | b'.')
                })
        });
        let semantic = semantic_identity(SemanticIdentityInput {
            project_uid: &project_uid,
            kind: fact.kind,
            language: fact.language,
            ecosystem: None,
            path: fact.path.filter(|path| *path != "."),
            qualified_name: (fact.kind != "file").then_some(fact.label).flatten(),
            owner_uid: owner_uid.as_ref(),
            signature: fact.signature,
            discriminator,
        })
        .map_err(conversion_error)?;

        // A semantic digest collision is never trusted. Exact canonical bytes
        // may be reused only when there is one unclaimed candidate.
        let candidates = if let Some(semantic_candidates) = semantic_candidates.as_mut() {
            semantic_candidates
                .query_map(
                    params![project_pk, semantic.hash().as_bytes().as_slice()],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        if candidates
            .iter()
            .any(|(_, _, canonical)| canonical != semantic.canonical())
        {
            return Err(invalid_query("fatal node semantic hash collision"));
        }
        if resolved[node_index].node_pk.is_none() {
            let reusable = candidates
                .iter()
                .filter(|(node_pk, _, canonical)| {
                    !claimed.contains(node_pk) && canonical == semantic.canonical()
                })
                .collect::<Vec<_>>();
            if reusable.len() == 1 {
                let (node_pk, uid, _) = reusable[0];
                resolved[node_index].node_pk = Some(*node_pk);
                resolved[node_index].uid = NodeUid::from_slice(uid).map_err(conversion_error)?;
                claimed.insert(*node_pk);
            }
        }
        resolved[node_index].semantic = Some(semantic);
    }
    drop(semantic_candidates);

    let resolved_node_pks = resolved
        .iter()
        .filter_map(|node| node.node_pk)
        .collect::<Vec<_>>();
    if resolved_node_pks
        .iter()
        .copied()
        .collect::<HashSet<_>>()
        .len()
        != resolved_node_pks.len()
    {
        return Err(invalid_query(
            "two current public nodes resolved to one durable node identity",
        ));
    }

    // A reused owner candidate can change the owner UID after a child semantic
    // hash was calculated. Rebuild owned identities once from the final map.
    index = resolved
        .iter()
        .enumerate()
        .map(|(index, node)| (node.fact.public_id.to_string(), index))
        .collect();
    // On an empty identity store every owner UID was allocated locally above,
    // so no move candidate can alter it. The second semantic pass is therefore
    // redundant on a cold promotion; retain it for warm/move reconciliation.
    if !identity_store_empty {
        for node_index in 0..resolved.len() {
            let owner_uid = resolved[node_index]
                .owner_public_id
                .as_ref()
                .and_then(|owner| index.get(owner))
                .map(|owner_index| resolved[*owner_index].uid);
            if owner_uid.is_none() {
                continue;
            }
            let fact = &resolved[node_index].fact;
            let discriminator = fact.node_type.filter(|value| {
                !value.is_empty()
                    && value.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'-' | b'_' | b'.')
                    })
            });
            resolved[node_index].semantic = Some(
                semantic_identity(SemanticIdentityInput {
                    project_uid: &project_uid,
                    kind: fact.kind,
                    language: fact.language,
                    ecosystem: None,
                    path: fact.path.filter(|path| *path != "."),
                    qualified_name: fact.label,
                    owner_uid: owner_uid.as_ref(),
                    signature: fact.signature,
                    discriminator,
                })
                .map_err(conversion_error)?,
            );
        }
    }

    let mut public_to_pk = HashMap::new();
    let mut public_to_uid = HashMap::new();
    for node in &mut resolved {
        let semantic = node.semantic.as_ref().expect("calculated");
        let fact = &node.fact;
        let public_id = fact.public_id.to_string();
        if let Some(node_pk) = node.node_pk {
            transaction.execute(
                "UPDATE nodes_v2 SET kind = ?1, language = ?2, ecosystem = NULL,
                   current_semantic_hash = ?3, current_canonical_identity = ?4,
                   last_seen_graph_version = NULL, status = 'active'
                 WHERE node_pk = ?5 AND project_pk = ?6",
                params![
                    fact.kind,
                    fact.language,
                    semantic.hash().as_bytes().as_slice(),
                    semantic.canonical(),
                    node_pk,
                    project_pk
                ],
            )?;
        } else {
            if let Some(statement) = cold_node_insert.as_mut() {
                statement.execute(params![
                    project_pk,
                    node.uid.as_bytes().as_slice(),
                    fact.kind,
                    fact.language,
                    semantic.hash().as_bytes().as_slice(),
                    semantic.canonical(),
                    graph_version
                ])?;
            } else {
                transaction.execute(
                    "INSERT INTO nodes_v2(project_pk, node_uid, kind, language, ecosystem,
                       lexical_owner_pk, current_semantic_hash, current_canonical_identity,
                       first_seen_graph_version, last_seen_graph_version, status)
                     VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, NULL, 'active')",
                    params![
                        project_pk,
                        node.uid.as_bytes().as_slice(),
                        fact.kind,
                        fact.language,
                        semantic.hash().as_bytes().as_slice(),
                        semantic.canonical(),
                        graph_version
                    ],
                )?;
            }
            node.node_pk = Some(transaction.last_insert_rowid());
        }
        let node_pk = node.node_pk.expect("inserted");
        public_to_pk.insert(public_id.clone(), node_pk);
        public_to_uid.insert(public_id.clone(), node.uid);
        if let Some(statement) = cold_external_insert.as_mut() {
            statement.execute(params![
                project_pk,
                node_pk,
                LEGACY_PUBLIC_ID_SCHEME,
                public_id,
                graph_version
            ])?;
        } else {
            transaction.execute(
                "INSERT INTO node_external_ids_v2(project_pk, node_pk, scheme, external_id, first_graph_version, last_graph_version)
                 SELECT ?1, ?2, ?3, ?4, ?5, NULL
                 WHERE NOT EXISTS (
                   SELECT 1 FROM node_external_ids_v2
                   WHERE project_pk = ?1 AND scheme = ?3 AND external_id = ?4 AND last_graph_version IS NULL
                 )",
                params![project_pk, node_pk, LEGACY_PUBLIC_ID_SCHEME, public_id, graph_version],
            )?;
        }
    }

    // Resolve current lexical owners only after every node has a local PK.
    let mut update_owner =
        transaction.prepare("UPDATE nodes_v2 SET lexical_owner_pk = ?1 WHERE node_pk = ?2")?;
    for node in &resolved {
        let node_pk = node.node_pk.expect("inserted");
        let owner_pk = node
            .owner_public_id
            .as_ref()
            .and_then(|owner| public_to_pk.get(owner))
            .copied();
        update_owner.execute(params![owner_pk, node_pk])?;
    }
    drop(update_owner);

    for node in &resolved {
        let node_pk = node.node_pk.expect("inserted");
        let fact = &node.fact;
        let semantic = node.semantic.as_ref().expect("calculated");
        let owner_uid = node
            .owner_public_id
            .as_ref()
            .and_then(|owner| public_to_uid.get(owner));
        let source_sha256 = fact.path.and_then(|path| source_hashes.get(path));
        let content_blake3 = (fact.kind == "file")
            .then(|| fact.path.and_then(|path| content_hashes.get(path)))
            .flatten();
        let metadata = fact.metadata();
        let revision = revision_hash(RevisionIdentityInput {
            semantic,
            lexical_owner_uid: owner_uid,
            display_name: fact.label,
            source_sha256,
            content_blake3,
            evidence: fact.evidence,
            metadata: Some(&metadata),
        })
        .map_err(conversion_error)?;
        let (start_line, start_column, end_line, end_column) = evidence_range(fact.evidence);
        let metadata_json = serde_json::to_string(&metadata).map_err(conversion_error)?;
        if let Some(statement) = cold_revision_insert.as_mut() {
            statement.execute(params![
                node_pk,
                graph_version,
                semantic.hash().as_bytes().as_slice(),
                semantic.canonical(),
                revision.as_bytes().as_slice(),
                source_sha256.map(<[u8; 32]>::as_slice),
                content_blake3.map(<[u8; 32]>::as_slice),
                fact.path,
                (fact.kind != "file").then_some(fact.label).flatten(),
                fact.label,
                fact.signature,
                node.owner_public_id
                    .as_ref()
                    .and_then(|owner| public_to_pk.get(owner))
                    .copied(),
                start_line,
                start_column,
                end_line,
                end_column,
                metadata_json
            ])?;
        } else {
            let open_revision = transaction
                .query_row(
                    "SELECT node_revision_pk, revision_hash FROM node_revisions_v2
                     WHERE node_pk = ?1 AND last_graph_version IS NULL",
                    [node_pk],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            if open_revision
                .as_ref()
                .is_some_and(|(_, hash)| hash.as_slice() == revision.as_bytes())
            {
                continue;
            }
            if let Some((revision_pk, _)) = open_revision {
                transaction.execute(
                    "UPDATE node_revisions_v2 SET last_graph_version = ?1 WHERE node_revision_pk = ?2",
                    params![close_version(graph_version), revision_pk],
                )?;
            }
            transaction.execute(
                "INSERT INTO node_revisions_v2(node_pk, first_graph_version, last_graph_version,
                   semantic_hash, canonical_identity, revision_hash, source_sha256, content_blake3,
                   path, qualified_name, display_name, signature, lexical_owner_pk,
                   start_line, start_column, end_line, end_column, metadata_json)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![node_pk, graph_version, semantic.hash().as_bytes().as_slice(), semantic.canonical(), revision.as_bytes().as_slice(),
                    source_sha256.map(<[u8; 32]>::as_slice), content_blake3.map(<[u8; 32]>::as_slice), fact.path,
                    (fact.kind != "file").then_some(fact.label).flatten(), fact.label, fact.signature,
                    node.owner_public_id.as_ref().and_then(|owner| public_to_pk.get(owner)).copied(),
                    start_line, start_column, end_line, end_column, metadata_json],
            )?;
        }
    }

    let canonical_node_pks = sync_canonical_symbols(
        transaction,
        CanonicalSymbolSync {
            project_pk,
            project_uid: &project_uid,
            graph_version,
            structural_batch,
            public_to_pk: &public_to_pk,
            public_to_uid: &public_to_uid,
            changed_record_paths: None,
        },
    )?;
    let canonical_external_pks = sync_canonical_external_import_roots(
        transaction,
        project_pk,
        &project_uid,
        graph_version,
        structural_batch,
    )?;
    let current_public_ids = resolved
        .iter()
        .map(|node| node.fact.public_id)
        .collect::<HashSet<_>>();
    let mut current_node_pks = resolved
        .iter()
        .map(|node| node.node_pk.expect("inserted"))
        .collect::<HashSet<_>>();
    current_node_pks.extend(canonical_node_pks);
    current_node_pks.extend(canonical_external_pks);
    let open_external = existing_external_ids(transaction, project_pk)?;
    for (external_id, _) in open_external {
        if !current_public_ids.contains(external_id.as_str()) {
            transaction.execute(
                "UPDATE node_external_ids_v2 SET last_graph_version = ?1
                 WHERE project_pk = ?2 AND scheme = ?3 AND external_id = ?4 AND last_graph_version IS NULL",
                params![close_version(graph_version), project_pk, LEGACY_PUBLIC_ID_SCHEME, external_id],
            )?;
        }
    }
    let mut statement = transaction
        .prepare("SELECT node_pk FROM nodes_v2 WHERE project_pk = ?1 AND status = 'active'")?;
    let active_node_pks = statement
        .query_map([project_pk], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for node_pk in active_node_pks {
        if current_node_pks.contains(&node_pk) {
            continue;
        }
        transaction.execute(
            "UPDATE nodes_v2 SET status = 'tombstone', last_seen_graph_version = ?1 WHERE node_pk = ?2",
            params![close_version(graph_version), node_pk],
        )?;
        transaction.execute(
            "UPDATE node_revisions_v2 SET last_graph_version = ?1 WHERE node_pk = ?2 AND last_graph_version IS NULL",
            params![close_version(graph_version), node_pk],
        )?;
    }

    let public_identity_index = PublicIdentityIndex {
        node_pks: &public_to_pk,
        node_uids: &public_to_uid,
        cold_start: identity_store_empty,
    };
    sync_edges_and_placements(
        transaction,
        project_pk,
        &project_uid,
        graph_version,
        payload,
        structural_batch,
        &public_identity_index,
    )?;
    Ok(())
}

/// Advance only source-backed revisions when the validated structural topology
/// and public collections are unchanged. Durable node identities, placements,
/// edges, and tombstones are already exact for the prior complete graph, so a
/// source-only refresh must not rewrite every identity row in the project.
pub(crate) fn sync_identity_v2_changed_records(
    transaction: &Transaction<'_>,
    project_pk: i64,
    public_project_id: &str,
    graph_version: i64,
    payload: &Value,
    structural_batch: Option<&Value>,
    changed_record_paths: &BTreeSet<String>,
) -> rusqlite::Result<()> {
    if changed_record_paths.is_empty() {
        return Ok(());
    }
    let project_uid = ensure_project(transaction, project_pk, public_project_id)?;
    let facts = public_nodes(payload)?;
    let owners = lexical_owners(payload, &facts);
    let source_hashes = source_hashes(structural_batch, Some(changed_record_paths))?;
    let content_hashes = content_hashes(transaction, project_pk)?;
    let external_ids = existing_external_ids(transaction, project_pk)?;
    let mut public_to_pk = HashMap::with_capacity(facts.len());
    let mut public_to_uid = HashMap::with_capacity(facts.len());
    for fact in &facts {
        let (node_pk, uid) = external_ids.get(fact.public_id).copied().ok_or_else(|| {
            invalid_query("source-only identity refresh is missing a current public node")
        })?;
        public_to_pk.insert(fact.public_id.to_string(), node_pk);
        public_to_uid.insert(fact.public_id.to_string(), uid);
    }
    // Parser symbols share the public node ID but their durable semantic
    // identity is canonicalized by `sync_canonical_symbols` below (class,
    // method, overload owner, language). Comparing those rows against the
    // coarser public `kind: symbol` projection is invalid. Update public file
    // revisions here and let the canonical-symbol pass own symbol revisions.
    for fact in facts.into_iter().filter(|fact| {
        fact.kind == "file"
            && fact
                .path
                .as_ref()
                .is_some_and(|path| changed_record_paths.contains(*path))
    }) {
        let node_pk = public_to_pk[fact.public_id];
        let owner_public_id = owners.get(fact.public_id);
        let owner_uid = owner_public_id.and_then(|owner| public_to_uid.get(owner));
        let discriminator = fact.node_type.filter(|value| {
            !value.is_empty()
                && value.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_' | b'.')
                })
        });
        let semantic = semantic_identity(SemanticIdentityInput {
            project_uid: &project_uid,
            kind: fact.kind,
            language: fact.language,
            ecosystem: None,
            path: fact.path.filter(|path| *path != "."),
            qualified_name: (fact.kind != "file").then_some(fact.label).flatten(),
            owner_uid,
            signature: fact.signature,
            discriminator,
        })
        .map_err(conversion_error)?;
        let stored_identity = transaction.query_row(
            "SELECT current_semantic_hash, current_canonical_identity, status
             FROM nodes_v2 WHERE project_pk = ?1 AND node_pk = ?2",
            params![project_pk, node_pk],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        if stored_identity.0.as_slice() != semantic.hash().as_bytes()
            || stored_identity.1 != semantic.canonical()
            || stored_identity.2 != "active"
        {
            return Err(invalid_query(format!(
                "source-only identity refresh changed durable file identity {} (hashMatch={}, canonicalMatch={}, status={})",
                fact.public_id,
                stored_identity.0.as_slice() == semantic.hash().as_bytes(),
                stored_identity.1 == semantic.canonical(),
                stored_identity.2,
            )));
        }
        let source_sha256 = fact.path.and_then(|path| source_hashes.get(path));
        let content_blake3 = (fact.kind == "file")
            .then(|| fact.path.and_then(|path| content_hashes.get(path)))
            .flatten();
        let metadata = fact.metadata();
        let revision = revision_hash(RevisionIdentityInput {
            semantic: &semantic,
            lexical_owner_uid: owner_uid,
            display_name: fact.label,
            source_sha256,
            content_blake3,
            evidence: fact.evidence,
            metadata: Some(&metadata),
        })
        .map_err(conversion_error)?;
        let open_revision = transaction
            .query_row(
                "SELECT node_revision_pk, revision_hash FROM node_revisions_v2
                 WHERE node_pk = ?1 AND last_graph_version IS NULL",
                [node_pk],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        if open_revision
            .as_ref()
            .is_some_and(|(_, hash)| hash.as_slice() == revision.as_bytes())
        {
            continue;
        }
        if let Some((revision_pk, _)) = open_revision {
            transaction.execute(
                "UPDATE node_revisions_v2 SET last_graph_version = ?1 WHERE node_revision_pk = ?2",
                params![close_version(graph_version), revision_pk],
            )?;
        }
        let (start_line, start_column, end_line, end_column) = evidence_range(fact.evidence);
        let metadata_json = serde_json::to_string(&metadata).map_err(conversion_error)?;
        transaction.execute(
            "INSERT INTO node_revisions_v2(node_pk, first_graph_version, last_graph_version,
               semantic_hash, canonical_identity, revision_hash, source_sha256, content_blake3,
               path, qualified_name, display_name, signature, lexical_owner_pk,
               start_line, start_column, end_line, end_column, metadata_json)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![node_pk, graph_version, semantic.hash().as_bytes().as_slice(), semantic.canonical(), revision.as_bytes().as_slice(),
                source_sha256.map(<[u8; 32]>::as_slice), content_blake3.map(<[u8; 32]>::as_slice), fact.path,
                (fact.kind != "file").then_some(fact.label).flatten(), fact.label, fact.signature,
                owner_public_id.and_then(|owner| public_to_pk.get(owner)).copied(),
                start_line, start_column, end_line, end_column, metadata_json],
        )?;
    }
    sync_canonical_symbols(
        transaction,
        CanonicalSymbolSync {
            project_pk,
            project_uid: &project_uid,
            graph_version,
            structural_batch,
            public_to_pk: &public_to_pk,
            public_to_uid: &public_to_uid,
            changed_record_paths: Some(changed_record_paths),
        },
    )?;
    Ok(())
}

struct CanonicalSymbolSync<'a> {
    project_pk: i64,
    project_uid: &'a ProjectUid,
    graph_version: i64,
    structural_batch: Option<&'a Value>,
    public_to_pk: &'a HashMap<String, i64>,
    public_to_uid: &'a HashMap<String, NodeUid>,
    changed_record_paths: Option<&'a BTreeSet<String>>,
}

fn sync_canonical_symbols(
    transaction: &Transaction<'_>,
    sync: CanonicalSymbolSync<'_>,
) -> rusqlite::Result<HashSet<i64>> {
    let CanonicalSymbolSync {
        project_pk,
        project_uid,
        graph_version,
        structural_batch,
        public_to_pk,
        public_to_uid,
        changed_record_paths,
    } = sync;
    let mut current_node_pks = HashSet::new();
    let mut current_external_ids = HashSet::new();
    let identity_store_empty = transaction.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM nodes_v2 WHERE project_pk = ?1)",
        [project_pk],
        |row| row.get::<_, bool>(0),
    )?;
    // Canonical symbols perform the same identity lookups and revision writes
    // for every parser symbol. Compile those statements once per promotion so
    // symbol-heavy repositories spend time on identity work, not SQL parsing.
    let mut symbol_collision = if identity_store_empty {
        None
    } else {
        Some(transaction.prepare(
            "SELECT current_canonical_identity FROM nodes_v2
             WHERE project_pk = ?1 AND current_semantic_hash = ?2
               AND current_canonical_identity <> ?3 LIMIT 1",
        )?)
    };
    let mut symbol_existing = if identity_store_empty {
        None
    } else {
        Some(transaction.prepare(
            "SELECT nodes.node_pk, nodes.current_canonical_identity
             FROM node_external_ids_v2 AS external
             JOIN nodes_v2 AS nodes ON nodes.node_pk = external.node_pk
             WHERE external.project_pk = ?1 AND external.scheme = ?2
               AND external.external_id = ?3 AND external.last_graph_version IS NULL",
        )?)
    };
    let mut symbol_bind_external = transaction.prepare(
        "INSERT INTO node_external_ids_v2(project_pk, node_pk, scheme, external_id,
           first_graph_version, last_graph_version)
         SELECT ?1, ?2, ?3, ?4, ?5, NULL
         WHERE NOT EXISTS (
           SELECT 1 FROM node_external_ids_v2
           WHERE project_pk = ?1 AND scheme = ?3 AND external_id = ?4
             AND last_graph_version IS NULL
         )",
    )?;
    let mut symbol_cold_external = identity_store_empty
        .then(|| {
            transaction.prepare(
                "INSERT INTO node_external_ids_v2(project_pk, node_pk, scheme, external_id,
                   first_graph_version, last_graph_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            )
        })
        .transpose()?;
    let mut symbol_insert_node = transaction.prepare(
        "INSERT INTO nodes_v2(project_pk, node_uid, kind, language, ecosystem,
           lexical_owner_pk, current_semantic_hash, current_canonical_identity,
           first_seen_graph_version, last_seen_graph_version, status)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, NULL, 'active')",
    )?;
    let mut symbol_insert_external = transaction.prepare(
        "INSERT INTO node_external_ids_v2(project_pk, node_pk, scheme, external_id,
           first_graph_version, last_graph_version)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
    )?;
    let mut symbol_update_node = transaction.prepare(
        "UPDATE nodes_v2 SET kind = ?1, language = ?2, lexical_owner_pk = ?3,
           current_semantic_hash = ?4, current_canonical_identity = ?5,
           last_seen_graph_version = NULL, status = 'active' WHERE node_pk = ?6",
    )?;
    let mut symbol_open_revision = transaction.prepare(
        "SELECT node_revision_pk, revision_hash, first_graph_version FROM node_revisions_v2
         WHERE node_pk = ?1 AND last_graph_version IS NULL",
    )?;
    let mut symbol_update_revision = transaction.prepare(
        "UPDATE node_revisions_v2 SET semantic_hash = ?1, canonical_identity = ?2,
           revision_hash = ?3, source_sha256 = ?4, content_blake3 = NULL,
           path = ?5, qualified_name = ?6, display_name = ?7, signature = ?8,
           lexical_owner_pk = ?9, start_line = ?10, start_column = ?11,
           end_line = ?12, end_column = ?13, metadata_json = ?14
         WHERE node_revision_pk = ?15",
    )?;
    let mut symbol_cold_update_revision = identity_store_empty
        .then(|| {
            transaction.prepare(
                "UPDATE node_revisions_v2 SET semantic_hash = ?1, canonical_identity = ?2,
                   revision_hash = ?3, source_sha256 = ?4, content_blake3 = NULL,
                   path = ?5, qualified_name = ?6, display_name = ?7, signature = ?8,
                   lexical_owner_pk = ?9, start_line = ?10, start_column = ?11,
                   end_line = ?12, end_column = ?13, metadata_json = ?14
                 WHERE node_pk = ?15 AND first_graph_version = ?16
                   AND last_graph_version IS NULL",
            )
        })
        .transpose()?;
    let mut symbol_close_revision = transaction.prepare(
        "UPDATE node_revisions_v2 SET last_graph_version = ?1 WHERE node_revision_pk = ?2",
    )?;
    let mut symbol_insert_revision = transaction.prepare(
        "INSERT INTO node_revisions_v2(node_pk, first_graph_version, last_graph_version,
           semantic_hash, canonical_identity, revision_hash, source_sha256, content_blake3,
           path, qualified_name, display_name, signature, lexical_owner_pk,
           start_line, start_column, end_line, end_column, metadata_json)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
    )?;
    let mut symbol_cold_revision = identity_store_empty
        .then(|| {
            transaction.prepare(
                "INSERT INTO node_revisions_v2(node_pk, first_graph_version, last_graph_version,
                   semantic_hash, canonical_identity, revision_hash, source_sha256, content_blake3,
                   path, qualified_name, display_name, signature, lexical_owner_pk,
                   start_line, start_column, end_line, end_column, metadata_json)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )
        })
        .transpose()?;
    for record in structural_batch
        .and_then(|batch| batch.get("records"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(record) = record.as_object() else {
            continue;
        };
        let (Some(path), Some(language), Some(symbols)) = (
            record.get("relativePath").and_then(Value::as_str),
            record.get("language").and_then(Value::as_str),
            record
                .get("result")
                .and_then(Value::as_object)
                .and_then(|result| result.get("identitySymbols"))
                .and_then(Value::as_array),
        ) else {
            continue;
        };
        if changed_record_paths.is_some_and(|paths| !paths.contains(path)) {
            continue;
        }
        let source_sha256 = record
            .get("sourceHash")
            .and_then(Value::as_str)
            .map(|value| decode_hex_32("sourceHash", value))
            .transpose()?;
        let file_id = public_file_node_id(path);
        let file_owner_pk = public_to_pk.get(&file_id).copied();
        let file_owner_uid = public_to_uid.get(&file_id).copied();
        for symbol in symbols {
            let Some(symbol) = symbol.as_object() else {
                continue;
            };
            let (Some(symbol_type), Some(name), Some(identity)) = (
                symbol.get("type").and_then(Value::as_str),
                symbol.get("name").and_then(Value::as_str),
                symbol.get("identity").and_then(Value::as_object),
            ) else {
                continue;
            };
            let qualified_name = identity
                .get("qualifiedName")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_query("canonical parser symbol requires qualifiedName"))?;
            let discriminator = identity
                .get("discriminator")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_query("canonical parser symbol requires discriminator"))?;
            let signature = identity.get("signature").and_then(Value::as_str);
            let public_symbol_type = if symbol_type == "method" {
                "function"
            } else {
                symbol_type
            };
            let public_candidate = public_symbol_node_id(path, public_symbol_type, qualified_name);
            let public_candidate_pk = public_to_pk.get(&public_candidate).copied();
            let lexical_owner = identity.get("lexicalOwner").and_then(Value::as_object);
            let owner_public_id = lexical_owner.and_then(|owner| {
                Some(public_symbol_node_id(
                    path,
                    owner.get("type")?.as_str()?,
                    owner.get("name")?.as_str()?,
                ))
            });
            let owner_pk = owner_public_id
                .as_ref()
                .and_then(|owner| public_to_pk.get(owner))
                .copied()
                .or(file_owner_pk);
            let owner_uid = owner_public_id
                .as_ref()
                .and_then(|owner| public_to_uid.get(owner))
                .copied()
                .or(file_owner_uid);
            let semantic = semantic_identity(SemanticIdentityInput {
                project_uid,
                kind: symbol_type,
                language: Some(language),
                ecosystem: None,
                path: Some(path),
                qualified_name: Some(qualified_name),
                owner_uid: owner_uid.as_ref(),
                signature,
                discriminator: Some(discriminator),
            })
            .map_err(conversion_error)?;
            let external_id = semantic.hash().to_string();
            current_external_ids.insert(external_id.clone());
            let collision = symbol_collision
                .as_mut()
                .map(|statement| {
                    statement
                        .query_row(
                            params![
                                project_pk,
                                semantic.hash().as_bytes().as_slice(),
                                semantic.canonical()
                            ],
                            |row| row.get::<_, Vec<u8>>(0),
                        )
                        .optional()
                })
                .transpose()?
                .flatten();
            if collision.is_some() {
                return Err(invalid_query("fatal parser symbol semantic hash collision"));
            }
            let existing = symbol_existing
                .as_mut()
                .map(|statement| {
                    statement
                        .query_row(
                            params![project_pk, PARSER_SYMBOL_ID_SCHEME, external_id],
                            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                        )
                        .optional()
                })
                .transpose()?
                .flatten();
            let mut node_is_new = false;
            let node_pk = if let Some(public_node_pk) = public_candidate_pk {
                if existing
                    .as_ref()
                    .is_some_and(|(node_pk, _)| *node_pk != public_node_pk)
                {
                    return Err(invalid_query(
                        "parser symbol identity is already bound to another public entity",
                    ));
                }
                if identity_store_empty {
                    symbol_cold_external
                        .as_mut()
                        .expect("cold symbol external statement")
                        .execute(params![
                            project_pk,
                            public_node_pk,
                            PARSER_SYMBOL_ID_SCHEME,
                            external_id,
                            graph_version
                        ])?;
                } else {
                    symbol_bind_external.execute(params![
                        project_pk,
                        public_node_pk,
                        PARSER_SYMBOL_ID_SCHEME,
                        external_id,
                        graph_version
                    ])?;
                }
                public_node_pk
            } else if let Some((node_pk, canonical)) = existing {
                if canonical != semantic.canonical() {
                    return Err(invalid_query("fatal parser symbol semantic hash collision"));
                }
                node_pk
            } else {
                node_is_new = true;
                let node_uid = NodeUid::new_v7();
                symbol_insert_node.execute(params![
                    project_pk,
                    node_uid.as_bytes().as_slice(),
                    symbol_type,
                    language,
                    owner_pk,
                    semantic.hash().as_bytes().as_slice(),
                    semantic.canonical(),
                    graph_version
                ])?;
                let node_pk = transaction.last_insert_rowid();
                if let Some(statement) = symbol_cold_external.as_mut() {
                    statement.execute(params![
                        project_pk,
                        node_pk,
                        PARSER_SYMBOL_ID_SCHEME,
                        external_id,
                        graph_version
                    ])?;
                } else {
                    symbol_insert_external.execute(params![
                        project_pk,
                        node_pk,
                        PARSER_SYMBOL_ID_SCHEME,
                        external_id,
                        graph_version
                    ])?;
                }
                node_pk
            };
            if !node_is_new {
                symbol_update_node.execute(params![
                    symbol_type,
                    language,
                    owner_pk,
                    semantic.hash().as_bytes().as_slice(),
                    semantic.canonical(),
                    node_pk
                ])?;
            }
            let evidence = symbol.get("evidence");
            let metadata = json!({
                "parserIdentity": identity,
                "publicCompatibility": "identity-only",
                "name": name,
            });
            let revision = revision_hash(RevisionIdentityInput {
                semantic: &semantic,
                lexical_owner_uid: owner_uid.as_ref(),
                display_name: Some(name),
                source_sha256: source_sha256.as_ref(),
                content_blake3: None,
                evidence,
                metadata: Some(&metadata),
            })
            .map_err(conversion_error)?;
            let (start_line, start_column, end_line, end_column) = evidence_range(evidence);
            let metadata_json = serde_json::to_string(&metadata).map_err(conversion_error)?;
            if node_is_new && identity_store_empty {
                symbol_cold_revision
                    .as_mut()
                    .expect("cold symbol revision statement")
                    .execute(params![
                        node_pk,
                        graph_version,
                        semantic.hash().as_bytes().as_slice(),
                        semantic.canonical(),
                        revision.as_bytes().as_slice(),
                        source_sha256.as_ref().map(|value| value.as_slice()),
                        path,
                        qualified_name,
                        name,
                        signature,
                        owner_pk,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        metadata_json
                    ])?;
                current_node_pks.insert(node_pk);
                continue;
            }
            if identity_store_empty {
                symbol_cold_update_revision
                    .as_mut()
                    .expect("cold symbol revision update statement")
                    .execute(params![
                        semantic.hash().as_bytes().as_slice(),
                        semantic.canonical(),
                        revision.as_bytes().as_slice(),
                        source_sha256.as_ref().map(|value| value.as_slice()),
                        path,
                        qualified_name,
                        name,
                        signature,
                        owner_pk,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        metadata_json,
                        node_pk,
                        graph_version
                    ])?;
                current_node_pks.insert(node_pk);
                continue;
            }
            let open_revision = symbol_open_revision
                .query_row([node_pk], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .optional()?;
            if !open_revision
                .as_ref()
                .is_some_and(|(_, hash, _)| hash.as_slice() == revision.as_bytes())
            {
                if let Some((revision_pk, _, first_version)) = open_revision
                    && first_version == graph_version
                {
                    symbol_update_revision.execute(params![
                        semantic.hash().as_bytes().as_slice(),
                        semantic.canonical(),
                        revision.as_bytes().as_slice(),
                        source_sha256.as_ref().map(|value| value.as_slice()),
                        path,
                        qualified_name,
                        name,
                        signature,
                        owner_pk,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        metadata_json,
                        revision_pk
                    ])?;
                } else {
                    if let Some((revision_pk, _, _)) = open_revision {
                        symbol_close_revision
                            .execute(params![close_version(graph_version), revision_pk])?;
                    }
                    symbol_insert_revision.execute(params![
                        node_pk,
                        graph_version,
                        semantic.hash().as_bytes().as_slice(),
                        semantic.canonical(),
                        revision.as_bytes().as_slice(),
                        source_sha256.as_ref().map(|value| value.as_slice()),
                        path,
                        qualified_name,
                        name,
                        signature,
                        owner_pk,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        metadata_json
                    ])?;
                }
            }
            current_node_pks.insert(node_pk);
        }
    }
    // A source-only refresh can close parser identities only on the changed
    // paths. Restrict the lookup at SQL level instead of loading every open
    // symbol in a large project and filtering it in Rust.
    let (sql, mut parameters) = if let Some(paths) = changed_record_paths {
        let placeholders = std::iter::repeat_n("?", paths.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT external.external_id, revisions.path FROM node_external_ids_v2 AS external
             JOIN node_revisions_v2 AS revisions ON revisions.node_pk = external.node_pk
             WHERE external.project_pk = ?1 AND external.scheme = ?2
               AND external.last_graph_version IS NULL
               AND revisions.last_graph_version IS NULL
               AND revisions.path IN ({placeholders})"
        );
        let mut parameters = vec![
            SqlValue::Integer(project_pk),
            SqlValue::Text(PARSER_SYMBOL_ID_SCHEME.to_string()),
        ];
        parameters.extend(paths.iter().cloned().map(SqlValue::Text));
        (sql, parameters)
    } else {
        (
            "SELECT external.external_id, revisions.path FROM node_external_ids_v2 AS external
             JOIN node_revisions_v2 AS revisions ON revisions.node_pk = external.node_pk
             WHERE external.project_pk = ?1 AND external.scheme = ?2
               AND external.last_graph_version IS NULL
               AND revisions.last_graph_version IS NULL"
                .to_string(),
            vec![
                SqlValue::Integer(project_pk),
                SqlValue::Text(PARSER_SYMBOL_ID_SCHEME.to_string()),
            ],
        )
    };
    let mut statement = transaction.prepare(&sql)?;
    let open_external_ids = statement
        .query_map(params_from_iter(parameters.drain(..)), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut symbol_close_external = transaction.prepare(
        "UPDATE node_external_ids_v2 SET last_graph_version = ?1
         WHERE project_pk = ?2 AND scheme = ?3 AND external_id = ?4
           AND last_graph_version IS NULL",
    )?;
    for (external_id, path) in open_external_ids {
        if changed_record_paths
            .is_some_and(|paths| path.as_ref().is_none_or(|path| !paths.contains(path)))
        {
            continue;
        }
        if !current_external_ids.contains(&external_id) {
            symbol_close_external.execute(params![
                close_version(graph_version),
                project_pk,
                PARSER_SYMBOL_ID_SCHEME,
                external_id
            ])?;
        }
    }
    Ok(current_node_pks)
}

fn persist_edge_evidence(
    transaction: &Transaction<'_>,
    graph_version: i64,
    edge_pk: i64,
    edge_identity: &EdgeUid,
    evidence: &Value,
    confidence: &str,
    current_evidence: &mut HashSet<(i64, Vec<u8>)>,
) -> rusqlite::Result<()> {
    let path = evidence.get("file").and_then(Value::as_str);
    let (start_line, start_column, end_line, end_column) = evidence_range(Some(evidence));
    let parser = evidence
        .get("parser")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let parser_version = evidence.get("parserVersion").and_then(Value::as_str);
    let evidence_identity = evidence_uid(EvidenceIdentityInput {
        edge_uid: edge_identity,
        path,
        start_line,
        start_column,
        end_line,
        end_column,
        parser,
        parser_version,
        confidence,
    })
    .map_err(conversion_error)?;
    current_evidence.insert((edge_pk, evidence_identity.as_bytes().to_vec()));
    let evidence_json = serde_json::to_string(evidence).map_err(conversion_error)?;
    transaction.execute(
        "INSERT INTO edge_evidence_v2(edge_pk, evidence_uid, first_graph_version, last_graph_version,
           path, start_line, start_column, end_line, end_column, parser, parser_version, confidence, evidence_json)
         SELECT ?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
         WHERE NOT EXISTS (
           SELECT 1 FROM edge_evidence_v2 WHERE edge_pk = ?1 AND evidence_uid = ?2 AND last_graph_version IS NULL
         )",
        params![edge_pk, evidence_identity.as_bytes().as_slice(), graph_version, path, start_line,
            start_column, end_line, end_column, parser, parser_version, confidence, evidence_json],
    )?;
    Ok(())
}

fn package_ecosystem(language: &str) -> &'static str {
    match language {
        "javascript" | "typescript" | "svelte" => "npm",
        "python" => "pypi",
        "java" => "maven",
        "rust" => "cargo",
        "go" => "go",
        "php" => "composer",
        "csharp" => "nuget",
        _ => "unknown",
    }
}

fn canonical_import_root(ecosystem: &str, specifier: &str) -> String {
    match ecosystem {
        "npm" if specifier.starts_with('@') => {
            specifier.split('/').take(2).collect::<Vec<_>>().join("/")
        }
        "npm" => specifier.split('/').next().unwrap_or(specifier).to_string(),
        "pypi" => specifier.split('.').next().unwrap_or(specifier).to_string(),
        _ => specifier.to_string(),
    }
}

fn sync_canonical_external_import_roots(
    transaction: &Transaction<'_>,
    project_pk: i64,
    project_uid: &ProjectUid,
    graph_version: i64,
    structural_batch: Option<&Value>,
) -> rusqlite::Result<HashSet<i64>> {
    let mut current_node_pks = HashSet::new();
    let mut current_external_ids = HashSet::new();
    let identity_store_empty = transaction.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM nodes_v2 WHERE project_pk = ?1)",
        [project_pk],
        |row| row.get::<_, bool>(0),
    )?;
    let mut packages = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for record in structural_batch
        .and_then(|batch| batch.get("records"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(language), Some(imports)) = (
            record.get("language").and_then(Value::as_str),
            record
                .get("result")
                .and_then(Value::as_object)
                .and_then(|result| result.get("externalImports"))
                .and_then(Value::as_array),
        ) else {
            continue;
        };
        let ecosystem = package_ecosystem(language).to_string();
        for import in imports {
            let Some(specifier) = import.get("specifier").and_then(Value::as_str) else {
                continue;
            };
            let import_root = canonical_import_root(&ecosystem, specifier);
            packages
                .entry((ecosystem.clone(), import_root))
                .or_default()
                .insert(specifier.to_string());
        }
    }
    let mut cold_node_insert = identity_store_empty
        .then(|| {
            transaction.prepare(
                "INSERT INTO nodes_v2(project_pk, node_uid, kind, language, ecosystem,
                       lexical_owner_pk, current_semantic_hash, current_canonical_identity,
                       first_seen_graph_version, last_seen_graph_version, status)
                     VALUES (?1, ?2, 'external', NULL, ?3, NULL, ?4, ?5, ?6, NULL, 'active')",
            )
        })
        .transpose()?;
    let mut cold_external_insert = identity_store_empty
        .then(|| {
            transaction.prepare(
                "INSERT INTO node_external_ids_v2(project_pk, node_pk, scheme, external_id,
                       first_graph_version, last_graph_version) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            )
        })
        .transpose()?;
    let mut cold_revision_insert = identity_store_empty
        .then(|| {
            transaction.prepare(
                "INSERT INTO node_revisions_v2(node_pk, first_graph_version, last_graph_version,
                       semantic_hash, canonical_identity, revision_hash, path, qualified_name,
                       display_name, metadata_json)
                     VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, ?6, ?7, ?8)",
            )
        })
        .transpose()?;
    for ((ecosystem, import_root), observed_specifiers) in packages {
        let external_id = format!("{ecosystem}:{import_root}");
        current_external_ids.insert(external_id.clone());
        let semantic = semantic_identity(SemanticIdentityInput {
            project_uid,
            kind: "external",
            language: None,
            ecosystem: Some(&ecosystem),
            path: None,
            qualified_name: Some(&import_root),
            owner_uid: None,
            signature: None,
            discriminator: Some("import-root"),
        })
        .map_err(conversion_error)?;
        let existing = if identity_store_empty {
            None
        } else {
            transaction
                .query_row(
                    "SELECT nodes.node_pk, nodes.current_canonical_identity
                         FROM node_external_ids_v2 AS external
                         JOIN nodes_v2 AS nodes ON nodes.node_pk = external.node_pk
                         WHERE external.project_pk = ?1 AND external.scheme = ?2
                           AND external.external_id = ?3 AND external.last_graph_version IS NULL",
                    params![project_pk, EXTERNAL_IMPORT_ROOT_ID_SCHEME, external_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?
        };
        let node_pk = if let Some((node_pk, canonical)) = existing {
            if canonical != semantic.canonical() {
                return Err(invalid_query(
                    "fatal external package semantic hash collision",
                ));
            }
            node_pk
        } else {
            let uid = NodeUid::new_v7();
            if let Some(statement) = cold_node_insert.as_mut() {
                statement.execute(params![
                    project_pk,
                    uid.as_bytes().as_slice(),
                    ecosystem,
                    semantic.hash().as_bytes().as_slice(),
                    semantic.canonical(),
                    graph_version
                ])?;
            } else {
                transaction.execute(
                    "INSERT INTO nodes_v2(project_pk, node_uid, kind, language, ecosystem,
                           lexical_owner_pk, current_semantic_hash, current_canonical_identity,
                           first_seen_graph_version, last_seen_graph_version, status)
                         VALUES (?1, ?2, 'external', NULL, ?3, NULL, ?4, ?5, ?6, NULL, 'active')",
                    params![
                        project_pk,
                        uid.as_bytes().as_slice(),
                        ecosystem,
                        semantic.hash().as_bytes().as_slice(),
                        semantic.canonical(),
                        graph_version
                    ],
                )?;
            }
            let node_pk = transaction.last_insert_rowid();
            if let Some(statement) = cold_external_insert.as_mut() {
                statement.execute(params![
                    project_pk,
                    node_pk,
                    EXTERNAL_IMPORT_ROOT_ID_SCHEME,
                    external_id,
                    graph_version
                ])?;
            } else {
                transaction.execute(
                    "INSERT INTO node_external_ids_v2(project_pk, node_pk, scheme, external_id,
                           first_graph_version, last_graph_version) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                    params![
                        project_pk,
                        node_pk,
                        EXTERNAL_IMPORT_ROOT_ID_SCHEME,
                        external_id,
                        graph_version
                    ],
                )?;
            }
            node_pk
        };
        if !identity_store_empty {
            transaction.execute(
                "UPDATE nodes_v2 SET ecosystem = ?1, current_semantic_hash = ?2,
                       current_canonical_identity = ?3, last_seen_graph_version = NULL,
                       status = 'active' WHERE node_pk = ?4",
                params![
                    ecosystem,
                    semantic.hash().as_bytes().as_slice(),
                    semantic.canonical(),
                    node_pk
                ],
            )?;
        }
        let metadata = json!({
            "ecosystem": &ecosystem,
            "canonicalImportRoot": &import_root,
            "observedSpecifiers": &observed_specifiers,
        });
        let revision = revision_hash(RevisionIdentityInput {
            semantic: &semantic,
            lexical_owner_uid: None,
            display_name: Some(&import_root),
            source_sha256: None,
            content_blake3: None,
            evidence: None,
            metadata: Some(&metadata),
        })
        .map_err(conversion_error)?;
        if let Some(statement) = cold_revision_insert.as_mut() {
            statement.execute(params![
                node_pk,
                graph_version,
                semantic.hash().as_bytes().as_slice(),
                semantic.canonical(),
                revision.as_bytes().as_slice(),
                import_root,
                import_root,
                serde_json::to_string(&metadata).map_err(conversion_error)?
            ])?;
            current_node_pks.insert(node_pk);
            continue;
        }
        let current_revision = transaction
            .query_row(
                "SELECT node_revision_pk, revision_hash FROM node_revisions_v2
                     WHERE node_pk = ?1 AND last_graph_version IS NULL",
                [node_pk],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        if !current_revision
            .as_ref()
            .is_some_and(|(_, hash)| hash.as_slice() == revision.as_bytes())
        {
            if let Some((revision_pk, _)) = current_revision {
                transaction.execute(
                        "UPDATE node_revisions_v2 SET last_graph_version = ?1 WHERE node_revision_pk = ?2",
                        params![close_version(graph_version), revision_pk],
                    )?;
            }
            transaction.execute(
                "INSERT INTO node_revisions_v2(node_pk, first_graph_version, last_graph_version,
                       semantic_hash, canonical_identity, revision_hash, path, qualified_name,
                       display_name, metadata_json)
                     VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, ?6, ?7, ?8)",
                params![
                    node_pk,
                    graph_version,
                    semantic.hash().as_bytes().as_slice(),
                    semantic.canonical(),
                    revision.as_bytes().as_slice(),
                    import_root,
                    import_root,
                    serde_json::to_string(&metadata).map_err(conversion_error)?
                ],
            )?;
        }
        current_node_pks.insert(node_pk);
    }
    let mut statement = transaction.prepare(
        "SELECT external_id FROM node_external_ids_v2
         WHERE project_pk = ?1 AND scheme = ?2 AND last_graph_version IS NULL",
    )?;
    let open_ids = statement
        .query_map(params![project_pk, EXTERNAL_IMPORT_ROOT_ID_SCHEME], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for external_id in open_ids {
        if !current_external_ids.contains(&external_id) {
            transaction.execute(
                "UPDATE node_external_ids_v2 SET last_graph_version = ?1
                 WHERE project_pk = ?2 AND scheme = ?3 AND external_id = ?4
                   AND last_graph_version IS NULL",
                params![
                    close_version(graph_version),
                    project_pk,
                    EXTERNAL_IMPORT_ROOT_ID_SCHEME,
                    external_id
                ],
            )?;
        }
    }
    Ok(current_node_pks)
}

struct PublicIdentityIndex<'a> {
    node_pks: &'a HashMap<String, i64>,
    node_uids: &'a HashMap<String, NodeUid>,
    cold_start: bool,
}

fn sync_edges_and_placements(
    transaction: &Transaction<'_>,
    project_pk: i64,
    project_uid: &ProjectUid,
    graph_version: i64,
    payload: &Value,
    structural_batch: Option<&Value>,
    public_index: &PublicIdentityIndex<'_>,
) -> rusqlite::Result<()> {
    let cold_start = public_index.cold_start;
    let mut current_edge_uids = HashSet::<Vec<u8>>::new();
    let mut current_placement_hashes = HashSet::<Vec<u8>>::new();
    let mut current_evidence = HashSet::<(i64, Vec<u8>)>::new();
    // Edges are the largest repeated write set in a cold graph. Keep the
    // statements compiled once for the whole transaction instead of asking
    // SQLite to parse the same INSERT/SELECT/presence SQL for every edge.
    let mut insert_edge = transaction.prepare(
        "INSERT INTO edges_v2(project_pk, edge_uid, source_node_pk, target_node_pk, relation,
           qualifier_hash, canonical_qualifier, first_graph_version, last_graph_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, X'', ?7, NULL)
         ON CONFLICT(project_pk, edge_uid) DO UPDATE SET
           source_node_pk = excluded.source_node_pk,
           target_node_pk = excluded.target_node_pk,
           relation = excluded.relation,
           last_graph_version = NULL
         RETURNING edge_pk",
    )?;
    let mut insert_edge_cold = cold_start
        .then(|| {
            transaction.prepare(
                "INSERT INTO edges_v2(project_pk, edge_uid, source_node_pk, target_node_pk, relation,
                   qualifier_hash, canonical_qualifier, first_graph_version, last_graph_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, X'', ?7, NULL)",
            )
        })
        .transpose()?;
    let mut insert_edge_presence = transaction.prepare(
        "INSERT INTO edge_presence_v2(edge_pk, first_graph_version, last_graph_version)
         SELECT ?1, ?2, NULL
         WHERE NOT EXISTS (
           SELECT 1 FROM edge_presence_v2 WHERE edge_pk = ?1 AND last_graph_version IS NULL
         )",
    )?;
    let mut insert_placement = transaction.prepare(
        "INSERT INTO node_placements_v2(project_pk, parent_node_pk, child_node_pk, relation,
           ordinal, placement_hash, first_graph_version, last_graph_version)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, NULL)
         ON CONFLICT(project_pk, placement_hash) DO UPDATE SET last_graph_version = NULL
         RETURNING placement_pk",
    )?;
    let mut insert_placement_cold = cold_start
        .then(|| {
            transaction.prepare(
                "INSERT INTO node_placements_v2(project_pk, parent_node_pk, child_node_pk, relation,
                   ordinal, placement_hash, first_graph_version, last_graph_version)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, NULL)",
            )
        })
        .transpose()?;
    let mut insert_placement_presence = transaction.prepare(
        "INSERT INTO placement_presence_v2(
           placement_pk, first_graph_version, last_graph_version
         )
         SELECT ?1, ?2, NULL
         WHERE NOT EXISTS (
           SELECT 1 FROM placement_presence_v2
           WHERE placement_pk = ?1 AND last_graph_version IS NULL
         )",
    )?;
    for edge in payload
        .get("edges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let object = edge
            .as_object()
            .ok_or_else(|| invalid_query("public graph edges must be objects"))?;
        let (Some(source), Some(target), Some(relation)) = (
            object.get("source").and_then(Value::as_str),
            object.get("target").and_then(Value::as_str),
            object.get("type").and_then(Value::as_str),
        ) else {
            return Err(invalid_query(
                "public graph edge requires source, target, and type",
            ));
        };
        let (Some(source_pk), Some(target_pk), Some(source_uid), Some(target_uid)) = (
            public_index.node_pks.get(source),
            public_index.node_pks.get(target),
            public_index.node_uids.get(source),
            public_index.node_uids.get(target),
        ) else {
            return Err(invalid_query(
                "public graph edge references an unknown node",
            ));
        };
        let uid = edge_uid(EdgeIdentityInput {
            project_uid,
            source_uid,
            target_uid,
            relation,
            qualifier: None,
        })
        .map_err(conversion_error)?;
        current_edge_uids.insert(uid.as_bytes().to_vec());
        let qualifier_hash = blake3::hash(b"");
        let edge_pk = if let Some(statement) = insert_edge_cold.as_mut() {
            statement.execute(params![
                project_pk,
                uid.as_bytes().as_slice(),
                source_pk,
                target_pk,
                relation,
                qualifier_hash.as_bytes().as_slice(),
                graph_version
            ])?;
            transaction.last_insert_rowid()
        } else {
            insert_edge.query_row(
                params![
                    project_pk,
                    uid.as_bytes().as_slice(),
                    source_pk,
                    target_pk,
                    relation,
                    qualifier_hash.as_bytes().as_slice(),
                    graph_version
                ],
                |row| row.get::<_, i64>(0),
            )?
        };
        if !cold_start {
            insert_edge_presence.execute(params![edge_pk, graph_version])?;
        }

        if relation == "contains" {
            current_placement_hashes.insert(uid.as_bytes().to_vec());
            let placement_pk = if let Some(statement) = insert_placement_cold.as_mut() {
                statement.execute(params![
                    project_pk,
                    source_pk,
                    target_pk,
                    relation,
                    uid.as_bytes().as_slice(),
                    graph_version
                ])?;
                transaction.last_insert_rowid()
            } else {
                insert_placement.query_row(
                    params![
                        project_pk,
                        source_pk,
                        target_pk,
                        relation,
                        uid.as_bytes().as_slice(),
                        graph_version
                    ],
                    |row| row.get::<_, i64>(0),
                )?
            };
            if !cold_start {
                insert_placement_presence.execute(params![placement_pk, graph_version])?;
            }
        }

        let evidence = object.get("evidence").filter(|value| !value.is_null());
        if let Some(evidence) = evidence {
            let confidence = object
                .get("confidence")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            persist_edge_evidence(
                transaction,
                graph_version,
                edge_pk,
                &uid,
                evidence,
                confidence,
                &mut current_evidence,
            )?;
        }
    }

    // A cold store has no existing open intervals. Materialize all presence
    // rows with two set-based inserts after the edge/placement primary keys
    // exist, instead of issuing one statement per relationship.
    if cold_start {
        transaction.execute(
            "INSERT INTO edge_presence_v2(edge_pk, first_graph_version, last_graph_version)
             SELECT edge_pk, ?2, NULL FROM edges_v2
             WHERE project_pk = ?1 AND first_graph_version = ?2",
            params![project_pk, graph_version],
        )?;
        transaction.execute(
            "INSERT INTO placement_presence_v2(placement_pk, first_graph_version, last_graph_version)
             SELECT placement_pk, ?2, NULL FROM node_placements_v2
             WHERE project_pk = ?1 AND first_graph_version = ?2",
            params![project_pk, graph_version],
        )?;
    }

    // Public graph v1 deliberately collapses equal (source, target, relation)
    // tuples. Re-read parser call facts here so every distinct source
    // occurrence remains attached to the one canonical relationship.
    for record in structural_batch
        .and_then(|batch| batch.get("records"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(record) = record.as_object() else {
            continue;
        };
        let (Some(path), Some(result)) = (
            record.get("relativePath").and_then(Value::as_str),
            record.get("result").and_then(Value::as_object),
        ) else {
            continue;
        };
        let resolved = result
            .get("resolvedImports")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                Some((
                    item.get("specifier")?.as_str()?.to_string(),
                    item.get("targetPath")?.as_str()?.to_string(),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let packages = result
            .get("resolvedPackages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                Some((
                    item.get("specifier")?.as_str()?.to_string(),
                    item.get("files")?.as_array()?.clone(),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        for call in result
            .get("calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(call) = call.as_object() else {
                continue;
            };
            let Some(name) = call.get("name").and_then(Value::as_str) else {
                continue;
            };
            let source_id = call
                .get("source")
                .and_then(Value::as_object)
                .and_then(|source| {
                    Some(public_symbol_node_id(
                        path,
                        source.get("type")?.as_str()?,
                        source.get("name")?.as_str()?,
                    ))
                })
                .filter(|candidate| public_index.node_pks.contains_key(candidate))
                .unwrap_or_else(|| public_file_node_id(path));
            let imported = call.get("imported").and_then(Value::as_object);
            let target_name = imported
                .and_then(|value| value.get("exportedName"))
                .and_then(Value::as_str)
                .unwrap_or(name);
            let target_id = if let Some(specifier) = imported
                .and_then(|value| value.get("specifier"))
                .and_then(Value::as_str)
            {
                if let Some(target_path) = resolved.get(specifier) {
                    let candidate = public_symbol_node_id(target_path, "function", target_name);
                    public_index
                        .node_pks
                        .contains_key(&candidate)
                        .then_some(candidate)
                } else {
                    let matches = packages
                        .get(specifier)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(|target_path| {
                            public_symbol_node_id(target_path, "function", target_name)
                        })
                        .filter(|candidate| public_index.node_pks.contains_key(candidate))
                        .collect::<Vec<_>>();
                    (matches.len() == 1).then(|| matches[0].clone())
                }
            } else {
                let candidate = public_symbol_node_id(path, "function", target_name);
                public_index
                    .node_pks
                    .contains_key(&candidate)
                    .then_some(candidate)
            };
            let (Some(target_id), Some(evidence)) = (
                target_id,
                call.get("evidence").filter(|value| !value.is_null()),
            ) else {
                continue;
            };
            let (Some(source_pk), Some(target_pk), Some(source_uid), Some(target_uid)) = (
                public_index.node_pks.get(&source_id),
                public_index.node_pks.get(&target_id),
                public_index.node_uids.get(&source_id),
                public_index.node_uids.get(&target_id),
            ) else {
                continue;
            };
            let uid = edge_uid(EdgeIdentityInput {
                project_uid,
                source_uid,
                target_uid,
                relation: "calls",
                qualifier: None,
            })
            .map_err(conversion_error)?;
            let edge_pk = transaction
                .query_row(
                    "SELECT edge_pk FROM edges_v2 WHERE project_pk = ?1 AND edge_uid = ?2
                     AND source_node_pk = ?3 AND target_node_pk = ?4 AND last_graph_version IS NULL",
                    params![project_pk, uid.as_bytes().as_slice(), source_pk, target_pk],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(edge_pk) = edge_pk {
                persist_edge_evidence(
                    transaction,
                    graph_version,
                    edge_pk,
                    &uid,
                    evidence,
                    "exact",
                    &mut current_evidence,
                )?;
            }
        }
    }

    let mut statement = transaction.prepare(
        "SELECT edge_pk, edge_uid FROM edges_v2 WHERE project_pk = ?1 AND last_graph_version IS NULL",
    )?;
    let active_edges = statement
        .query_map([project_pk], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (edge_pk, uid) in active_edges {
        if !current_edge_uids.contains(&uid) {
            transaction.execute(
                "UPDATE edges_v2 SET last_graph_version = ?1 WHERE edge_pk = ?2",
                params![close_version(graph_version), edge_pk],
            )?;
            transaction.execute(
                "UPDATE edge_presence_v2 SET last_graph_version = ?1
                 WHERE edge_pk = ?2 AND last_graph_version IS NULL",
                params![close_version(graph_version), edge_pk],
            )?;
        }
    }
    let mut statement = transaction.prepare(
        "SELECT placement_pk, placement_hash FROM node_placements_v2
         WHERE project_pk = ?1 AND last_graph_version IS NULL",
    )?;
    let active_placements = statement
        .query_map([project_pk], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (placement_pk, hash) in active_placements {
        if !current_placement_hashes.contains(&hash) {
            transaction.execute(
                "UPDATE node_placements_v2 SET last_graph_version = ?1 WHERE placement_pk = ?2",
                params![close_version(graph_version), placement_pk],
            )?;
            transaction.execute(
                "UPDATE placement_presence_v2 SET last_graph_version = ?1
                 WHERE placement_pk = ?2 AND last_graph_version IS NULL",
                params![close_version(graph_version), placement_pk],
            )?;
        }
    }
    let mut statement = transaction.prepare(
        "SELECT evidence.evidence_pk, evidence.edge_pk, evidence.evidence_uid
         FROM edge_evidence_v2 AS evidence
         JOIN edges_v2 AS edges ON edges.edge_pk = evidence.edge_pk
         WHERE edges.project_pk = ?1 AND evidence.last_graph_version IS NULL",
    )?;
    let active_evidence = statement
        .query_map([project_pk], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (evidence_pk, edge_pk, uid) in active_evidence {
        if !current_evidence.contains(&(edge_pk, uid)) {
            transaction.execute(
                "UPDATE edge_evidence_v2 SET last_graph_version = ?1 WHERE evidence_pk = ?2",
                params![close_version(graph_version), evidence_pk],
            )?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeNodeIdentityRecord {
    pub node_pk: i64,
    pub node_uid: String,
    pub legacy_id: Option<String>,
    pub kind: String,
    pub status: String,
    pub semantic_hash: String,
    pub revision_hash: Option<String>,
    pub path: Option<String>,
    pub qualified_name: Option<String>,
    pub display_name: Option<String>,
    pub signature: Option<String>,
    pub owner_uid: Option<String>,
    pub first_seen_graph_version: i64,
    pub last_seen_graph_version: Option<i64>,
    pub external_ids: Vec<NativeExternalNodeId>,
    pub parents: Vec<NativeNodeIdentityRelation>,
    pub history: Vec<NativeNodeIdentityRevision>,
    pub catalog: NativeNodeIdentityCatalog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeExternalNodeId {
    pub scheme: String,
    pub external_id: String,
    pub first_graph_version: i64,
    pub last_graph_version: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeNodeIdentityRelation {
    pub parent_uid: String,
    pub parent_legacy_id: Option<String>,
    pub relation: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeNodeIdentityRevision {
    pub first_graph_version: i64,
    pub last_graph_version: Option<i64>,
    pub revision_hash: String,
    pub path: Option<String>,
    pub qualified_name: Option<String>,
    pub display_name: Option<String>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NativeNodeIdentityCatalog {
    pub external_id_total: i64,
    pub parent_total: i64,
    pub revision_total: i64,
    pub truncated: bool,
}

fn encode_digest(prefix: &str, value: &[u8]) -> rusqlite::Result<String> {
    if value.len() != 32 {
        return Err(invalid_query(format!(
            "{prefix} identity digest must contain 32 bytes"
        )));
    }
    let mut encoded = String::with_capacity(prefix.len() + 65);
    encoded.push_str(prefix);
    encoded.push(':');
    for byte in value {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(encoded)
}

fn node_identity_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NativeNodeIdentityRecord> {
    let uid = NodeUid::from_slice(&row.get::<_, Vec<u8>>(1)?).map_err(conversion_error)?;
    let semantic = row.get::<_, Vec<u8>>(6)?;
    let revision = row.get::<_, Option<Vec<u8>>>(7)?;
    let owner_uid = row
        .get::<_, Option<Vec<u8>>>(13)?
        .map(|value| NodeUid::from_slice(&value).map(|uid| uid.public_id()))
        .transpose()
        .map_err(conversion_error)?;
    Ok(NativeNodeIdentityRecord {
        node_pk: row.get(0)?,
        node_uid: uid.public_id(),
        legacy_id: row.get(2)?,
        kind: row.get(3)?,
        status: row.get(4)?,
        first_seen_graph_version: row.get(5)?,
        semantic_hash: encode_digest("blake3", &semantic)?,
        revision_hash: revision
            .as_deref()
            .map(|value| encode_digest("sha256", value))
            .transpose()?,
        path: row.get(8)?,
        qualified_name: row.get(9)?,
        display_name: row.get(10)?,
        signature: row.get(11)?,
        last_seen_graph_version: row.get(12)?,
        owner_uid,
        external_ids: vec![],
        parents: vec![],
        history: vec![],
        catalog: NativeNodeIdentityCatalog::default(),
    })
}

const IDENTITY_DETAIL_LIMIT: i64 = 100;

fn hydrate_node_identity(
    connection: &Connection,
    mut record: NativeNodeIdentityRecord,
) -> rusqlite::Result<NativeNodeIdentityRecord> {
    let mut external_statement = connection.prepare(
        "SELECT scheme, external_id, first_graph_version, last_graph_version
         FROM node_external_ids_v2 WHERE node_pk = ?1
         ORDER BY (last_graph_version IS NULL) DESC, first_graph_version DESC LIMIT ?2",
    )?;
    record.external_ids = external_statement
        .query_map(params![record.node_pk, IDENTITY_DETAIL_LIMIT], |row| {
            Ok(NativeExternalNodeId {
                scheme: row.get(0)?,
                external_id: row.get(1)?,
                first_graph_version: row.get(2)?,
                last_graph_version: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut parent_statement = connection.prepare(
        "SELECT parent.node_uid,
           (SELECT external.external_id FROM node_external_ids_v2 AS external
            WHERE external.node_pk = parent.node_pk AND external.scheme = 'legacy-js-v1'
            ORDER BY (external.last_graph_version IS NULL) DESC,
              external.first_graph_version DESC LIMIT 1),
           relationships.relation, relationships.source
         FROM (
           SELECT parent_node_pk, relation, 'placement' AS source
           FROM node_placements_v2
           WHERE child_node_pk = ?1 AND last_graph_version IS NULL
           UNION
           SELECT source_node_pk, relation, 'edge' AS source
           FROM edges_v2
           WHERE target_node_pk = ?1 AND last_graph_version IS NULL
         ) AS relationships
         JOIN nodes_v2 AS parent ON parent.node_pk = relationships.parent_node_pk
         ORDER BY relationships.source, relationships.relation, parent.node_uid LIMIT ?2",
    )?;
    record.parents = parent_statement
        .query_map(params![record.node_pk, IDENTITY_DETAIL_LIMIT], |row| {
            let uid = NodeUid::from_slice(&row.get::<_, Vec<u8>>(0)?).map_err(conversion_error)?;
            Ok(NativeNodeIdentityRelation {
                parent_uid: uid.public_id(),
                parent_legacy_id: row.get(1)?,
                relation: row.get(2)?,
                source: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut history_statement = connection.prepare(
        "SELECT first_graph_version, last_graph_version, revision_hash, path,
           qualified_name, display_name, signature
         FROM node_revisions_v2 WHERE node_pk = ?1
         ORDER BY first_graph_version DESC LIMIT ?2",
    )?;
    record.history = history_statement
        .query_map(params![record.node_pk, IDENTITY_DETAIL_LIMIT], |row| {
            Ok(NativeNodeIdentityRevision {
                first_graph_version: row.get(0)?,
                last_graph_version: row.get(1)?,
                revision_hash: encode_digest("sha256", &row.get::<_, Vec<u8>>(2)?)?,
                path: row.get(3)?,
                qualified_name: row.get(4)?,
                display_name: row.get(5)?,
                signature: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    record.catalog = NativeNodeIdentityCatalog {
        external_id_total: connection.query_row(
            "SELECT COUNT(*) FROM node_external_ids_v2 WHERE node_pk = ?1",
            [record.node_pk],
            |row| row.get(0),
        )?,
        parent_total: connection.query_row(
            "SELECT COUNT(*) FROM (
               SELECT parent_node_pk, relation, 'placement' AS source
               FROM node_placements_v2 WHERE child_node_pk = ?1 AND last_graph_version IS NULL
               UNION
               SELECT source_node_pk, relation, 'edge' AS source
               FROM edges_v2 WHERE target_node_pk = ?1 AND last_graph_version IS NULL
             )",
            [record.node_pk],
            |row| row.get(0),
        )?,
        revision_total: connection.query_row(
            "SELECT COUNT(*) FROM node_revisions_v2 WHERE node_pk = ?1",
            [record.node_pk],
            |row| row.get(0),
        )?,
        truncated: false,
    };
    record.catalog.truncated = record.catalog.external_id_total > IDENTITY_DETAIL_LIMIT
        || record.catalog.parent_total > IDENTITY_DETAIL_LIMIT
        || record.catalog.revision_total > IDENTITY_DETAIL_LIMIT;
    Ok(record)
}

const NODE_IDENTITY_SELECT: &str = "SELECT nodes.node_pk, nodes.node_uid,
       (SELECT external.external_id FROM node_external_ids_v2 AS external
        WHERE external.node_pk = nodes.node_pk AND external.scheme = 'legacy-js-v1'
        ORDER BY (external.last_graph_version IS NULL) DESC,
          external.first_graph_version DESC LIMIT 1) AS legacy_id,
       nodes.kind, nodes.status, nodes.first_seen_graph_version,
       nodes.current_semantic_hash, revisions.revision_hash,
       revisions.path, revisions.qualified_name, revisions.display_name,
       revisions.signature, nodes.last_seen_graph_version, owner.node_uid
     FROM nodes_v2 AS nodes
     JOIN projects AS projects ON projects.project_pk = nodes.project_pk
     LEFT JOIN node_revisions_v2 AS revisions
       ON revisions.node_pk = nodes.node_pk AND revisions.last_graph_version IS NULL
     LEFT JOIN nodes_v2 AS owner ON owner.node_pk = nodes.lexical_owner_pk";

pub fn node_identity_by_external_id(
    connection: &Connection,
    project_id: &str,
    external_id: &str,
) -> rusqlite::Result<Option<NativeNodeIdentityRecord>> {
    let mut statement = connection.prepare(&format!(
        "{NODE_IDENTITY_SELECT}
         JOIN node_external_ids_v2 AS requested ON requested.node_pk = nodes.node_pk
         WHERE projects.project_id = ?1 AND requested.scheme = ?2
           AND requested.external_id = ?3
         ORDER BY (requested.last_graph_version IS NULL) DESC,
           requested.first_graph_version DESC"
    ))?;
    let records = statement
        .query_map(
            params![project_id, LEGACY_PUBLIC_ID_SCHEME, external_id],
            node_identity_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let distinct_node_pks = records
        .iter()
        .map(|record| record.node_pk)
        .collect::<HashSet<_>>();
    if distinct_node_pks.len() > 1 {
        return Err(invalid_query(
            "legacy external node ID is ambiguous because it was reused by multiple entities",
        ));
    }
    records
        .into_iter()
        .next()
        .map(|record| hydrate_node_identity(connection, record))
        .transpose()
}

pub fn node_identity_by_uid(
    connection: &Connection,
    project_id: &str,
    public_uid: &str,
) -> rusqlite::Result<Option<NativeNodeIdentityRecord>> {
    let uid = NodeUid::from_public_id(public_uid).map_err(conversion_error)?;
    connection
        .query_row(
            &format!(
                "{NODE_IDENTITY_SELECT}
                 WHERE projects.project_id = ?1 AND nodes.node_uid = ?2"
            ),
            params![project_id, uid.as_bytes().as_slice()],
            node_identity_from_row,
        )
        .optional()?
        .map(|record| hydrate_node_identity(connection, record))
        .transpose()
}

pub fn search_node_identities(
    connection: &Connection,
    project_id: &str,
    requested: &str,
    limit: usize,
) -> rusqlite::Result<Vec<NativeNodeIdentityRecord>> {
    let query = requested.trim().to_lowercase();
    if query.is_empty() || query.len() > 512 {
        return Ok(vec![]);
    }
    let digest = query
        .strip_prefix("sha256:")
        .or_else(|| query.strip_prefix("blake3:"))
        .unwrap_or(&query);
    let digest_prefix = if digest.len() >= 8 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        digest
    } else {
        ""
    };
    let text_pattern = format!("%{query}%");
    let digest_pattern = format!("{digest_prefix}%");
    let bounded_limit = limit.clamp(1, 50) as i64;
    let mut statement = connection.prepare(
        "SELECT DISTINCT nodes.node_uid
         FROM nodes_v2 AS nodes
         JOIN projects AS projects ON projects.project_pk = nodes.project_pk
         LEFT JOIN node_revisions_v2 AS revisions ON revisions.node_pk = nodes.node_pk
         LEFT JOIN node_external_ids_v2 AS external ON external.node_pk = nodes.node_pk
         WHERE projects.project_id = ?1 AND (
           lower(COALESCE(revisions.path, '')) LIKE ?2
           OR lower(COALESCE(revisions.qualified_name, '')) LIKE ?2
           OR lower(COALESCE(revisions.display_name, '')) LIKE ?2
           OR lower(COALESCE(revisions.signature, '')) LIKE ?2
           OR lower(COALESCE(external.external_id, '')) LIKE ?2
           OR lower('n_' || hex(nodes.node_uid)) = ?3
           OR (?4 <> '%' AND (lower(hex(nodes.current_semantic_hash)) LIKE ?4
             OR lower(hex(revisions.revision_hash)) LIKE ?4))
         )
         ORDER BY nodes.status = 'active' DESC, nodes.first_seen_graph_version DESC
         LIMIT ?5",
    )?;
    let uids = statement
        .query_map(
            params![
                project_id,
                text_pattern,
                query,
                digest_pattern,
                bounded_limit
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    uids.into_iter()
        .map(|uid| {
            let uid = NodeUid::from_slice(&uid).map_err(conversion_error)?;
            node_identity_by_uid(connection, project_id, &uid.public_id())?
                .ok_or_else(|| invalid_query("identity search returned a missing node UID"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::decode_hex_32;

    #[test]
    fn digest_decoder_requires_exact_binary_length() {
        assert_eq!(decode_hex_32("hash", &"ab".repeat(32)).unwrap(), [0xab; 32]);
        assert!(decode_hex_32("hash", "ab").is_err());
        assert!(decode_hex_32("hash", &"zz".repeat(32)).is_err());
    }
}
