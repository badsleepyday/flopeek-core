use super::*;

fn invalid_store(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::new(
        io::ErrorKind::InvalidData,
        message.into(),
    )))
}

fn native_store_path(root: &Path) -> rusqlite::Result<PathBuf> {
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let metadata_directory = root.join(".flopeek");
    if metadata_directory.exists() {
        let metadata = fs::symlink_metadata(&metadata_directory)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid_store(
                ".flopeek must be a real directory inside the project root, not a symlink, junction, or file",
            ));
        }
    } else {
        fs::create_dir(&metadata_directory)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    }
    let canonical_metadata = fs::canonicalize(&metadata_directory)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    if canonical_metadata.parent() != Some(canonical_root.as_path()) {
        return Err(invalid_store(
            ".flopeek resolves outside the canonical project root",
        ));
    }
    let database_path = metadata_directory.join("native-core.sqlite3");
    if database_path.exists()
        && fs::symlink_metadata(&database_path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
            .file_type()
            .is_symlink()
    {
        return Err(invalid_store(
            "native-core.sqlite3 must not be a symlink or junction",
        ));
    }
    Ok(database_path)
}

fn sqlite_object_exists(
    connection: &Connection,
    object_type: &str,
    name: &str,
) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2)",
        rusqlite::params![object_type, name],
        |row| row.get(0),
    )
}

fn validate_native_store_schema(connection: &Connection) -> rusqlite::Result<()> {
    for table in [
        "metadata",
        "projects",
        "graph_versions",
        "projects_v2",
        "nodes_v2",
        "node_revisions_v2",
        "node_placements_v2",
        "edges_v2",
        "edge_evidence_v2",
        "node_external_ids_v2",
        "node_identity_aliases_v2",
        "edge_presence_v2",
        "placement_presence_v2",
    ] {
        if !sqlite_object_exists(connection, "table", table)? {
            return Err(invalid_store(format!(
                "native store schema v{NATIVE_STORE_SCHEMA_VERSION} is missing required table {table}"
            )));
        }
    }
    for index in ["edge_presence_v2_open", "placement_presence_v2_open"] {
        if !sqlite_object_exists(connection, "index", index)? {
            return Err(invalid_store(format!(
                "native store schema v{NATIVE_STORE_SCHEMA_VERSION} is missing required index {index}"
            )));
        }
    }
    if !sqlite_object_exists(
        connection,
        "trigger",
        "node_identity_aliases_v2_validate_insert",
    )? {
        return Err(invalid_store(format!(
            "native store schema v{NATIVE_STORE_SCHEMA_VERSION} is missing its identity alias validation trigger"
        )));
    }
    Ok(())
}

fn preflight_native_store(connection: &Connection) -> rusqlite::Result<i64> {
    let user_version =
        connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    if user_version > NATIVE_STORE_SCHEMA_VERSION {
        return Err(invalid_store(format!(
            "native store schema v{user_version} is newer than supported v{NATIVE_STORE_SCHEMA_VERSION}; refusing to modify it"
        )));
    }
    let metadata_version = if sqlite_object_exists(connection, "table", "metadata")? {
        connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| {
                value.parse::<i64>().map_err(|_| {
                    invalid_store(format!("invalid metadata schema_version value {value:?}"))
                })
            })
            .transpose()?
    } else {
        None
    };
    match (user_version, metadata_version) {
        (0, None | Some(0)) => {}
        (0, Some(metadata)) => {
            return Err(invalid_store(format!(
                "native store schema metadata v{metadata} disagrees with PRAGMA user_version 0"
            )));
        }
        (_, Some(metadata)) if metadata == user_version => {}
        (_, Some(metadata)) => {
            return Err(invalid_store(format!(
                "native store schema metadata v{metadata} disagrees with PRAGMA user_version {user_version}"
            )));
        }
        (_, None) => {
            return Err(invalid_store(format!(
                "native store schema v{user_version} has no matching metadata schema_version"
            )));
        }
    }
    if user_version == NATIVE_STORE_SCHEMA_VERSION {
        validate_native_store_schema(connection)?;
    }
    Ok(user_version)
}

pub fn open_native_store(root: &Path) -> rusqlite::Result<Connection> {
    let database_path = native_store_path(root)?;
    let mut connection = Connection::open(&database_path)?;
    let on_disk_version = preflight_native_store(&connection)?;
    connection.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA busy_timeout = 5000;
        -- Keep the first durable promotion below SQLite's default 1000-page
        -- auto-checkpoint. WAL remains crash-recoverable and bounded: once
        -- this 32 MiB threshold is reached SQLite checkpoints, while the
        -- journal-size limit prevents a completed checkpoint from retaining
        -- an unbounded file. This avoids making every cold graph promotion
        -- synchronously checkpoint its entire write set.
        PRAGMA wal_autocheckpoint = 8192;
        PRAGMA journal_size_limit = 33554432;
        -- Keep a bounded repository-local page cache during one promotion.
        -- Identity writes revisit the same indexes, but the cache must not
        -- dominate the native process peak on large graphs. This is an
        -- in-memory performance hint; it does not weaken WAL durability or
        -- alter the authoritative schema.
        PRAGMA cache_size = -8192;
        PRAGMA temp_store = MEMORY;
        ",
    )?;
    if on_disk_version == NATIVE_STORE_SCHEMA_VERSION {
        return Ok(connection);
    }
    let migration = connection.transaction()?;
    migration.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS metadata (
          key TEXT PRIMARY KEY,
          value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS projects (
          project_pk INTEGER PRIMARY KEY,
          project_id TEXT NOT NULL UNIQUE,
          current_graph_version INTEGER,
          created_at_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS scan_runs (
          scan_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          source_fingerprint TEXT NOT NULL,
          compatibility_digest TEXT,
          created_at_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS nodes (
          node_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          node_id TEXT NOT NULL,
          semantic_key TEXT NOT NULL,
          content_hash TEXT,
          kind TEXT NOT NULL,
          path TEXT,
          symbol TEXT,
          signature TEXT,
          UNIQUE(project_pk, node_id),
          UNIQUE(project_pk, semantic_key)
        );
        CREATE TABLE IF NOT EXISTS parser_facts (
          fact_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          path TEXT NOT NULL,
          source_hash TEXT NOT NULL,
          adapter_version TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          UNIQUE(project_pk, path, source_hash, adapter_version)
        );
        CREATE TABLE IF NOT EXISTS node_aliases (
          alias_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          from_node_id TEXT NOT NULL,
          to_node_id TEXT NOT NULL,
          reason TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          UNIQUE(project_pk, from_node_id, to_node_id)
        );
        CREATE TABLE IF NOT EXISTS inventory_files (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          path TEXT NOT NULL,
          size_bytes INTEGER NOT NULL,
          modified_at_ns INTEGER NOT NULL,
          source_scope TEXT NOT NULL,
          content_hash TEXT NOT NULL,
          last_seen_scan_pk INTEGER NOT NULL REFERENCES scan_runs(scan_pk),
          PRIMARY KEY(project_pk, path)
        );
        CREATE INDEX IF NOT EXISTS inventory_files_project_seen
          ON inventory_files(project_pk, last_seen_scan_pk);
        CREATE TABLE IF NOT EXISTS js_file_records (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          path TEXT NOT NULL,
          source_hash TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY(project_pk, path)
        );
        CREATE INDEX IF NOT EXISTS js_file_records_project_hash
          ON js_file_records(project_pk, source_hash);
        CREATE TABLE IF NOT EXISTS graph_versions (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          graph_version INTEGER NOT NULL,
          public_graph_version INTEGER,
          status TEXT NOT NULL CHECK(status IN ('building', 'complete')),
          material_fingerprint TEXT NOT NULL,
          source_fingerprint TEXT NOT NULL,
          compatibility_digest TEXT,
          payload_json TEXT,
          created_at_ms INTEGER NOT NULL,
          completed_at_ms INTEGER,
          PRIMARY KEY(project_pk, graph_version)
        );
        CREATE TABLE IF NOT EXISTS graph_deltas (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          from_graph_version INTEGER NOT NULL,
          to_graph_version INTEGER NOT NULL,
          payload_json TEXT NOT NULL,
          PRIMARY KEY(project_pk, from_graph_version, to_graph_version)
        );
        -- v9 stores a public graph as an envelope and content-addressed
        -- components. A version still records its exact public ordering, but
        -- unchanged component JSON is never re-written for a one-file refresh.
        CREATE TABLE IF NOT EXISTS native_public_graph_envelopes (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          graph_version INTEGER NOT NULL,
          payload_json TEXT NOT NULL,
          PRIMARY KEY(project_pk, graph_version),
          FOREIGN KEY(project_pk, graph_version)
            REFERENCES graph_versions(project_pk, graph_version)
        );
        CREATE TABLE IF NOT EXISTS native_public_graph_components (
          component_digest TEXT PRIMARY KEY,
          component_kind TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS native_public_graph_memberships (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          graph_version INTEGER NOT NULL,
          component_kind TEXT NOT NULL,
          component_id TEXT NOT NULL,
          ordinal INTEGER NOT NULL,
          component_digest TEXT NOT NULL REFERENCES native_public_graph_components(component_digest),
          PRIMARY KEY(project_pk, graph_version, component_kind, component_id),
          UNIQUE(project_pk, graph_version, component_kind, ordinal),
          FOREIGN KEY(project_pk, graph_version)
            REFERENCES graph_versions(project_pk, graph_version)
        );
        CREATE INDEX IF NOT EXISTS native_public_graph_memberships_load
          ON native_public_graph_memberships(project_pk, graph_version, component_kind, ordinal);
        -- v10 removes the remaining per-version membership rewrite. An open
        -- interval represents a component unchanged across consecutive graph
        -- versions; a changed or removed component closes only its prior row.
        CREATE TABLE IF NOT EXISTS native_public_graph_component_history (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          component_kind TEXT NOT NULL,
          component_id TEXT NOT NULL,
          first_graph_version INTEGER NOT NULL,
          last_graph_version INTEGER,
          ordinal INTEGER NOT NULL,
          component_digest TEXT NOT NULL REFERENCES native_public_graph_components(component_digest),
          PRIMARY KEY(project_pk, component_kind, component_id, first_graph_version)
        );
        CREATE INDEX IF NOT EXISTS native_public_graph_component_history_load
          ON native_public_graph_component_history(project_pk, first_graph_version, last_graph_version, component_kind, ordinal);
        -- This is a derived transport cache, never a second graph authority.
        -- It is retained only for the complete graph currently selected by the
        -- project pointer, and is promoted in the same transaction as that
        -- pointer.  Incremental fact patches must reconstruct and validate a
        -- complete StructuralFactBatch from this row before use.
        CREATE TABLE IF NOT EXISTS native_structural_batches (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          graph_version INTEGER NOT NULL,
          facts_digest TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          PRIMARY KEY(project_pk, graph_version),
          FOREIGN KEY(project_pk, graph_version)
            REFERENCES graph_versions(project_pk, graph_version)
        );
        -- v8 replaces the monolithic transport-cache JSON write on every
        -- promotion. The current complete batch is represented by one small
        -- envelope plus individually upserted parser records, so unchanged
        -- records are not rewritten into the WAL for a one-file refresh.
        CREATE TABLE IF NOT EXISTS native_structural_batch_cache (
          project_pk INTEGER PRIMARY KEY REFERENCES projects(project_pk),
          graph_version INTEGER NOT NULL,
          facts_digest TEXT NOT NULL,
          envelope_json TEXT NOT NULL,
          FOREIGN KEY(project_pk, graph_version)
            REFERENCES graph_versions(project_pk, graph_version)
        );
        CREATE TABLE IF NOT EXISTS native_structural_batch_records (
          project_pk INTEGER NOT NULL REFERENCES projects(project_pk),
          path TEXT NOT NULL,
          source_hash TEXT NOT NULL,
          record_order INTEGER NOT NULL,
          payload_json TEXT NOT NULL,
          PRIMARY KEY(project_pk, path)
        );
        CREATE INDEX IF NOT EXISTS native_structural_batch_records_order
          ON native_structural_batch_records(project_pk, record_order, path);
        ",
    )?;
    let has_source_scope = {
        let mut statement = migration.prepare("PRAGMA table_info(inventory_files)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "source_scope")
    };
    if !has_source_scope {
        migration.execute("ALTER TABLE inventory_files ADD COLUMN source_scope TEXT NOT NULL DEFAULT 'application'", [])?;
    }
    let has_current_graph_version = {
        let mut statement = migration.prepare("PRAGMA table_info(projects)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "current_graph_version")
    };
    if !has_current_graph_version {
        migration.execute(
            "ALTER TABLE projects ADD COLUMN current_graph_version INTEGER",
            [],
        )?;
    }
    let has_compatibility_digest = {
        let mut statement = migration.prepare("PRAGMA table_info(graph_versions)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "compatibility_digest")
    };
    if !has_compatibility_digest {
        migration.execute(
            "ALTER TABLE graph_versions ADD COLUMN compatibility_digest TEXT",
            [],
        )?;
    }
    let has_public_graph_version = {
        let mut statement = migration.prepare("PRAGMA table_info(graph_versions)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "public_graph_version")
    };
    if !has_public_graph_version {
        migration.execute(
            "ALTER TABLE graph_versions ADD COLUMN public_graph_version INTEGER",
            [],
        )?;
    }
    migration.execute_batch(
        "CREATE INDEX IF NOT EXISTS graph_versions_project_public_version
           ON graph_versions(project_pk, public_graph_version);",
    )?;
    // v11 is an additive identity store. Public graph v1 and
    // its component cache remain authoritative compatibility projections.
    // The complete upgrade is one transaction, so an older database is either
    // wholly upgraded or left at its original declared version after a crash.
    migration.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projects_v2 (
          project_pk INTEGER PRIMARY KEY REFERENCES projects(project_pk) ON DELETE CASCADE,
          project_uid BLOB NOT NULL UNIQUE CHECK(length(project_uid) = 16),
          public_project_id TEXT NOT NULL UNIQUE,
          identity_status TEXT NOT NULL CHECK(identity_status IN ('local', 'imported', 'ambiguous')),
          created_at_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS nodes_v2 (
          node_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects_v2(project_pk) ON DELETE CASCADE,
          node_uid BLOB NOT NULL CHECK(length(node_uid) = 16),
          kind TEXT NOT NULL,
          language TEXT,
          ecosystem TEXT,
          lexical_owner_pk INTEGER REFERENCES nodes_v2(node_pk),
          current_semantic_hash BLOB NOT NULL CHECK(length(current_semantic_hash) = 32),
          current_canonical_identity BLOB NOT NULL,
          first_seen_graph_version INTEGER NOT NULL CHECK(first_seen_graph_version >= 0),
          last_seen_graph_version INTEGER CHECK(last_seen_graph_version >= first_seen_graph_version),
          status TEXT NOT NULL CHECK(status IN ('active', 'tombstone', 'ambiguous')),
          UNIQUE(project_pk, node_uid)
        );
        CREATE TABLE IF NOT EXISTS node_revisions_v2 (
          node_revision_pk INTEGER PRIMARY KEY,
          node_pk INTEGER NOT NULL REFERENCES nodes_v2(node_pk) ON DELETE CASCADE,
          first_graph_version INTEGER NOT NULL CHECK(first_graph_version >= 0),
          last_graph_version INTEGER CHECK(last_graph_version >= first_graph_version),
          semantic_hash BLOB NOT NULL CHECK(length(semantic_hash) = 32),
          canonical_identity BLOB NOT NULL,
          revision_hash BLOB NOT NULL CHECK(length(revision_hash) = 32),
          source_sha256 BLOB CHECK(source_sha256 IS NULL OR length(source_sha256) = 32),
          content_blake3 BLOB CHECK(content_blake3 IS NULL OR length(content_blake3) = 32),
          path TEXT,
          qualified_name TEXT,
          display_name TEXT,
          signature TEXT,
          lexical_owner_pk INTEGER REFERENCES nodes_v2(node_pk),
          start_line INTEGER,
          start_column INTEGER,
          end_line INTEGER,
          end_column INTEGER,
          metadata_json TEXT,
          UNIQUE(node_pk, first_graph_version)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS node_revisions_v2_open
          ON node_revisions_v2(node_pk) WHERE last_graph_version IS NULL;
        CREATE INDEX IF NOT EXISTS nodes_v2_project_kind
          ON nodes_v2(project_pk, kind, status);
        CREATE INDEX IF NOT EXISTS nodes_v2_semantic
          ON nodes_v2(project_pk, current_semantic_hash);
        CREATE INDEX IF NOT EXISTS node_revisions_v2_semantic
          ON node_revisions_v2(semantic_hash);
        CREATE INDEX IF NOT EXISTS node_revisions_v2_path
          ON node_revisions_v2(path);
        CREATE INDEX IF NOT EXISTS node_revisions_v2_revision
          ON node_revisions_v2(revision_hash);
        CREATE TABLE IF NOT EXISTS node_placements_v2 (
          placement_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects_v2(project_pk) ON DELETE CASCADE,
          parent_node_pk INTEGER NOT NULL REFERENCES nodes_v2(node_pk) ON DELETE CASCADE,
          child_node_pk INTEGER NOT NULL REFERENCES nodes_v2(node_pk) ON DELETE CASCADE,
          relation TEXT NOT NULL,
          ordinal INTEGER,
          placement_hash BLOB NOT NULL CHECK(length(placement_hash) = 32),
          first_graph_version INTEGER NOT NULL CHECK(first_graph_version >= 0),
          last_graph_version INTEGER CHECK(last_graph_version >= first_graph_version),
          UNIQUE(project_pk, placement_hash),
          CHECK(parent_node_pk != child_node_pk)
        );
        CREATE INDEX IF NOT EXISTS node_placements_v2_parent
          ON node_placements_v2(parent_node_pk, relation, last_graph_version);
        CREATE INDEX IF NOT EXISTS node_placements_v2_child
          ON node_placements_v2(child_node_pk, relation, last_graph_version);
        CREATE TABLE IF NOT EXISTS edges_v2 (
          edge_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects_v2(project_pk) ON DELETE CASCADE,
          edge_uid BLOB NOT NULL CHECK(length(edge_uid) = 32),
          source_node_pk INTEGER NOT NULL REFERENCES nodes_v2(node_pk) ON DELETE CASCADE,
          target_node_pk INTEGER NOT NULL REFERENCES nodes_v2(node_pk) ON DELETE CASCADE,
          relation TEXT NOT NULL,
          qualifier_hash BLOB NOT NULL CHECK(length(qualifier_hash) = 32),
          canonical_qualifier BLOB NOT NULL,
          first_graph_version INTEGER NOT NULL CHECK(first_graph_version >= 0),
          last_graph_version INTEGER CHECK(last_graph_version >= first_graph_version),
          UNIQUE(project_pk, edge_uid)
        );
        CREATE INDEX IF NOT EXISTS edges_v2_source_relation
          ON edges_v2(source_node_pk, relation, last_graph_version);
        CREATE INDEX IF NOT EXISTS edges_v2_target_relation
          ON edges_v2(target_node_pk, relation, last_graph_version);
        CREATE TABLE IF NOT EXISTS edge_evidence_v2 (
          evidence_pk INTEGER PRIMARY KEY,
          edge_pk INTEGER NOT NULL REFERENCES edges_v2(edge_pk) ON DELETE CASCADE,
          evidence_uid BLOB NOT NULL CHECK(length(evidence_uid) = 32),
          first_graph_version INTEGER NOT NULL CHECK(first_graph_version >= 0),
          last_graph_version INTEGER CHECK(last_graph_version >= first_graph_version),
          path TEXT,
          start_line INTEGER,
          start_column INTEGER,
          end_line INTEGER,
          end_column INTEGER,
          parser TEXT NOT NULL,
          parser_version TEXT,
          confidence TEXT NOT NULL,
          evidence_json TEXT,
          UNIQUE(edge_pk, evidence_uid, first_graph_version)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS edge_evidence_v2_open
          ON edge_evidence_v2(edge_pk, evidence_uid) WHERE last_graph_version IS NULL;
        CREATE TABLE IF NOT EXISTS node_external_ids_v2 (
          external_id_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects_v2(project_pk) ON DELETE CASCADE,
          node_pk INTEGER NOT NULL REFERENCES nodes_v2(node_pk) ON DELETE CASCADE,
          scheme TEXT NOT NULL,
          external_id TEXT NOT NULL,
          first_graph_version INTEGER NOT NULL CHECK(first_graph_version >= 0),
          last_graph_version INTEGER CHECK(last_graph_version >= first_graph_version),
          UNIQUE(project_pk, scheme, external_id, first_graph_version)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS node_external_ids_v2_open
          ON node_external_ids_v2(project_pk, scheme, external_id)
          WHERE last_graph_version IS NULL;
        CREATE TABLE IF NOT EXISTS node_identity_aliases_v2 (
          alias_pk INTEGER PRIMARY KEY,
          project_pk INTEGER NOT NULL REFERENCES projects_v2(project_pk) ON DELETE CASCADE,
          old_node_uid BLOB NOT NULL CHECK(length(old_node_uid) = 16),
          current_node_pk INTEGER NOT NULL REFERENCES nodes_v2(node_pk) ON DELETE CASCADE,
          reason TEXT NOT NULL,
          confidence TEXT NOT NULL CHECK(confidence IN ('high', 'human-confirmed')),
          evidence_json TEXT,
          created_graph_version INTEGER NOT NULL CHECK(created_graph_version >= 0),
          confirmed_by TEXT,
          UNIQUE(project_pk, old_node_uid)
        );
        CREATE TRIGGER IF NOT EXISTS node_identity_aliases_v2_validate_insert
        BEFORE INSERT ON node_identity_aliases_v2
        BEGIN
          SELECT CASE WHEN NOT EXISTS (
            SELECT 1 FROM nodes_v2 AS target
            WHERE target.node_pk = NEW.current_node_pk AND target.project_pk = NEW.project_pk
          ) THEN RAISE(ABORT, 'identity alias target belongs to another project') END;
          SELECT CASE WHEN EXISTS (
            SELECT 1 FROM nodes_v2 AS target
            WHERE target.node_pk = NEW.current_node_pk AND target.node_uid = NEW.old_node_uid
          ) THEN RAISE(ABORT, 'identity alias cannot target itself') END;
          SELECT CASE WHEN EXISTS (
            SELECT 1 FROM node_identity_aliases_v2 AS existing
            JOIN nodes_v2 AS target ON target.node_pk = NEW.current_node_pk
            WHERE existing.project_pk = NEW.project_pk
              AND existing.old_node_uid = target.node_uid
          ) THEN RAISE(ABORT, 'identity alias chains are forbidden') END;
          SELECT CASE WHEN EXISTS (
            SELECT 1 FROM node_identity_aliases_v2 AS existing
            JOIN nodes_v2 AS existing_target ON existing_target.node_pk = existing.current_node_pk
            WHERE existing.project_pk = NEW.project_pk
              AND existing_target.node_uid = NEW.old_node_uid
          ) THEN RAISE(ABORT, 'identity alias chains are forbidden') END;
        END;

        -- v12 separates durable relationship identity from presence history.
        -- The v11 last_graph_version columns remain a compatibility projection
        -- for current-state queries; these interval tables are authoritative
        -- for historical presence and retain every absent/reappeared gap.
        CREATE TABLE IF NOT EXISTS edge_presence_v2 (
          edge_pk INTEGER NOT NULL REFERENCES edges_v2(edge_pk) ON DELETE CASCADE,
          first_graph_version INTEGER NOT NULL CHECK(first_graph_version >= 0),
          last_graph_version INTEGER CHECK(last_graph_version >= first_graph_version),
          PRIMARY KEY(edge_pk, first_graph_version)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS edge_presence_v2_open
          ON edge_presence_v2(edge_pk) WHERE last_graph_version IS NULL;
        CREATE INDEX IF NOT EXISTS edge_presence_v2_history
          ON edge_presence_v2(first_graph_version, last_graph_version, edge_pk);
        CREATE TABLE IF NOT EXISTS placement_presence_v2 (
          placement_pk INTEGER NOT NULL REFERENCES node_placements_v2(placement_pk) ON DELETE CASCADE,
          first_graph_version INTEGER NOT NULL CHECK(first_graph_version >= 0),
          last_graph_version INTEGER CHECK(last_graph_version >= first_graph_version),
          PRIMARY KEY(placement_pk, first_graph_version)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS placement_presence_v2_open
          ON placement_presence_v2(placement_pk) WHERE last_graph_version IS NULL;
        CREATE INDEX IF NOT EXISTS placement_presence_v2_history
          ON placement_presence_v2(first_graph_version, last_graph_version, placement_pk);

        -- v11 stored only one possibly-reopened interval. Preserve exactly the
        -- recoverable history during migration; future promotions append new
        -- intervals instead of rewriting this imported interval.
        INSERT OR IGNORE INTO edge_presence_v2(
          edge_pk, first_graph_version, last_graph_version
        )
        SELECT edge_pk, first_graph_version, last_graph_version FROM edges_v2;
        INSERT OR IGNORE INTO placement_presence_v2(
          placement_pk, first_graph_version, last_graph_version
        )
        SELECT placement_pk, first_graph_version, last_graph_version FROM node_placements_v2;
        ",
    )?;
    migration.execute(
        "INSERT INTO metadata(key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [NATIVE_STORE_SCHEMA_VERSION.to_string()],
    )?;
    migration.pragma_update(None, "user_version", NATIVE_STORE_SCHEMA_VERSION)?;
    validate_native_store_schema(&migration)?;
    migration.commit()?;
    Ok(connection)
}

pub fn initialize_native_store(root: &Path) -> rusqlite::Result<NativeStoreStatus> {
    let database_path = root.join(NATIVE_STORE_RELATIVE_PATH);
    let connection = open_native_store(root)?;
    let journal_mode =
        connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?;
    let foreign_keys =
        connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?;
    let synchronous_mode =
        connection.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))?;
    let busy_timeout_ms =
        connection.query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))?;
    let quick_check =
        connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))?;
    if !quick_check.eq_ignore_ascii_case("ok") {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::other(format!("SQLite quick_check failed: {quick_check}")),
        )));
    }
    Ok(NativeStoreStatus {
        path: database_path,
        schema_version: NATIVE_STORE_SCHEMA_VERSION,
        journal_mode,
        foreign_keys_enabled: foreign_keys == 1,
        synchronous_mode,
        busy_timeout_ms,
        quick_check,
    })
}
