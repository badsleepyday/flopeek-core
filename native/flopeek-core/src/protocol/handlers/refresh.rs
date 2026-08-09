use super::super::*;

pub(in crate::protocol) fn project_root(params: &Value) -> Result<PathBuf, NativeProtocolError> {
    let Some(value) = params.get("projectRoot").and_then(Value::as_str) else {
        return Err(NativeProtocolError {
            code: "invalid-params",
            message: "initialize requires params.projectRoot as a directory path.".to_string(),
        });
    };
    let root = PathBuf::from(value);
    if !root.is_dir() {
        return Err(NativeProtocolError {
            code: "invalid-project-root",
            message: "initialize params.projectRoot must resolve to an existing directory."
                .to_string(),
        });
    }
    Ok(root)
}

pub(in crate::protocol) fn native_incremental_manifest(
    params: &Value,
) -> Result<Value, NativeProtocolError> {
    let root = project_root(params)?;
    let include_source_batch = params
        .get("includeSourceBatch")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let manifest = (if include_source_batch {
        scan_native_incremental_manifest_with_source_batch(&root)
    } else {
        scan_native_incremental_manifest(&root)
    })
    .map_err(|message| NativeProtocolError {
        code: "native-incremental-manifest-failed",
        message,
    })?;
    let inventory = manifest.inventory;
    let source_batch = inventory.source_batch_records.as_ref().map(|records| {
        json!({
            "schemaVersion": "flopeek-native-ephemeral-source-batch/v1",
            "records": records.iter().map(|record| json!({
                "path": &record.path,
                "utf8": &record.utf8,
                "sizeBytes": record.size_bytes,
                "modifiedAtNs": record.modified_at_ns.to_string(),
            })).collect::<Vec<_>>(),
            "omittedFiles": inventory.source_batch_omitted_files,
            "persistence": "ephemeral-jsonl-only",
            "limitation": "Source text is returned only for the current bounded JSONL request. It is not accepted by StructuralFactBatch/v1 and is never written to SQLite or the JS record cache.",
        })
    });
    Ok(json!({
        "schemaVersion": "flopeek-native-incremental-manifest/v1",
        "mode": "native-incremental-manifest",
        "projectRoot": inventory.project_root,
        "projectId": inventory.project_identity.project_id,
        "sourceFingerprint": inventory.source_fingerprint,
        "candidatePaths": inventory.candidate_paths.unwrap_or_default(),
        "changedPaths": inventory.changed_paths,
        "reusedPaths": inventory.reused_paths,
        "removedPaths": inventory.removed_paths,
        "candidateFiles": inventory.candidate_files,
        "hashedFiles": inventory.hashed_files,
        "reusedFiles": inventory.reused_files,
        "removedFiles": inventory.removed_files,
        "sourceBatch": source_batch,
        "limitation": "This manifest identifies cache-safe source candidates only. JavaScript remains authoritative for parsing and graph assembly until full compatibility parity is demonstrated."
    }))
}

pub(in crate::protocol) fn native_bounded_discovery(
    params: &Value,
) -> Result<Value, NativeProtocolError> {
    let root = project_root(params)?;
    let limits = params.get("limits").and_then(Value::as_object);
    let max_files = limits
        .and_then(|limits| limits.get("maxFiles"))
        .and_then(Value::as_u64)
        .map(|value| {
            usize::try_from(value).map_err(|_| NativeProtocolError {
                code: "invalid-params",
                message: "limits.maxFiles exceeds this platform's address space.".to_string(),
            })
        })
        .transpose()?;
    let max_bytes = limits
        .and_then(|limits| limits.get("maxBytes"))
        .and_then(Value::as_i64);
    let budget_ms = limits
        .and_then(|limits| limits.get("budgetMs"))
        .and_then(Value::as_u64);
    let package_path = params.get("packagePath").and_then(Value::as_str);
    let discovery =
        discover_native_bounded_project(&root, package_path, max_files, max_bytes, budget_ms)
            .map_err(|message| NativeProtocolError {
                code: if message.starts_with("native-bounded-") {
                    "native-bounded-discovery-failed"
                } else {
                    "native-bounded-discovery-error"
                },
                message,
            })?;
    Ok(json!({
        "schemaVersion":"flopeek-native-bounded-discovery/v1",
        "projectRoot":discovery.project_root,
        "packagePath":discovery.package_path,
        "scopeSource":discovery.scope_source,
        "planFingerprint":discovery.plan_fingerprint,
        "candidateFiles":discovery.candidates.len(),
        "candidateBytes":discovery.total_bytes,
        "candidates":discovery.candidates.into_iter().map(|candidate| json!({
            "path":candidate.path,
            "sizeBytes":candidate.size_bytes,
            "modifiedAtNs":candidate.modified_at_ns.to_string(),
            "sourceScope":candidate.source_scope,
        })).collect::<Vec<_>>(),
        "promotion":"not-started",
        "limitation":"This is native-owned bounded discovery and limit validation. Execution, mutation verification, and graph promotion are intentionally separate so an incomplete plan cannot become a graph."
    }))
}

pub(in crate::protocol) fn refresh_native_project(
    session: &mut NativeProtocolSession,
    params: &Value,
) -> Result<Value, NativeProtocolError> {
    let root = project_root(params)?;
    let session_project_id = params
        .get("sessionProjectId")
        .and_then(Value::as_str)
        .ok_or_else(|| NativeProtocolError {
            code: "invalid-params",
            message: "refreshNativeProject requires sessionProjectId.".to_string(),
        })?;
    let limits = params.get("limits").and_then(Value::as_object);
    let max_files = limits
        .and_then(|limits| limits.get("maxFiles"))
        .and_then(Value::as_u64)
        .map(|value| {
            usize::try_from(value).map_err(|_| NativeProtocolError {
                code: "invalid-params",
                message: "limits.maxFiles exceeds this platform's address space.".to_string(),
            })
        })
        .transpose()?;
    let max_bytes = limits
        .and_then(|limits| limits.get("maxBytes"))
        .and_then(Value::as_i64);
    let budget_ms = limits
        .and_then(|limits| limits.get("budgetMs"))
        .and_then(Value::as_u64);
    let package_path = params.get("packagePath").and_then(Value::as_str);
    let (status, discovery) = scan_native_js_facts_ephemeral_bounded(
        &root,
        Some(session_project_id),
        package_path,
        max_files,
        max_bytes,
        budget_ms,
    )
    .map_err(|message| NativeProtocolError {
        code: "native-bounded-execution-failed",
        message,
    })?;
    let supported_paths = status
        .facts
        .keys()
        .chain(status.compacted_facts.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let unsupported_paths = status
        .candidate_paths
        .iter()
        .filter(|path| !supported_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported_paths.is_empty() {
        return Err(NativeProtocolError {
            code: "native-source-adapter-unavailable",
            message: format!(
                "Rust bounded source authority has no promoted adapter for: {}.",
                unsupported_paths.join(", ")
            ),
        });
    }
    // A second native discovery before assembly is deliberately mandatory.
    // A changed plan is discarded rather than becoming a plausible partial
    // graph. This costs one metadata walk, but has no source-body transport.
    let verified =
        discover_native_bounded_project(&root, package_path, max_files, max_bytes, budget_ms)
            .map_err(|message| NativeProtocolError {
                code: "native-bounded-verification-failed",
                message,
            })?;
    if verified.plan_fingerprint != discovery.plan_fingerprint {
        return Err(NativeProtocolError {
            code: "native-bounded-plan-changed",
            message: "Repository source plan changed during native bounded execution; the graph was discarded."
                .to_string(),
        });
    }
    let batch = native_js_batch_envelope_for_package(&status, discovery.package_path.as_deref())?;
    let mut result = refresh_native_session_graph(session, &batch)?;
    result["batch"] = batch;
    result["sourceAuthority"] = json!("rust-native-bounded/v1");
    result["boundedDiscovery"] = json!({
        "schemaVersion":"flopeek-native-bounded-discovery/v1",
        "projectRoot":discovery.project_root,
        "packagePath":discovery.package_path,
        "scopeSource":discovery.scope_source,
        "planFingerprint":discovery.plan_fingerprint,
        "candidateFiles":discovery.candidates.len(),
        "candidateBytes":discovery.total_bytes,
        "verified":true,
        "promotion":"session-memory-only",
    });
    Ok(result)
}

pub(in crate::protocol) fn native_js_record_cache(
    params: &Value,
) -> Result<Value, NativeProtocolError> {
    let root = project_root(params)?;
    let request = params
        .get("cacheRequest")
        .ok_or_else(|| NativeProtocolError {
            code: "invalid-params",
            message: "nativeJsRecordCache requires params.cacheRequest.".to_string(),
        })?;
    handle_native_js_record_cache_value(&root, request).map_err(|message| NativeProtocolError {
        code: "native-js-record-cache-failed",
        message,
    })
}

pub(in crate::protocol) fn native_project_identity_value(identity: &ProjectIdentity) -> Value {
    let mut value = json!({
        "projectId": identity.project_id,
        "source": identity.source,
        "status": identity.status,
        "originRemote": identity.origin_remote,
        "limitation": identity.limitation,
    });
    if let Some(canonical_project_id) = &identity.canonical_project_id {
        value["canonicalProjectId"] = Value::String(canonical_project_id.clone());
    }
    value
}

/// Load or incrementally refresh the Rust-owned JS/TS source session without
/// materialising the diagnostic JSON protocol payload. Persistent graph
/// promotion consumes this directly; only the compatibility/debug protocol
/// method below serializes complete facts, resolution, and records.
pub(in crate::protocol) fn load_native_js_facts_status(
    session: &mut NativeProtocolSession,
    params: &Value,
) -> Result<crate::js_facts::NativeJsFactsStatus, NativeProtocolError> {
    let root = project_root(params)?;
    let ephemeral = params
        .get("ephemeral")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let session_project_id = params.get("sessionProjectId").and_then(Value::as_str);
    let session_key = root.display().to_string();
    let changed_paths = params
        .get("changedPaths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
    // Move the process-local source cache into the refresh. Keeping the old
    // status in the map while cloning a complete replacement doubled facts,
    // resolution, and structural records across the promotion peak. SQLite is
    // still authoritative; a failed refresh deliberately leaves this derived
    // cache empty so the next request reconciles from durable state.
    let previous = (!ephemeral)
        .then(|| session.persistent_sources.remove(&session_key))
        .flatten();
    let status = (if ephemeral {
        scan_native_js_facts_ephemeral(&root, session_project_id)
    } else if let (Some(mut previous), Some(paths)) = (previous, changed_paths.as_deref()) {
        if paths.is_empty() {
            Ok(reuse_native_js_facts_session_owned(previous))
        } else if previous.facts.is_empty()
            && previous.compacted_facts.is_empty()
            && !previous.candidate_paths.is_empty()
        {
            let promoted_facts_digest = previous.promoted_facts_digest.take();
            let previous_record_digests = std::mem::take(&mut previous.structural_record_digests);
            scan_native_js_facts(&root).map(|mut reconciled| {
                reconciled.initial_scan = false;
                reconciled.changed_paths = paths.to_vec();
                let next_record_digests =
                    native_structural_record_digests(&reconciled.structural_records);
                reconciled.changed_record_paths = next_record_digests
                    .iter()
                    .filter(|(path, digest)| previous_record_digests.get(*path) != Some(*digest))
                    .map(|(path, _)| path.clone())
                    .chain(
                        previous_record_digests
                            .keys()
                            .filter(|path| !next_record_digests.contains_key(*path))
                            .cloned(),
                    )
                    .collect();
                reconciled.structural_record_digests = next_record_digests;
                reconciled.promoted_facts_digest = promoted_facts_digest;
                reconciled
            })
        } else {
            hydrate_native_js_source_facts_for_changed_paths(&mut previous, paths)
                .and_then(|()| refresh_native_js_facts_session_owned(previous, paths))
        }
    } else {
        scan_native_js_facts(&root)
    })
    .map_err(|message| NativeProtocolError {
        code: if message.starts_with("native-session-reconcile-required:") {
            "native-session-reconcile-required"
        } else {
            "native-source-facts-failed"
        },
        message,
    })?;
    Ok(status)
}

pub(in crate::protocol) fn native_js_structural_facts(
    session: &mut NativeProtocolSession,
    params: &Value,
) -> Result<Value, NativeProtocolError> {
    let ephemeral = params
        .get("ephemeral")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status = load_native_js_facts_status(session, params)?;
    let supported_paths = status
        .facts
        .keys()
        .chain(status.compacted_facts.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let unsupported_paths = status
        .candidate_paths
        .iter()
        .filter(|path| !supported_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    let native_envelope = native_js_batch_envelope(&status)?;
    let result = json!({
        "schemaVersion": "flopeek-native-source-facts/v1",
        "adapterVersion": status.adapter_version,
        "persistence": if ephemeral { "session-memory" } else { "sqlite" },
        "projectRoot": status.project_root,
        "projectIdentity": native_project_identity_value(&status.project_identity),
        "candidateFiles": status.candidate_files,
        "candidatePaths": status.candidate_paths,
        "changedPaths": status.changed_paths,
        "reusedPaths": status.reused_paths,
        "removedPaths": status.removed_paths,
        "sourceScopeCounts": status.source_scope_counts,
        "scopeSource": status.scope_source,
        "flowEntries": {
            "primary": { "tests": status.flow_entries_tests, "fixtures": status.flow_entries_fixtures },
            "diagnostic": { "tests": true, "fixtures": true },
        },
        "parsedFiles": status.parsed_files,
        "reusedFiles": status.reused_files,
        "failedFiles": status.failed_files,
        "removedFacts": status.removed_facts,
        "unsupportedPaths": unsupported_paths,
        "facts": status.facts,
        "resolution": status.resolution,
        "records": status.structural_records,
        "entryFacts": status.entry_facts,
        "nativeEnvelope": native_envelope,
    });
    if !ephemeral {
        session
            .persistent_sources
            .insert(status.project_root.display().to_string(), status);
    }
    Ok(result)
}

// A caller that explicitly supplies an empty changed-path list is asserting
// that its watcher observed no source event.  Once this JSONL process already
// owns the matching Rust source session, do not rebuild a complete fact
// envelope merely to rediscover the same SQLite graph. SQLite remains the
// authority: its current pointer and payload are matched to the promoted
// source digest before a compact public envelope is reconstructed.
pub(in crate::protocol) fn reuse_native_persistent_project_no_op(
    session: &mut NativeProtocolSession,
    root: &Path,
    status: &NativeJsFactsStatus,
) -> Result<Option<Value>, NativeProtocolError> {
    let project_id = status.project_identity.project_id.clone();
    let Some(expected_facts_digest) = status.promoted_facts_digest.as_ref() else {
        return Ok(None);
    };
    let current = with_persistent_session_connection(session, root, |session, connection| {
        let current = current_complete_graph(connection, &project_id).map_err(|error| {
            NativeProtocolError {
                code: "store-read-failed",
                message: error.to_string(),
            }
        })?;
        let cached_snapshot = current.as_ref().and_then(|current| {
            session
                .persistent_graph
                .as_ref()
                .filter(|cached| {
                    cached.project_id == project_id && cached.graph_version == current.graph_version
                })
                .and_then(|cached| cached.public_snapshot.clone())
        });
        if cached_snapshot.is_some() {
            return Ok((current, cached_snapshot));
        }
        let snapshot = current
            .as_ref()
            .map(|current| complete_graph_payload(connection, &project_id, current.graph_version))
            .transpose()
            .map_err(|error| NativeProtocolError {
                code: "store-read-failed",
                message: error.to_string(),
            })?
            .flatten()
            .map(|stored| native_public_graph_snapshot(&stored.payload))
            .transpose()
            .map_err(|error| NativeProtocolError {
                code: "store-read-failed",
                message: error.message,
            })?;
        Ok((current, snapshot))
    })?;
    let (Some(current), Some(public_graph)) = current else {
        return Ok(None);
    };
    if current.material_fingerprint != expected_facts_digest.as_str()
        || current.public_graph_version.unwrap_or_default() < 1
    {
        return Ok(None);
    }
    let mut envelope = native_public_graph_envelope(&public_graph);
    envelope["analysis"]["refresh"] = json!({
        "strategy": "incremental-content-analysis",
        "mode": "incremental",
        "analyzedFiles": 0,
        "reusedFiles": status.candidate_paths.len(),
        "removedFiles": 0,
        "changedPaths": [],
    });
    envelope["state"]["status"] = Value::String("native-current".to_string());
    envelope["analysis"]["latestDelta"] = Value::Null;
    let public_graph_version = current
        .public_graph_version
        .expect("positive public graph version was checked");
    envelope["analysis"]["graphState"] = json!({
        "schemaVersion": "flopeek-native-graph-state/v1",
        "status": "unchanged",
        "persistence": "sqlite",
        "nativeGraphVersion": current.graph_version,
        "graphVersion": public_graph_version,
        "materialFingerprint": expected_facts_digest,
        "sourceFingerprint": envelope["state"]["sourceFingerprint"].clone(),
        "sourceRevision": envelope["state"]["sourceRevision"].clone(),
        "updatedAt": envelope["state"]["updatedAt"].clone(),
        "latestDelta": Value::Null,
        "limitation": "Native graph versions are retained in the repository-local SQLite store. They identify static graph state and do not prove runtime behavior.",
    });
    Ok(Some(json!({
        "schemaVersion": "flopeek-native-public-lifecycle/v1",
        "status": "reused",
        "nativeGraphVersion": current.graph_version,
        "publicGraphVersion": public_graph_version,
        "factsDigest": expected_facts_digest,
        "receipt": {
            "schemaVersion": "flopeek-native-source-session-no-op/v1",
            "stored": false,
            "status": "reused",
            "reason": "explicit-empty-changed-paths",
        },
        "publicGraphReuse": {
            "schemaVersion": "flopeek-native-public-graph-reuse/v1",
            "envelope": envelope,
        },
    })))
}

/// Persistent strict-Rust lifecycle. Source discovery, fact assembly, graph
/// promotion, and the SQLite-attached fact cache all remain in this process;
/// the JSONL caller receives a graph handle instead of a complete fact batch
/// that it would otherwise send straight back for persistence.
pub(in crate::protocol) fn refresh_native_persistent_project(
    session: &mut NativeProtocolSession,
    params: &Value,
) -> Result<Value, NativeProtocolError> {
    let root = project_root(params)?;
    let handle_only_public_graph = requests_handle_only_public_graph(params)?;
    // Retaining public collections is an explicit session-cache choice.
    // Handle-only callers need it for native queries; materialized CoreClient
    // callers opt in so the next changed-path refresh can reuse the snapshot
    // without changing the cold recovery timeout contract.
    let retain_public_snapshot = match params.get("retainPublicSnapshot") {
        None => handle_only_public_graph,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(NativeProtocolError {
                code: "invalid-params",
                message:
                    "refreshNativePersistentProject params.retainPublicSnapshot must be a boolean."
                        .to_string(),
            });
        }
    };
    let session_key = root.display().to_string();
    let source_refresh_started = Instant::now();
    let explicit_no_op = params
        .get("changedPaths")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
        && session.persistent_sources.contains_key(&session_key);
    // An explicit no-op first validates the lightweight session checkpoint
    // against SQLite. Its parser facts were deliberately evicted after the
    // preceding promotion, so routing it through source-adapter validation
    // would misclassify every known path as unsupported before reuse can run.
    let mut status = if explicit_no_op {
        session
            .persistent_sources
            .remove(&session_key)
            .expect("explicit no-op requires the checked session checkpoint")
    } else {
        load_native_js_facts_status(session, params)?
    };
    let source_refresh_ms = elapsed_ms(source_refresh_started);
    if explicit_no_op
        && let Some(mut response) = reuse_native_persistent_project_no_op(session, &root, &status)?
    {
        response["sourceAuthority"] = json!("rust-native-persistent/v1");
        response["sourceRefresh"] = json!({
            "mode": "no-op-session",
            "parsedFiles": 0,
            "reusedFiles": status.candidate_paths.len(),
            "changedPaths": [],
            "removedPaths": [],
        });
        response["graphHandle"] = json!({
            "schemaVersion": "flopeek-native-graph-handle/v1",
            "projectId": status.project_identity.project_id,
            "factsDigest": response["factsDigest"],
            "persistence": "sqlite",
            "publicGraphVersion": response["publicGraphVersion"],
        });
        if handle_only_public_graph {
            replace_public_graph_with_handle_envelope(&mut response)?;
        }
        session.persistent_sources.insert(session_key, status);
        return Ok(response);
    }
    if explicit_no_op {
        // SQLite moved independently or the cached lineage no longer matches.
        // Restore the checkpoint so the ordinary durable reconciliation path
        // can compare it, then acquire a complete verified status.
        session
            .persistent_sources
            .insert(session_key.clone(), status);
        status = load_native_js_facts_status(session, params)?;
    }
    let supported_paths = status
        .facts
        .keys()
        .chain(status.compacted_facts.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let unsupported_paths = status
        .candidate_paths
        .iter()
        .filter(|path| !supported_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported_paths.is_empty() {
        return Err(NativeProtocolError {
            code: "native-source-adapter-unavailable",
            message: format!(
                "Rust source authority has no promoted adapter for: {}.",
                unsupported_paths.join(", ")
            ),
        });
    }
    let git_metadata = if status.initial_scan {
        let metadata = native_js_git_metadata(&root);
        session
            .persistent_git_metadata
            .insert(session_key.clone(), metadata.clone());
        metadata
    } else if let Some(metadata) = session.persistent_git_metadata.get(&session_key) {
        metadata.clone()
    } else {
        // A source session can be restored independently from this lightweight
        // observation cache. Acquire one live baseline rather than assuming a
        // Git state that this process never observed.
        let metadata = native_js_git_metadata(&root);
        session
            .persistent_git_metadata
            .insert(session_key.clone(), metadata.clone());
        metadata
    };
    // Once a complete graph has been promoted in this JSONL session, a
    // changed-path refresh can move only its changed records.  The native
    // patch routine transfers the cached unchanged records instead of cloning
    // them into a second complete envelope. It still reconstructs and hashes
    // the exact batch before SQLite promotion, so factsDigest and public graph
    // compatibility remain byte-for-byte equivalent to a full refresh.
    let cached_base_digest = (!status.initial_scan)
        .then(|| status.promoted_facts_digest.clone())
        .flatten();
    let (mut result, facts_digest, used_fact_patch, envelope_build_ms, persistent_promotion_ms) =
        if let Some(base_digest) = cached_base_digest {
            let envelope_started = Instant::now();
            let patch = native_js_structural_fact_patch(&status, &base_digest, &git_metadata)?;
            let envelope_build_ms = elapsed_ms(envelope_started);
            let promotion_started = Instant::now();
            // `patch` now owns the changed records needed by promotion. Drop
            // the parser/source cache before graph assembly so one-file
            // refresh does not retain two copies of those facts at peak. A
            // concurrent SQLite advance is rare and reacquires a complete
            // verified source status below before attempting the full batch.
            evict_native_js_source_cache(&mut status);
            match persist_native_public_graph_patch(session, &patch) {
                Ok(result) => {
                    let facts_digest = result
                        .get("factsDigest")
                        .and_then(Value::as_str)
                        .ok_or_else(|| NativeProtocolError {
                            code: "native-source-facts-failed",
                            message: "Native fact patch promotion returned no factsDigest."
                                .to_string(),
                        })?
                        .to_string();
                    (
                        result,
                        facts_digest,
                        true,
                        envelope_build_ms,
                        elapsed_ms(promotion_started),
                    )
                }
                // A second process may have advanced SQLite after this client
                // cached its base. Rebuild once from current Rust source facts;
                // malformed internal patches stay loud rather than being hidden by
                // a full-batch retry.
                Err(error) if error.code == "structural-fact-patch-miss" => {
                    status = load_native_js_facts_status(session, params)?;
                    ensure_complete_native_js_structural_records(&mut status).map_err(
                        |message| NativeProtocolError {
                            code: "native-source-facts-failed",
                            message,
                        },
                    )?;
                    let full_envelope_started = Instant::now();
                    let mut batch = native_js_batch_envelope_with_git_owned_records(
                        &mut status,
                        &git_metadata,
                    )?;
                    batch["projectRoot"] = Value::String(root.to_string_lossy().to_string());
                    let facts_digest = batch
                    .get("factsDigest")
                    .and_then(Value::as_str)
                    .ok_or_else(|| NativeProtocolError {
                        code: "native-source-facts-failed",
                        message: "Rust source authority returned a StructuralFactBatch without factsDigest."
                            .to_string(),
                    })?
                    .to_string();
                    let changed_record_paths = status
                        .changed_record_paths
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    let full_envelope_build_ms = elapsed_ms(full_envelope_started);
                    let full_promotion_started = Instant::now();
                    evict_native_js_source_cache(&mut status);
                    let result = with_persistent_session_connection(
                        session,
                        &root,
                        |session, connection| {
                            let receipt = submit_structural_facts(&batch)?;
                            persist_native_public_graph_with_receipt_using_connection(
                                session,
                                &mut batch,
                                receipt,
                                PersistNativePublicGraphOptions {
                                    retain_persistent_facts: false,
                                    verified_topology_digest: None,
                                    changed_record_paths_override: Some(changed_record_paths),
                                    // The normal compatibility lifecycle also retains the
                                    // committed public collections.  The next changed-path
                                    // refresh can then reuse them for its adjacent delta and
                                    // publicGraphReuse envelope instead of reconstructing every
                                    // collection from the SQLite projection.  Handle-only mode
                                    // is not the only caller that benefits from this cache: the
                                    // benchmark and product CoreClient both retain a materialized
                                    // graph across refreshes.
                                    retain_public_snapshot,
                                },
                                connection,
                            )
                        },
                    )?;
                    (
                        result,
                        facts_digest,
                        false,
                        full_envelope_build_ms,
                        elapsed_ms(full_promotion_started),
                    )
                }
                Err(error) => return Err(error),
            }
        } else {
            ensure_complete_native_js_structural_records(&mut status).map_err(|message| {
                NativeProtocolError {
                    code: "native-source-facts-failed",
                    message,
                }
            })?;
            let envelope_started = Instant::now();
            let mut batch =
                native_js_batch_envelope_with_git_owned_records(&mut status, &git_metadata)?;
            batch["projectRoot"] = Value::String(root.to_string_lossy().to_string());
            let facts_digest = batch
                .get("factsDigest")
                .and_then(Value::as_str)
                .ok_or_else(|| NativeProtocolError {
                    code: "native-source-facts-failed",
                    message:
                        "Rust source authority returned a StructuralFactBatch without factsDigest."
                            .to_string(),
                })?
                .to_string();
            let changed_record_paths = (!status.initial_scan).then(|| {
                status
                    .changed_record_paths
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
            });
            let envelope_build_ms = elapsed_ms(envelope_started);
            let promotion_started = Instant::now();
            evict_native_js_source_cache(&mut status);
            let result =
                with_persistent_session_connection(session, &root, |session, connection| {
                    let receipt = submit_structural_facts(&batch)?;
                    persist_native_public_graph_with_receipt_using_connection(
                        session,
                        &mut batch,
                        receipt,
                        PersistNativePublicGraphOptions {
                            retain_persistent_facts: false,
                            verified_topology_digest: None,
                            changed_record_paths_override: changed_record_paths,
                            retain_public_snapshot,
                        },
                        connection,
                    )
                })?;
            (
                result,
                facts_digest,
                false,
                envelope_build_ms,
                elapsed_ms(promotion_started),
            )
        };
    status.promoted_facts_digest = Some(facts_digest.clone());
    let project_id = status.project_identity.project_id.clone();
    if let Some(profile) = result
        .pointer_mut("/receipt/profile")
        .and_then(Value::as_object_mut)
    {
        profile.insert("sourceRefreshMs".to_string(), json!(source_refresh_ms));
        profile.insert("envelopeBuildMs".to_string(), json!(envelope_build_ms));
        profile.insert(
            "persistentPromotionMs".to_string(),
            json!(persistent_promotion_ms),
        );
        profile.insert("usedFactPatch".to_string(), json!(used_fact_patch));
    }
    result["sourceAuthority"] = json!("rust-native-persistent/v1");
    result["sourceRefresh"] = json!({
        "mode": if status.initial_scan { "initial" } else { "incremental" },
        "parsedFiles": status.parsed_files,
        "reusedFiles": status.reused_files,
        "changedPaths": status.changed_paths,
        "removedPaths": status.removed_paths,
    });
    result["graphHandle"] = json!({
        "schemaVersion": "flopeek-native-graph-handle/v1",
        "projectId": project_id,
        "factsDigest": facts_digest,
        "persistence": "sqlite",
        "publicGraphVersion": result["publicGraphVersion"],
    });
    if handle_only_public_graph {
        replace_public_graph_with_handle_envelope(&mut result)?;
    }
    // Keep complete source facts available until every incremental fallback
    // path has either promoted or failed. Only then evict the derived parser
    // cache; SQLite and the retained record digests remain the durable lineage.
    evict_native_js_source_cache(&mut status);
    session.persistent_sources.insert(session_key, status);
    // Retain only the committed public snapshot for the next adjacent delta.
    // SQLite remains authoritative: lifecycle code gates every reuse by project
    // and graph version, while this process-local cache avoids reconstructing
    // the previous public collections during the next refresh.
    let committed_graph_version = result.get("nativeGraphVersion").and_then(Value::as_i64);
    if let Some(cached) = session.persistent_graph.as_mut()
        && cached.project_id == project_id
        && committed_graph_version == Some(cached.graph_version)
        && cached.public_snapshot.is_some()
    {
        // The public snapshot is the only derived cache needed before the next
        // refresh. Drop the duplicate projection; ensure_persistent_payload
        // rehydrates it from the verified SQLite graph while preserving this
        // snapshot for adjacent-delta reuse.
        cached.payload = Value::Null;
    }
    Ok(result)
}

// The ephemeral path must not serialize a complete StructuralFactBatch to Node
// only to have Node send the identical payload back for session assembly or
// later query calls. Keep discovery, parsing, envelope construction, graph
// assembly, and query-batch retention inside the native JSONL process; Node
// receives only a versioned session handle for this process-local lineage.
pub(in crate::protocol) fn refresh_native_js_session_graph(
    session: &mut NativeProtocolSession,
    params: &Value,
) -> Result<Value, NativeProtocolError> {
    let root = project_root(params)?;
    let handle_only_public_graph = requests_handle_only_public_graph(params)?;
    let session_project_id = params
        .get("sessionProjectId")
        .and_then(Value::as_str)
        .ok_or_else(|| NativeProtocolError {
            code: "invalid-params",
            message: "refreshNativeJsSessionGraph requires sessionProjectId.".to_string(),
        })?;
    let session_key = format!("{}\0{}", root.display(), session_project_id);
    let changed_paths = params
        .get("changedPaths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
    let no_op = matches!(
        (
            session.ephemeral_sources.get(&session_key),
            changed_paths.as_deref()
        ),
        (Some(_), Some([]))
    );
    let status = match (
        session.ephemeral_sources.get(&session_key),
        changed_paths.as_deref(),
    ) {
        (Some(previous), Some(paths)) if !paths.is_empty() => {
            refresh_native_js_facts_session(previous, paths)
        }
        (Some(previous), Some([])) => Ok(reuse_native_js_facts_session(previous)),
        _ => scan_native_js_facts_ephemeral(&root, Some(session_project_id)),
    }
    .map_err(|message| NativeProtocolError {
        code: if message.starts_with("native-session-reconcile-required:") {
            "native-session-reconcile-required"
        } else {
            "native-source-facts-failed"
        },
        message,
    })?;
    let batch = native_js_batch_envelope(&status)?;
    let mut result = refresh_native_session_graph(session, &batch)?;
    result["sourceAuthority"] = json!("rust-native-ephemeral/v1");
    result["sourceRefresh"] = json!({
        "mode": if no_op { "no-op-session" } else if changed_paths.as_ref().is_some_and(|paths| !paths.is_empty()) { "changed-path-session" } else { "initial-or-reconciled" },
        "parsedFiles": status.parsed_files,
        "reusedFiles": status.reused_files,
        "changedPaths": status.changed_paths,
        "removedPaths": status.removed_paths,
    });
    session.ephemeral_sources.insert(session_key, status);
    if handle_only_public_graph {
        replace_public_graph_with_handle_envelope(&mut result)?;
    }
    Ok(result)
}

pub(in crate::protocol) fn native_js_source_fingerprint(records: &[Value]) -> String {
    let mut lines = records
        .iter()
        .filter_map(|record| {
            Some(format!(
                "{}\0{}\0{}",
                record.get("relativePath")?.as_str()?,
                record
                    .get("sourceScope")
                    .and_then(Value::as_str)
                    .unwrap_or("application"),
                record.get("sourceHash")?.as_str()?,
            ))
        })
        .collect::<Vec<_>>();
    lines.sort_by(|left, right| javascript_ascii_locale_cmp(left, right));
    format!("sha256:{:x}", Sha256::digest(lines.join("\n")))
}

pub(in crate::protocol) fn native_js_git_metadata(root: &Path) -> Value {
    let output = Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "status",
            "--porcelain=v2",
            "--branch",
        ])
        .output();
    let Ok(output) = output else {
        return json!({"branch":"not-a-git-repository","revision":null,"shallow":null,"dirty":null,"availability":"not-a-repository","reason":"Git metadata is unavailable because this directory is not a readable Git repository."});
    };
    if !output.status.success() {
        return json!({"branch":"not-a-git-repository","revision":null,"shallow":null,"dirty":null,"availability":"not-a-repository","reason":"Git metadata is unavailable because this directory is not a readable Git repository."});
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut branch = "detached".to_string();
    let mut revision = Value::Null;
    let mut dirty = false;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("# branch.oid ") {
            if value != "(initial)" {
                revision = Value::String(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("# branch.head ") {
            if value != "(detached)" {
                branch = value.to_string();
            }
        } else if !line.starts_with("# ") {
            dirty = true;
        }
    }
    json!({"branch":branch,"revision":revision,"shallow":Value::Null,"dirty":dirty,"availability":"available","reason":Value::Null})
}

/// The adapter contract is owned once at the repository root.  Rust embeds the
/// same bytes that the JavaScript scanner loads, so a release cannot advertise
/// divergent adapter capabilities across the two execution paths.
pub(in crate::protocol) fn adapter_registry_for_implementation(implementation: &str) -> Value {
    let mut registry: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/adapter-capabilities.json"
    )))
    .expect("shared adapter capability contract is valid JSON");
    if let Some(adapters) = registry.get_mut("adapters").and_then(Value::as_array_mut) {
        for adapter in adapters {
            let Some(object) = adapter.as_object_mut() else {
                continue;
            };
            if let Some(capability) = object.get("productCapability").cloned() {
                object.insert("capabilities".to_string(), capability);
            }
            let selected = object
                .get("implementations")
                .and_then(|value| value.get(implementation))
                .and_then(Value::as_object)
                .cloned();
            if let Some(selected) = selected {
                for field in ["parser", "availability", "requiredToolchain"] {
                    if let Some(value) = selected.get(field) {
                        object.insert(field.to_string(), value.clone());
                    }
                }
            }
        }
    }
    registry
}

pub(in crate::protocol) fn native_adapter_registry() -> Value {
    adapter_registry_for_implementation("javascript")
}

pub(in crate::protocol) fn native_execution_adapter_registry() -> Value {
    adapter_registry_for_implementation("native")
}

pub(in crate::protocol) fn native_js_batch_envelope(
    status: &crate::js_facts::NativeJsFactsStatus,
) -> Result<Value, NativeProtocolError> {
    native_js_batch_envelope_for_package(status, None)
}

pub(in crate::protocol) fn native_js_batch_envelope_with_git_owned_records(
    status: &mut crate::js_facts::NativeJsFactsStatus,
    git_metadata: &Value,
) -> Result<Value, NativeProtocolError> {
    let mut batch =
        native_js_batch_envelope_for_package_with_records(status, None, false, Some(git_metadata))?;
    let records = take_complete_native_js_structural_records(status).map_err(|message| {
        NativeProtocolError {
            code: "native-source-facts-incomplete",
            message,
        }
    })?;
    batch["records"] = Value::Array(records);
    let facts_digest = structural_facts_digest(
        batch.as_object().expect("native batch is an object"),
    )
    .map_err(|message| NativeProtocolError {
        code: "native-source-facts-failed",
        message,
    })?;
    batch["factsDigest"] = Value::String(facts_digest);
    Ok(batch)
}

pub(in crate::protocol) fn native_js_structural_fact_patch(
    status: &crate::js_facts::NativeJsFactsStatus,
    base_facts_digest: &str,
    git_metadata: &Value,
) -> Result<Value, NativeProtocolError> {
    let batch = native_js_batch_envelope_without_records(status, git_metadata)?;
    let changed_paths = status
        .changed_record_paths
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let record_manifest = if status.structural_records_complete {
        &status.structural_records
    } else {
        &status.structural_record_manifest
    };
    let manifest = record_manifest
        .iter()
        .map(|record| {
            json!({
                "relativePath": record["relativePath"],
                "sourceHash": record["sourceHash"],
                "sourceScope": record["sourceScope"],
                "recordOrder": record["recordOrder"],
            })
        })
        .collect::<Vec<_>>();
    let changed_records = status
        .structural_records
        .iter()
        .filter(|record| {
            record
                .get("relativePath")
                .and_then(Value::as_str)
                .is_some_and(|path| changed_paths.contains(path))
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(json!({
        "schemaVersion": STRUCTURAL_FACT_PATCH_SCHEMA,
        "projectId": status.project_identity.project_id,
        "baseFactsDigest": base_facts_digest,
        "projectRoot": status.project_root,
        "batch": batch,
        "manifest": manifest,
        "changedRecords": changed_records,
    }))
}

/// Assemble a graph envelope from native facts. Bounded/package scans retain
/// the repository root as their identity anchor, but use the selected package
/// manifest for package-owned metadata and commands. This prevents a parent
/// monorepo's scripts from appearing as executable entry points in a child
/// package graph.
pub(in crate::protocol) fn native_js_batch_envelope_for_package(
    status: &crate::js_facts::NativeJsFactsStatus,
    package_path: Option<&str>,
) -> Result<Value, NativeProtocolError> {
    native_js_batch_envelope_for_package_with_records(status, package_path, true, None)
}

// Compact patch envelopes carry the full non-record contract but deliberately
// omit parser records and factsDigest. The patch reconstruction routine moves
// unchanged records out of its verified cache, then computes the same digest
// as a complete StructuralFactBatch.
pub(in crate::protocol) fn native_js_batch_envelope_without_records(
    status: &crate::js_facts::NativeJsFactsStatus,
    git_metadata: &Value,
) -> Result<Value, NativeProtocolError> {
    native_js_batch_envelope_for_package_with_records(status, None, false, Some(git_metadata))
}

pub(in crate::protocol) fn native_js_batch_envelope_for_package_with_records(
    status: &crate::js_facts::NativeJsFactsStatus,
    package_path: Option<&str>,
    include_records: bool,
    git_metadata: Option<&Value>,
) -> Result<Value, NativeProtocolError> {
    if include_records && !status.structural_records_complete {
        return Err(NativeProtocolError {
            code: "native-source-facts-incomplete",
            message: "A complete StructuralFactBatch requires complete native source records."
                .to_string(),
        });
    }
    let scope = read_native_scope(&status.project_root).map_err(|message| NativeProtocolError {
        code: "native-source-facts-failed",
        message,
    })?;
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| NativeProtocolError {
            code: "native-source-facts-failed",
            message: error.to_string(),
        })?;
    let git = git_metadata
        .cloned()
        .unwrap_or_else(|| native_js_git_metadata(&status.project_root));
    let manifest_path = package_path
        .map(|path| status.project_root.join(path).join("package.json"))
        .unwrap_or_else(|| status.project_root.join("package.json"));
    let package = std::fs::read_to_string(manifest_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let project_name = package
        .as_ref()
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            status
                .project_root
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "project".to_string());
    let mut summary = json!({"scannedFiles":0,"parsedFiles":0,"parsedWithDiagnosticsFiles":0,"inventoryOnlyFiles":0,"parseFailedFiles":0});
    let mut by_language =
        BTreeMap::<String, (usize, usize, usize, usize, usize, BTreeSet<String>)>::new();
    let record_view = if status.structural_records_complete {
        &status.structural_records
    } else {
        &status.structural_record_manifest
    };
    for record in record_view {
        let language = record
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        // StructuralFactBatch intentionally keeps parser diagnostics in file
        // metadata rather than duplicating them inside every record result.
        // Coverage is envelope data, so read the canonical file projection.
        let analysis = &record["fileMetadata"]["analysis"];
        let parser = analysis
            .get("parser")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let status_name = analysis.get("status").and_then(Value::as_str).unwrap_or("");
        summary["scannedFiles"] = Value::from(summary["scannedFiles"].as_u64().unwrap_or(0) + 1);
        let item = by_language
            .entry(language)
            .or_insert((0, 0, 0, 0, 0, BTreeSet::new()));
        item.0 += 1;
        item.5.insert(parser.to_string());
        if status_name.starts_with("parsed") {
            summary["parsedFiles"] = Value::from(summary["parsedFiles"].as_u64().unwrap_or(0) + 1);
            item.1 += 1;
        }
        if status_name == "parsed-with-diagnostics" {
            summary["parsedWithDiagnosticsFiles"] =
                Value::from(summary["parsedWithDiagnosticsFiles"].as_u64().unwrap_or(0) + 1);
            item.2 += 1;
        }
        if status_name == "inventory-only" {
            summary["inventoryOnlyFiles"] =
                Value::from(summary["inventoryOnlyFiles"].as_u64().unwrap_or(0) + 1);
            item.3 += 1;
        }
        if status_name == "parse-failed" {
            summary["parseFailedFiles"] =
                Value::from(summary["parseFailedFiles"].as_u64().unwrap_or(0) + 1);
            item.4 += 1;
        }
    }
    let by_language = by_language.into_iter().map(|(language, (files, parsed, parsed_with_diagnostics, inventory_only, parse_failed, parsers))| json!({"language":language,"files":files,"parsed":parsed,"parsedWithDiagnostics":parsed_with_diagnostics,"inventoryOnly":inventory_only,"parseFailed":parse_failed,"parsers":parsers})).collect::<Vec<_>>();
    let coverage = json!({"summary":summary,"byLanguage":by_language,"interpretation":"Coverage counts syntax-tree analysis status, not runtime execution coverage or relationship precision."});
    let source_fingerprint = native_js_source_fingerprint(record_view);
    // Inventory and parser status are the source of lifecycle telemetry. Do
    // not label every persistent refresh as an initial full scan merely
    // because graph assembly receives a complete compatibility envelope.
    let refresh = json!({
        "strategy":"incremental-content-analysis",
        "mode": if status.initial_scan { "initial" } else { "incremental" },
        "analyzedFiles":status.parsed_files,
        "reusedFiles":status.reused_files,
        "removedFiles":status.removed_facts,
        "changedPaths":&status.changed_paths,
    });
    let adapter_registry = native_adapter_registry();
    let execution_adapter_registry = native_execution_adapter_registry();
    let project_identity = native_project_identity_value(&status.project_identity);
    let mut public_graph_context = json!({
        "schemaVersion":5,"generatedAt":generated_at,
        "project":{"root":status.project_root,"name":project_name,"projectId":status.project_identity.project_id,"identity":project_identity,"git":git},
        "state":{"graphVersion":0,"materialFingerprint":Value::Null,"sourceFingerprint":source_fingerprint,"sourceRevision":git["revision"],"updatedAt":generated_at,"status":"unpersisted"},
        "analysis":{"mode":"deterministic","refresh":refresh,"codeInterpretation":"AST-only for registered language adapters","unparsedPolicy":"inventory-only; no dependency or flow is inferred","coverage":coverage,"nativeBoundedPackagePath":package_path,"repositoryScope":{"schemaVersion":1,"source":scope.source,"configPath":if scope.source == "config" { Value::String(".flopeek/config.json".to_string()) } else { Value::Null },"sourceRoots":scope.source_roots,"testRoots":scope.test_roots,"fixtureRoots":scope.fixture_roots,"exclude":scope.exclude,"projectId":scope.project_id,"flowEntries":{"tests":scope.flow_entries_tests,"fixtures":scope.flow_entries_fixtures},"precedence":["excluded","fixture","test","generated","application"],"counts":{"application":status.source_scope_counts.get("application").copied().unwrap_or(0),"test":status.source_scope_counts.get("test").copied().unwrap_or(0),"fixture":status.source_scope_counts.get("fixture").copied().unwrap_or(0),"generated":status.source_scope_counts.get("generated").copied().unwrap_or(0),"excluded":status.source_scope_counts.get("excluded").copied().unwrap_or(0)}},"resolution":{"internal":["relative imports","$lib","@/","tsconfig/jsconfig baseUrl and paths","literal aliases from exported Vite/Webpack configs","safe static Vite/Webpack alias expressions (__dirname, root process.cwd(), path.resolve/join/dirname, new URL/import.meta.url, fileURLToPath(import.meta.url), and constants)","package.json imports aliases","static import/node/default/require/types package condition trees","declared npm and pnpm workspace package entries","static Yarn PnP JSON workspace package entries","Python relative and src-package imports","static Go module packages","static Rust crate/self/super modules in conventional Cargo src roots"],"limitations":["Arbitrary computed Vite/Webpack aliases, custom package conditions, unsupported pnpm YAML constructs, PHP Composer autoloading, Java framework wiring and non-local-static method dispatch, Rust custom Cargo targets and #[path] modules, Go build tags and duplicate package function names, and runtime module loading are not resolved."]},"calls":{"supported":["direct identifier calls to top-level local functions","direct identifier calls to named ES/CommonJS imports resolved inside the repository","direct identifier calls to top-level local Python functions and named ES/CommonJS imports resolved inside the repository","direct local Go function calls and aliased Go package selectors resolved inside the repository","direct local PHP function calls","direct local Rust functions and named crate/self/super imports","direct unqualified unique local static Java method calls"],"limitations":"Java instance/qualified/overloaded method dispatch, Rust macros, qualified module calls, trait dispatch, custom Cargo targets, and #[path] modules, default and namespace imports, PHP Composer/autoloaded functions, Python attribute calls, Go function values, ambiguous package functions, and unaliased package-name mismatches, dependency injection, callbacks, reflection, dynamic loading, and non-literal CommonJS requires are not resolved as call edges."},"entryPoints":status.entry_facts["entryPoints"],"adapterCapabilities":adapter_registry,"executionAdapterCapabilities":execution_adapter_registry,"capabilities":adapter_registry["adapters"]},
        "stats":{"scannedFiles":summary["scannedFiles"],"parsedFiles":summary["parsedFiles"],"inventoryOnlyFiles":summary["inventoryOnlyFiles"],"parseFailedFiles":summary["parseFailedFiles"]}
    });
    if let Some(package_path) = package_path {
        public_graph_context["analysis"]["nativeBoundedPackagePath"] =
            Value::String(package_path.to_string());
    } else {
        public_graph_context["analysis"]
            .as_object_mut()
            .expect("native graph analysis is an object")
            .remove("nativeBoundedPackagePath");
    }
    // The compatibility contract is a set of supported call categories. Keep
    // its public array stable even if adjacent source declarations are merged
    // while evolving the native envelope.
    if let Some(supported) = public_graph_context
        .pointer_mut("/analysis/calls/supported")
        .and_then(Value::as_array_mut)
    {
        supported.dedup();
        for capability in supported {
            if capability.as_str()
                == Some(
                    "direct identifier calls to top-level local Python functions and named ES/CommonJS imports resolved inside the repository",
                )
            {
                *capability = Value::String(
                    "direct identifier calls to top-level local Python functions and named Python imports resolved inside the repository".to_string(),
                );
            }
        }
    }
    let mut batch = json!({
        "schemaVersion":STRUCTURAL_FACT_BATCH_SCHEMA,"projectId":status.project_identity.project_id,"packageCommands":status.entry_facts["packageCommands"],"entryMetadata":status.entry_facts["entryMetadata"],"entryEdgeMetadata":status.entry_facts["edgeMetadata"],"manualDescriptions":native_manual_descriptions(&status.project_root, record_view),
        "flowContext":{"graphVersion":0,"sourceRevision":git["revision"]},"flowEntries":{"primary":{"tests":scope.flow_entries_tests,"fixtures":scope.flow_entries_fixtures},"diagnostic":{"tests":true,"fixtures":true}},
        "lifecycleContext":{"sourceFingerprint":source_fingerprint,"sourceRevision":git["revision"],"updatedAt":generated_at,"refresh":refresh,"coverage":coverage},"publicGraphContext":public_graph_context
    });
    if !include_records {
        return Ok(batch);
    }
    batch["records"] = Value::Array(status.structural_records.clone());
    let facts_digest = structural_facts_digest(
        batch.as_object().expect("native batch is an object"),
    )
    .map_err(|message| NativeProtocolError {
        code: "native-source-facts-failed",
        message,
    })?;
    batch["factsDigest"] = Value::String(facts_digest);
    Ok(batch)
}

pub(in crate::protocol) fn native_js_record_cache_load_raw(
    params: &Value,
) -> Result<Box<RawValue>, NativeProtocolError> {
    let root = project_root(params)?;
    let request = params
        .get("cacheRequest")
        .ok_or_else(|| NativeProtocolError {
            code: "invalid-params",
            message: "nativeJsRecordCache requires params.cacheRequest.".to_string(),
        })?;
    load_native_js_record_cache_raw(&root, request).map_err(|message| NativeProtocolError {
        code: "native-js-record-cache-failed",
        message,
    })
}

pub(in crate::protocol) fn string_field<'a>(
    value: &'a serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, NativeProtocolError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|item| !item.is_empty())
        .ok_or_else(|| NativeProtocolError {
            code: "invalid-structural-facts",
            message: format!("StructuralFactBatch/v1 requires non-empty {field}."),
        })
}

pub(in crate::protocol) fn structural_batch(params: &Value) -> Result<&Value, NativeProtocolError> {
    match params.get("batch") {
        Some(batch) if batch.is_object() => Ok(batch),
        Some(_) => Err(NativeProtocolError {
            code: "invalid-structural-facts",
            message:
                "Native structural query params.batch must be a StructuralFactBatch/v1 object."
                    .to_string(),
        }),
        None => Ok(params),
    }
}

pub(in crate::protocol) fn is_portable_repository_path(value: &str) -> bool {
    !value.is_empty()
        && !Path::new(value).is_absolute()
        && !value.contains('\\')
        && !value.split('/').any(|segment| segment == "..")
}

pub(in crate::protocol) fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit())
}

// The material fingerprint deliberately omits a small, fixed set of
// observational fields.  Serializing this borrowed view avoids cloning a
// multi-megabyte StructuralFactBatch on every incremental patch merely to
// remove those keys before hashing.  It preserves serde_json's canonical map
// order and value encoding, so the public JavaScript-compatible SHA-256
// contract is unchanged.
pub(in crate::protocol) struct StructuralFactsCanonical<'a>(&'a serde_json::Map<String, Value>);

pub(in crate::protocol) struct ObjectWithoutKeys<'a> {
    value: &'a Value,
    omitted: &'static [&'static str],
}

pub(in crate::protocol) struct CanonicalValue<'a>(pub(in crate::protocol) &'a Value);

struct RawJsonValue<'a>(&'a str);

const RAW_VALUE_TOKEN: &str = "$serde_json::private::RawValue";

impl Serialize for RawJsonValue<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut value = serializer.serialize_struct(RAW_VALUE_TOKEN, 1)?;
        value.serialize_field(RAW_VALUE_TOKEN, self.0)?;
        value.end()
    }
}

struct RecordsCanonical<'a>(&'a Value);

impl Serialize for RecordsCanonical<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Some(records) = self.0.as_array() else {
            return self.0.serialize(serializer);
        };
        let mut sequence = serializer.serialize_seq(Some(records.len()))?;
        for record in records {
            match record {
                // Compact process-local records were canonicalized before
                // entering this envelope. RawValue lets the SHA-256 stream
                // reuse those bytes without reparsing and reallocating every
                // unchanged parser fact.
                Value::String(raw) => sequence.serialize_element(&RawJsonValue(raw))?,
                value => sequence.serialize_element(&CanonicalValue(value))?,
            }
        }
        sequence.end()
    }
}

impl Serialize for CanonicalValue<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0 {
            Value::Array(values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(&CanonicalValue(value))?;
                }
                sequence.end()
            }
            Value::Object(entries) => {
                let mut keys = entries.keys().collect::<Vec<_>>();
                keys.sort_unstable();
                let mut map = serializer.serialize_map(Some(keys.len()))?;
                for key in keys {
                    map.serialize_entry(key, &CanonicalValue(&entries[key]))?;
                }
                map.end()
            }
            value => value.serialize(serializer),
        }
    }
}

impl Serialize for ObjectWithoutKeys<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Some(object) = self.value.as_object() else {
            return self.value.serialize(serializer);
        };
        let retained = object
            .keys()
            .filter(|key| !self.omitted.contains(&key.as_str()))
            .count();
        let mut map = serializer.serialize_map(Some(retained))?;
        let mut keys = object.keys().collect::<Vec<_>>();
        keys.sort_unstable();
        for key in keys {
            if !self.omitted.contains(&key.as_str()) {
                map.serialize_entry(key, &CanonicalValue(&object[key]))?;
            }
        }
        map.end()
    }
}

impl Serialize for StructuralFactsCanonical<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        const ROOT_OMITTED: [&str; 3] = ["factsDigest", "projectRoot", "publicGraphContext"];
        let retained = self
            .0
            .keys()
            .filter(|key| !ROOT_OMITTED.contains(&key.as_str()))
            .count();
        let mut map = serializer.serialize_map(Some(retained))?;
        let mut keys = self.0.keys().collect::<Vec<_>>();
        keys.sort_unstable();
        for key in keys {
            if ROOT_OMITTED.contains(&key.as_str()) {
                continue;
            }
            let value = &self.0[key];
            match key.as_str() {
                "lifecycleContext" => map.serialize_entry(
                    key,
                    &ObjectWithoutKeys {
                        value,
                        omitted: &["updatedAt", "refresh"],
                    },
                )?,
                "flowContext" => map.serialize_entry(
                    key,
                    &ObjectWithoutKeys {
                        value,
                        omitted: &["graphVersion"],
                    },
                )?,
                "records" => map.serialize_entry(key, &RecordsCanonical(value))?,
                _ => map.serialize_entry(key, &CanonicalValue(value))?,
            }
        }
        map.end()
    }
}

pub(in crate::protocol) struct StructuralTopologyCanonical<'a>(&'a serde_json::Map<String, Value>);

pub(in crate::protocol) struct RecordsWithoutSourceHashes<'a>(&'a Value);

impl Serialize for RecordsWithoutSourceHashes<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Some(records) = self.0.as_array() else {
            return self.0.serialize(serializer);
        };
        let mut sequence = serializer.serialize_seq(Some(records.len()))?;
        for record in records {
            sequence.serialize_element(&ObjectWithoutKeys {
                value: record,
                omitted: &["sourceHash"],
            })?;
        }
        sequence.end()
    }
}

impl Serialize for StructuralTopologyCanonical<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        const ROOT_OMITTED: [&str; 3] = ["factsDigest", "projectRoot", "publicGraphContext"];
        let retained = self
            .0
            .keys()
            .filter(|key| !ROOT_OMITTED.contains(&key.as_str()))
            .count();
        let mut map = serializer.serialize_map(Some(retained))?;
        let mut keys = self.0.keys().collect::<Vec<_>>();
        keys.sort_unstable();
        for key in keys {
            if ROOT_OMITTED.contains(&key.as_str()) {
                continue;
            }
            let value = &self.0[key];
            match key.as_str() {
                "lifecycleContext" => map.serialize_entry(
                    key,
                    &ObjectWithoutKeys {
                        value,
                        omitted: &[
                            "updatedAt",
                            "refresh",
                            "sourceFingerprint",
                            "sourceRevision",
                        ],
                    },
                )?,
                "flowContext" => map.serialize_entry(
                    key,
                    &ObjectWithoutKeys {
                        value,
                        omitted: &["graphVersion", "sourceRevision"],
                    },
                )?,
                "records" => map.serialize_entry(key, &RecordsWithoutSourceHashes(value))?,
                _ => map.serialize_entry(key, &CanonicalValue(value))?,
            }
        }
        map.end()
    }
}

pub(in crate::protocol) struct Sha256Writer(Sha256);

impl Write for Sha256Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::protocol) fn structural_facts_canonical_json(
    batch: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    serde_json::to_string(&StructuralFactsCanonical(batch))
        .map_err(|error| format!("Unable to canonicalize structural facts: {error}"))
}

pub(in crate::protocol) fn structural_facts_digest(
    batch: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let mut writer = Sha256Writer(Sha256::new());
    serde_json::to_writer(&mut writer, &StructuralFactsCanonical(batch))
        .map_err(|error| format!("Unable to canonicalize structural facts: {error}"))?;
    Ok(format!("sha256:{:x}", writer.0.finalize()))
}

// This is deliberately narrower than the material fingerprint: changing a
// source hash must still advance the public graph version and preserve stale
// Context Ref semantics, but it must not force a graph/flow rebuild when the
// JavaScript adapter emitted identical structural facts.
pub(in crate::protocol) fn structural_topology_digest(
    batch: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let mut writer = Sha256Writer(Sha256::new());
    serde_json::to_writer(&mut writer, &StructuralTopologyCanonical(batch))
        .map_err(|error| format!("Unable to canonicalize structural topology: {error}"))?;
    Ok(format!("sha256:{:x}", writer.0.finalize()))
}

pub(in crate::protocol) fn topology_record_value(record: &Value) -> Option<Value> {
    let mut record = record.as_object()?.clone();
    record.remove("sourceHash");
    Some(Value::Object(record))
}

pub(in crate::protocol) fn topology_envelope_value(
    batch: &serde_json::Map<String, Value>,
) -> Value {
    let mut envelope = batch.clone();
    envelope.remove("records");
    envelope.remove("factsDigest");
    envelope.remove("projectRoot");
    envelope.remove("publicGraphContext");
    if let Some(lifecycle) = envelope
        .get_mut("lifecycleContext")
        .and_then(Value::as_object_mut)
    {
        lifecycle.remove("updatedAt");
        lifecycle.remove("refresh");
        lifecycle.remove("sourceFingerprint");
        lifecycle.remove("sourceRevision");
    }
    if let Some(flow_context) = envelope
        .get_mut("flowContext")
        .and_then(Value::as_object_mut)
    {
        flow_context.remove("graphVersion");
        flow_context.remove("sourceRevision");
    }
    Value::Object(envelope)
}

pub(in crate::protocol) fn record_has_cross_file_or_global_facts(record: &Value) -> bool {
    let Some(result) = record.get("result").and_then(Value::as_object) else {
        return true;
    };
    if [
        "resolvedImports",
        "resolvedPackages",
        "externalImports",
        "endpoints",
        "frameworkCommands",
        "schedules",
        "requests",
    ]
    .iter()
    .any(|field| {
        result
            .get(*field)
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    }) {
        return true;
    }
    result
        .get("calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|call| call.get("imported").is_some_and(Value::is_object))
}

pub(in crate::protocol) fn record_references_path(record: &Value, path: &str) -> bool {
    let Some(result) = record.get("result").and_then(Value::as_object) else {
        return true;
    };
    result
        .get("resolvedImports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|item| item.get("targetPath").and_then(Value::as_str) == Some(path))
        || result
            .get("resolvedPackages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|item| {
                item.get("files")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|file| file.as_str() == Some(path))
            })
}

// Return a record path only when rebuilding that one record cannot alter any
// cross-file or global contribution. This intentionally abstains for the vast
// majority of edits; correctness and public ordering win over cache breadth.
pub(in crate::protocol) fn isolated_structural_change_path(
    previous: &serde_json::Map<String, Value>,
    current: &serde_json::Map<String, Value>,
) -> Option<String> {
    if topology_envelope_value(previous) != topology_envelope_value(current) {
        return None;
    }
    let previous_records = previous.get("records")?.as_array()?;
    let current_records = current.get("records")?.as_array()?;
    if previous_records.len() != current_records.len() {
        return None;
    }
    let previous_by_path = previous_records
        .iter()
        .filter_map(|record| {
            record
                .get("relativePath")
                .and_then(Value::as_str)
                .map(|path| (path, record))
        })
        .collect::<BTreeMap<_, _>>();
    let current_by_path = current_records
        .iter()
        .filter_map(|record| {
            record
                .get("relativePath")
                .and_then(Value::as_str)
                .map(|path| (path, record))
        })
        .collect::<BTreeMap<_, _>>();
    if previous_by_path.len() != previous_records.len()
        || current_by_path.len() != current_records.len()
        || previous_by_path.keys().ne(current_by_path.keys())
    {
        return None;
    }
    let changed = current_by_path
        .iter()
        .filter_map(|(path, current_record)| {
            (topology_record_value(previous_by_path[*path])
                != topology_record_value(current_record))
            .then_some(*path)
        })
        .collect::<Vec<_>>();
    let [path] = changed.as_slice() else {
        return None;
    };
    let previous_record = previous_by_path[*path];
    let current_record = current_by_path[*path];
    if record_has_cross_file_or_global_facts(previous_record)
        || record_has_cross_file_or_global_facts(current_record)
    {
        return None;
    }
    if current_by_path
        .iter()
        .filter(|(other_path, _)| **other_path != *path)
        .any(|(_, record)| record_references_path(record, path))
    {
        return None;
    }
    current
        .get("packageCommands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .all(|command| command.get("targetPath").and_then(Value::as_str) != Some(path))
        .then_some((*path).to_string())
}

pub(in crate::protocol) fn build_isolated_incremental_graph(
    batch: &Value,
    previous_projection: &Value,
    changed_path: &str,
) -> Result<StructuralGraphProjection, NativeProtocolError> {
    let batch_object = batch.as_object().ok_or_else(|| NativeProtocolError {
        code: "invalid-structural-facts",
        message: "StructuralFactBatch/v1 must be an object.".to_string(),
    })?;
    let changed_record = batch_object
        .get("records")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|record| record.get("relativePath").and_then(Value::as_str) == Some(changed_path))
        .cloned()
        .ok_or_else(|| NativeProtocolError {
            code: "invalid-structural-facts",
            message: "Incremental structural record is missing from the current fact batch."
                .to_string(),
        })?;
    let mut isolated = batch_object.clone();
    isolated.insert("records".to_string(), Value::Array(vec![changed_record]));
    isolated.insert("packageCommands".to_string(), Value::Array(Vec::new()));
    let changed_graph = build_structural_graph(&Value::Object(isolated)).map_err(|message| {
        NativeProtocolError {
            code: "structural-graph-failed",
            message,
        }
    })?;
    let previous =
        structural_graph_snapshot(previous_projection).map_err(|message| NativeProtocolError {
            code: "store-read-failed",
            message,
        })?;
    let removed_ids = previous
        .nodes
        .iter()
        .filter(|node| node.path.as_deref() == Some(changed_path))
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut nodes = previous
        .nodes
        .into_iter()
        .filter(|node| !removed_ids.contains(&node.id))
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    for node in changed_graph.nodes {
        nodes.insert(node.id.clone(), node);
    }
    let mut edges = previous
        .edges
        .into_iter()
        .filter(|edge| !removed_ids.contains(&edge.source) && !removed_ids.contains(&edge.target))
        .map(|edge| {
            (
                (
                    edge.edge_type.clone(),
                    edge.source.clone(),
                    edge.target.clone(),
                ),
                edge,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for edge in changed_graph.edges {
        edges.insert(
            (
                edge.edge_type.clone(),
                edge.source.clone(),
                edge.target.clone(),
            ),
            edge,
        );
    }
    structural_graph_projection_from_parts(
        nodes.into_values().collect(),
        edges.into_values().collect(),
    )
    .map_err(|message| NativeProtocolError {
        code: "structural-graph-serialize-failed",
        message,
    })
}

pub(in crate::protocol) fn projection_digest(
    projection: &Value,
) -> Result<String, NativeProtocolError> {
    // SQLite stores public collections separately from their envelope and
    // reconstructs an equivalent JSON object on read. Object insertion order
    // is therefore not durable identity. Hash the same recursively sorted
    // representation used by JavaScript stable JSON so storage layout cannot
    // turn a valid graph into a false corruption report.
    let mut writer = Sha256Writer(Sha256::new());
    serde_json::to_writer(&mut writer, &CanonicalValue(projection)).map_err(|error| {
        NativeProtocolError {
            code: "structural-graph-serialize-failed",
            message: error.to_string(),
        }
    })?;
    Ok(format!("sha256:{:x}", writer.0.finalize()))
}

pub(in crate::protocol) fn submit_structural_facts_with_verified_digest(
    params: &Value,
    verified_digest: Option<&str>,
) -> Result<Value, NativeProtocolError> {
    let batch_value = structural_batch(params)?;
    let Some(batch) = batch_value.as_object() else {
        return Err(NativeProtocolError {
            code: "invalid-structural-facts",
            message: "submitStructuralFacts params must be a StructuralFactBatch/v1 object."
                .to_string(),
        });
    };
    if batch.get("schemaVersion").and_then(Value::as_str) != Some(STRUCTURAL_FACT_BATCH_SCHEMA) {
        return Err(NativeProtocolError {
            code: "unsupported-structural-facts",
            message: format!("Structural facts must use {STRUCTURAL_FACT_BATCH_SCHEMA}."),
        });
    }
    let project_id = string_field(batch, "projectId")?;
    let facts_digest = string_field(batch, "factsDigest")?;
    if !facts_digest.starts_with("sha256:") || !is_sha256_hex(&facts_digest[7..]) {
        return Err(NativeProtocolError {
            code: "invalid-structural-facts",
            message: "StructuralFactBatch/v1 factsDigest must be a SHA-256 digest.".to_string(),
        });
    }
    // Patch reconstruction has already canonicalized this exact batch and
    // checked the caller's optional expected digest. Reuse that internal
    // proof, while ordinary protocol requests continue to hash independently.
    let expected_facts_digest = match verified_digest {
        Some(digest) => digest.to_string(),
        None => structural_facts_digest(batch).map_err(|message| NativeProtocolError {
            code: "invalid-structural-facts",
            message,
        })?,
    };
    if facts_digest != expected_facts_digest {
        return Err(NativeProtocolError {
            code: "invalid-structural-facts",
            message: "StructuralFactBatch/v1 factsDigest does not match its canonical payload."
                .to_string(),
        });
    }
    let Some(records) = batch.get("records").and_then(Value::as_array) else {
        return Err(NativeProtocolError {
            code: "invalid-structural-facts",
            message: "StructuralFactBatch/v1 requires records.".to_string(),
        });
    };
    if records.len() > 100_000 {
        return Err(NativeProtocolError {
            code: "unsafe-structural-facts",
            message: "StructuralFactBatch/v1 must remain bounded.".to_string(),
        });
    }
    // With a verified digest, unchanged records came from the already
    // validated SQLite cache and changed records were validated while the
    // patch was reconstructed. Revalidating the complete multi-megabyte batch
    // here added latency without adding an independent trust boundary.
    if verified_digest.is_none() {
        validate_structural_records(records).map_err(|message| NativeProtocolError {
            code: "unsafe-structural-facts",
            message,
        })?;
    }
    if verified_digest.is_none() {
        let mut record_orders = std::collections::BTreeSet::new();
        for record in records {
            let Some(record) = record.as_object() else {
                return Err(NativeProtocolError {
                    code: "invalid-structural-facts",
                    message: "StructuralFactBatch/v1 records must be objects.".to_string(),
                });
            };
            let relative_path = string_field(record, "relativePath")?;
            if !is_portable_repository_path(relative_path) {
                return Err(NativeProtocolError {
                code: "invalid-structural-facts",
                message:
                    "StructuralFactBatch/v1 record paths must be portable and repository-relative."
                        .to_string(),
            });
            }
            let record_order = record
                .get("recordOrder")
                .and_then(Value::as_u64)
                .ok_or_else(|| NativeProtocolError {
                    code: "invalid-structural-facts",
                    message: "StructuralFactBatch/v1 recordOrder must be a non-negative integer."
                        .to_string(),
                })?;
            if !record_orders.insert(record_order) {
                return Err(NativeProtocolError {
                    code: "invalid-structural-facts",
                    message: "StructuralFactBatch/v1 recordOrder values must be unique."
                        .to_string(),
                });
            }
            let source_hash = string_field(record, "sourceHash")?;
            if !is_sha256_hex(source_hash) {
                return Err(NativeProtocolError {
                    code: "invalid-structural-facts",
                    message:
                        "StructuralFactBatch/v1 record sourceHash must be a SHA-256 hex digest."
                            .to_string(),
                });
            }
            if !record.contains_key("result") {
                return Err(NativeProtocolError {
                    code: "invalid-structural-facts",
                    message: "StructuralFactBatch/v1 records require result facts.".to_string(),
                });
            }
            if let Some(resolved_imports) = record
                .get("result")
                .and_then(Value::as_object)
                .and_then(|result| result.get("resolvedImports"))
            {
                let Some(resolved_imports) = resolved_imports.as_array() else {
                    return Err(NativeProtocolError {
                        code: "invalid-structural-facts",
                        message: "StructuralFactBatch/v1 resolvedImports must be an array."
                            .to_string(),
                    });
                };
                for resolved_import in resolved_imports {
                    let Some(resolved_import) = resolved_import.as_object() else {
                        return Err(NativeProtocolError {
                            code: "invalid-structural-facts",
                            message: "StructuralFactBatch/v1 resolvedImports must contain objects."
                                .to_string(),
                        });
                    };
                    string_field(resolved_import, "specifier")?;
                    let target_path = string_field(resolved_import, "targetPath")?;
                    if !is_portable_repository_path(target_path) {
                        return Err(NativeProtocolError {
                        code: "invalid-structural-facts",
                        message: "StructuralFactBatch/v1 resolved import target paths must be portable and repository-relative.".to_string(),
                    });
                    }
                }
            }
        }
        if !record_orders.iter().copied().eq(0..records.len() as u64) {
            return Err(NativeProtocolError {
                code: "invalid-structural-facts",
                message: "StructuralFactBatch/v1 recordOrder values must be contiguous from zero."
                    .to_string(),
            });
        }
    }
    Ok(json!({
        "schemaVersion": STRUCTURAL_FACT_BATCH_SCHEMA,
        "projectId": project_id,
        "acceptedRecords": records.len(),
        "factsDigest": facts_digest,
        "stored": false,
        "limitation": "Structural facts are validated transport input only. JavaScript remains authoritative for graph assembly and public output.",
    }))
}

pub(in crate::protocol) fn submit_structural_facts(
    params: &Value,
) -> Result<Value, NativeProtocolError> {
    submit_structural_facts_with_verified_digest(params, None)
}
