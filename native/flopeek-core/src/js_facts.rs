use crate::inventory::{
    MAX_NATIVE_SOURCE_FILE_BYTES, NativeBoundedDiscovery, discover_native_bounded_project,
    scan_native_ephemeral_inventory_with_paths, scan_native_inventory_with_paths,
};
use crate::js_batch::{
    build_native_js_entry_facts, build_native_js_entry_facts_for_manifests,
    build_native_js_structural_records, build_native_js_structural_records_with_source_hashes,
    native_public_source_hash, normalize_structural_record_orders,
};
use crate::js_resolver::{NativeJsResolutionFacts, resolve_native_js_imports};
use crate::project_identity::{ProjectIdentity, resolve_ephemeral_project_identity};
use crate::scope::read_native_scope;
use crate::source_text::read_source_text;
use crate::store::open_native_store;
use icu_collator::{Collator, CollatorBorrowed};
use icu_locale_core::locale;
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tree_sitter::{Language, Node, Parser};

pub const NATIVE_JS_FACTS_SCHEMA: &str = "flopeek-native-js-facts/v2";
// This must advance when a cached fact's observable structural semantics change.
pub const NATIVE_JS_ADAPTER_VERSION: &str = "native-tree-sitter-source/v19";

fn with_native_parser_pool<Operation, Output>(operation: Operation) -> Result<Output, String>
where
    Operation: FnOnce() -> Output + Send,
    Output: Send,
{
    // Parser work owns its syntax tree and source buffer on the heap and does
    // not require Rayon's multi-megabyte default worker stacks. Use a bounded,
    // operation-local pool so parser threads and their stacks are gone before
    // the long-lived JSONL session reaches its steady state.
    let pool = ThreadPoolBuilder::new()
        .stack_size(512 * 1024)
        .build()
        .map_err(|error| format!("Unable to create native parser worker pool: {error}"))?;
    Ok(pool.install(operation))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsFacts {
    pub schema_version: String,
    pub parser: String,
    pub status: String,
    pub diagnostics: usize,
    pub imports: Vec<String>,
    pub symbols: Vec<NativeJsSymbol>,
    pub direct_calls: Vec<String>,
    /// Compatibility-shaped parser output produced entirely by Rust. The
    /// legacy summary fields above remain diagnostic only; promotion is based
    /// on this ordered, evidence-bearing projection.
    pub structural: NativeJsStructuralFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeJsStructuralFacts {
    pub imports: Vec<NativeJsImport>,
    pub symbols: Vec<NativeJsStructuralSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub canonical_symbols: Vec<NativeJsStructuralSymbol>,
    pub calls: Vec<NativeJsCall>,
    pub endpoints: Vec<NativeJsEndpoint>,
    pub requests: Vec<NativeJsRequest>,
    pub integrations: Vec<serde_json::Value>,
    pub framework_commands: Vec<serde_json::Value>,
    pub unsupported_framework_commands: Vec<serde_json::Value>,
    pub runtime_actions: Vec<serde_json::Value>,
    pub schedules: Vec<NativeJsSchedule>,
    pub unsupported_schedules: Vec<NativeJsUnsupportedSchedule>,
    pub methods: Vec<String>,
    pub analysis: NativeJsAnalysis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsPosition {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsRange {
    pub start: NativeJsPosition,
    pub end: NativeJsPosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsEvidence {
    pub parser: String,
    pub file: String,
    pub range: NativeJsRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsImport {
    pub specifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard: Option<bool>,
    pub evidence: NativeJsEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsStructuralSymbol {
    #[serde(rename = "type")]
    pub symbol_type: String,
    pub name: String,
    pub methods: Vec<String>,
    pub evidence: NativeJsEvidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<NativeJsCanonicalSymbolIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeJsCanonicalSymbolIdentity {
    pub qualified_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lexical_owner: Option<NativeJsSymbolReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub discriminator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsSymbolReference {
    #[serde(rename = "type")]
    pub symbol_type: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeJsImportedReference {
    pub specifier: String,
    pub exported_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsCall {
    pub name: String,
    pub source: Option<NativeJsSymbolReference>,
    pub imported: Option<NativeJsImportedReference>,
    pub evidence: NativeJsEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeJsEndpoint {
    pub method: String,
    pub route: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handler_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handler_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_responsibility: Option<String>,
    pub evidence: NativeJsEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsRequest {
    pub method: String,
    pub route: String,
    pub evidence: NativeJsEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeJsSchedule {
    pub expression: String,
    pub task_name: String,
    pub evidence: NativeJsEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsUnsupportedSchedule {
    pub path: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<NativeJsEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsAnalysis {
    pub parser: String,
    pub status: String,
    pub confidence: String,
    pub diagnostics: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeJsSymbol {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeJsFactsStatus {
    pub project_root: PathBuf,
    pub project_identity: ProjectIdentity,
    /// True only for a full cold/reconciled source acquisition. This is kept
    /// explicit because an initial inventory may internally list every file as
    /// changed, which must never leak as incremental public telemetry.
    pub initial_scan: bool,
    pub adapter_version: String,
    pub parsed_files: usize,
    pub reused_files: usize,
    pub failed_files: usize,
    pub removed_facts: usize,
    pub candidate_files: usize,
    pub candidate_paths: Vec<String>,
    pub changed_paths: Vec<String>,
    pub reused_paths: Vec<String>,
    pub removed_paths: Vec<String>,
    // Includes direct source events plus importers whose resolved-record
    // payload changed. This is intentionally separate from public refresh
    // telemetry so SQLite can selectively write every affected record without
    // overstating the user's changed-path event.
    pub changed_record_paths: Vec<String>,
    pub source_scope_counts: BTreeMap<String, usize>,
    pub scope_source: String,
    pub flow_entries_tests: bool,
    pub flow_entries_fixtures: bool,
    /// Digest of the last batch atomically promoted for this source session.
    /// The complete batch remains authoritative in SQLite and is loaded only
    /// while reconstructing a verified incremental patch.
    pub promoted_facts_digest: Option<String>,
    /// Compact equality checkpoint for durable source-cache reconciliation.
    pub structural_record_digests: BTreeMap<String, String>,
    pub facts: BTreeMap<String, NativeJsFacts>,
    /// Compact derived cache used between persistent refreshes. SQLite remains
    /// authoritative for promoted graph state.
    pub compacted_facts: BTreeMap<String, String>,
    /// Public SHA-256 hashes already read during this process. Unlike the
    /// inventory's durable BLAKE3 detector, these are session-local graph
    /// contract values and avoid reopening unchanged sources on refresh.
    pub source_hashes: BTreeMap<String, String>,
    pub resolution: BTreeMap<String, NativeJsResolutionFacts>,
    /// Session-local reverse import index.  It lets an ordinary changed-path
    /// refresh locate direct resolution dependents without walking every
    /// cached resolver result.  It is derived from `resolution`, never
    /// persisted, and is rebuilt conservatively whenever membership changes.
    pub reverse_importers: BTreeMap<String, BTreeSet<String>>,
    /// The inverse membership is retained so replacing one importer's
    /// resolution can remove its old reverse edges without scanning the whole
    /// reverse index.
    pub importer_targets: BTreeMap<String, BTreeSet<String>>,
    pub structural_records: Vec<serde_json::Value>,
    /// Persistent sessions compact this to changed records after a complete
    /// batch is retained separately. Ephemeral and freshly reconciled statuses
    /// keep the complete ordered record set.
    pub structural_records_complete: bool,
    /// Compact headers and file metadata retained when complete record results
    /// move to the verified persistent fact cache.
    pub structural_record_manifest: Vec<serde_json::Value>,
    pub entry_facts: serde_json::Value,
}

fn native_resolution_indexes(
    resolution: &BTreeMap<String, NativeJsResolutionFacts>,
) -> (
    BTreeMap<String, BTreeSet<String>>,
    BTreeMap<String, BTreeSet<String>>,
) {
    let mut reverse_importers = BTreeMap::<String, BTreeSet<String>>::new();
    let mut importer_targets = BTreeMap::<String, BTreeSet<String>>::new();
    for (importer, facts) in resolution {
        let mut targets = facts
            .resolved_imports
            .iter()
            .map(|item| item.target_path.clone())
            .collect::<BTreeSet<_>>();
        targets.extend(
            facts
                .resolved_packages
                .iter()
                .flat_map(|package| package.files.iter().cloned()),
        );
        for target in &targets {
            reverse_importers
                .entry(target.clone())
                .or_default()
                .insert(importer.clone());
        }
        if !targets.is_empty() {
            importer_targets.insert(importer.clone(), targets);
        }
    }
    (reverse_importers, importer_targets)
}

fn update_native_resolution_indexes(
    status: &mut NativeJsFactsStatus,
    refreshed_paths: &BTreeSet<String>,
    rebuild: bool,
) {
    if rebuild {
        (status.reverse_importers, status.importer_targets) =
            native_resolution_indexes(&status.resolution);
        return;
    }
    for importer in refreshed_paths {
        let Some(old_targets) = status.importer_targets.remove(importer) else {
            continue;
        };
        let mut empty_targets = Vec::new();
        for target in old_targets {
            if let Some(importers) = status.reverse_importers.get_mut(&target) {
                importers.remove(importer);
                if importers.is_empty() {
                    empty_targets.push(target);
                }
            }
        }
        for target in empty_targets {
            status.reverse_importers.remove(&target);
        }
    }
    for importer in refreshed_paths {
        let mut targets = status
            .resolution
            .get(importer)
            .map(|facts| {
                facts
                    .resolved_imports
                    .iter()
                    .map(|item| item.target_path.clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        if let Some(facts) = status.resolution.get(importer) {
            targets.extend(
                facts
                    .resolved_packages
                    .iter()
                    .flat_map(|package| package.files.iter().cloned()),
            );
        }
        for target in &targets {
            status
                .reverse_importers
                .entry(target.clone())
                .or_default()
                .insert(importer.clone());
        }
        if !targets.is_empty() {
            status.importer_targets.insert(importer.clone(), targets);
        }
    }
}

/// Return a truthful lifecycle projection for an explicit no-op event. The
/// complete parser/graph facts remain session-owned, but a caller observing
/// refresh telemetry must not see cold-scan parse counts after `changedPaths`
/// explicitly declared that nothing changed.
pub fn reuse_native_js_facts_session(previous: &NativeJsFactsStatus) -> NativeJsFactsStatus {
    reuse_native_js_facts_session_owned(previous.clone())
}

pub fn reuse_native_js_facts_session_owned(mut next: NativeJsFactsStatus) -> NativeJsFactsStatus {
    next.initial_scan = false;
    next.parsed_files = 0;
    next.reused_files = next.candidate_paths.len();
    next.removed_facts = 0;
    next.changed_paths.clear();
    next.reused_paths = next.candidate_paths.clone();
    next.removed_paths.clear();
    next.changed_record_paths.clear();
    next
}

pub fn hydrate_native_js_source_facts(status: &mut NativeJsFactsStatus) -> Result<(), String> {
    if status.facts.len() == status.candidate_paths.len() {
        return Ok(());
    }
    for (path, payload) in std::mem::take(&mut status.compacted_facts) {
        let fact = serde_json::from_str(&payload)
            .map_err(|error| format!("Invalid compacted native parser fact for {path}: {error}"))?;
        status.facts.insert(path, fact);
    }
    let missing = status
        .candidate_paths
        .iter()
        .filter(|path| !status.facts.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "Persistent source cache is missing facts for: {}.",
            missing.join(", ")
        ));
    }
    Ok(())
}

/// Hydrate only the source facts that can participate in an ordinary
/// changed-file refresh.  Persistent sessions keep the rest as compact JSON
/// strings after promotion; deserializing every unchanged file on each edit
/// turns the incremental path back into an O(repository) operation.
///
/// Membership/configuration events are deliberately conservative and hydrate
/// the complete cache because they can invalidate every resolver record.
pub fn hydrate_native_js_source_facts_for_changed_paths(
    status: &mut NativeJsFactsStatus,
    changed_paths: &[String],
) -> Result<(), String> {
    if status.facts.len() == status.candidate_paths.len() {
        return Ok(());
    }
    let scope = read_native_scope(&status.project_root)?;
    let mut needed = BTreeSet::new();
    for raw_path in changed_paths {
        let normalized = raw_path.replace('\\', "/");
        if normalized.is_empty()
            || normalized.starts_with('/')
            || normalized
                .split('/')
                .any(|segment| segment == ".." || segment.is_empty())
            || normalized == ".flopeek/config.json"
            || normalized.ends_with('/')
            || native_resolution_manifest_path(&normalized)
            || native_js_ignored_path(&normalized)
            || scope.classify(&normalized) == crate::scope::SourceScope::Excluded
            || !native_fact_supported_path(&normalized)
            || !status.candidate_paths.contains(&normalized)
        {
            return hydrate_native_js_source_facts(status);
        }
        needed.insert(normalized.clone());
        if let Some(importers) = status.reverse_importers.get(&normalized) {
            needed.extend(importers.iter().cloned());
        }
    }
    for path in needed {
        if status.facts.contains_key(&path) {
            continue;
        }
        let payload = status
            .compacted_facts
            .remove(&path)
            .ok_or_else(|| format!("Persistent source cache is missing facts for {path}."))?;
        let fact = serde_json::from_str(&payload)
            .map_err(|error| format!("Invalid compacted native parser fact for {path}: {error}"))?;
        status.facts.insert(path, fact);
    }
    Ok(())
}

pub fn compact_native_js_source_facts(status: &mut NativeJsFactsStatus) -> Result<(), String> {
    status.compacted_facts = status
        .facts
        .iter()
        .map(|(path, fact)| {
            serde_json::to_string(fact)
                .map(|payload| (path.clone(), payload))
                .map_err(|error| error.to_string())
        })
        .collect::<Result<_, _>>()?;
    status.facts.clear();
    Ok(())
}

/// Retain only the lightweight lineage needed to recognize this process's
/// promoted source session. Parser facts, resolution indexes, and record
/// manifests are durable derivatives of SQLite-backed inventory/fact caches;
/// keeping them beside the promoted StructuralFactBatch would create a second
/// in-memory authority for the same repository state.
pub fn evict_native_js_source_cache(status: &mut NativeJsFactsStatus) {
    // Keep a compact, process-local source session after promotion. The old
    // implementation dropped every parser fact and resolver index, forcing
    // the next one-file event through a full repository reparse even though
    // SQLite already held the exact complete batch. Serialize facts one by
    // one so the long-lived session retains only their compact JSON form; a
    // later refresh hydrates them while computing the validated affected set.
    if !status.facts.is_empty() {
        if status.compacted_facts.is_empty() {
            let facts = std::mem::take(&mut status.facts);
            for (path, fact) in facts {
                let payload = serde_json::to_string(&fact)
                    .expect("native parser facts must remain JSON serializable");
                status.compacted_facts.insert(path, payload);
            }
        } else {
            // Incremental refreshes may hydrate reverse-importer facts in
            // addition to the directly changed path. Re-serialize exactly the
            // facts currently held in memory so targeted hydrations are not
            // lost when the typed cache is evicted again.
            for (path, fact) in &status.facts {
                let payload = serde_json::to_string(fact)
                    .expect("native parser facts must remain JSON serializable");
                status.compacted_facts.insert(path.clone(), payload);
            }
            for path in &status.removed_paths {
                status.compacted_facts.remove(path);
            }
            status.facts.clear();
        }
    }
    if status.structural_records_complete {
        status.structural_record_digests =
            native_structural_record_digests(&status.structural_records);
        let records = std::mem::take(&mut status.structural_records);
        status.structural_record_manifest = records.iter().map(compact_structural_record).collect();
    } else {
        status.structural_records.clear();
    }
    status.structural_records.shrink_to_fit();
    status.structural_records_complete = false;
    status.structural_record_manifest.shrink_to_fit();
    // Resolution facts are needed only while rebuilding the affected records
    // for the next changed-path event. The reverse-importer index above is the
    // bounded lookup needed to select those files; retaining every resolver
    // payload beside compact parser facts duplicates the promoted batch and
    // inflates the native process peak on large repositories. Incremental
    // refresh repopulates only the affected resolver entries before record
    // construction, while a membership/configuration event hydrates and
    // resolves the complete set again.
    status.resolution.clear();
}

// Refresh one already-initialized no-cache session without re-walking the
// repository. Directory, scope/config, and non-JS/TS events deliberately
// require reconciliation because their candidate set cannot be inferred from a
// single path event safely.
pub fn refresh_native_js_facts_session(
    previous: &NativeJsFactsStatus,
    changed_paths: &[String],
) -> Result<NativeJsFactsStatus, String> {
    refresh_native_js_facts_session_owned(previous.clone(), changed_paths)
}

pub fn refresh_native_js_facts_session_owned(
    previous: NativeJsFactsStatus,
    changed_paths: &[String],
) -> Result<NativeJsFactsStatus, String> {
    let scope = read_native_scope(&previous.project_root)?;
    let excluded_count = previous
        .source_scope_counts
        .get("excluded")
        .copied()
        .unwrap_or(0);
    let records_were_complete = previous.structural_records_complete;
    let mut next = previous;
    next.initial_scan = false;
    // Package manifests are never admitted to this changed-path fast path:
    // their events require reconciliation above.  Therefore package-command
    // entries are stable for an ordinary JS/TS source edit.  The only
    // source-derived entry family is node-cron, so retain the existing entry
    // projection unless a changed fact can add, remove, or invalidate one.
    let mut entry_facts_affected = false;
    let mut changed = BTreeSet::new();
    let mut resolution_manifests = BTreeSet::new();
    for path in changed_paths {
        let normalized = path.replace('\\', "/");
        if normalized.is_empty()
            || normalized.starts_with('/')
            || normalized
                .split('/')
                .any(|segment| segment == ".." || segment.is_empty())
        {
            return Err(format!("native-session-reconcile-required:{path}"));
        }
        if normalized == ".flopeek/config.json" || normalized.ends_with('/') {
            return Err(format!("native-session-reconcile-required:{normalized}"));
        }
        if native_js_ignored_path(&normalized) {
            continue;
        }
        if native_resolution_manifest_path(&normalized) {
            entry_facts_affected |= normalized.ends_with("package.json");
            resolution_manifests.insert(normalized);
            continue;
        }
        // Excluded files are intentionally absent from candidate_paths, so an
        // add/delete event cannot be accounted for incrementally from the
        // previous candidate set. Force the owning client through full native
        // reconciliation so repository-scope telemetry changes immediately.
        if scope.classify(&normalized) == crate::scope::SourceScope::Excluded {
            return Err(format!("native-session-reconcile-required:{normalized}"));
        }
        if !native_fact_supported_path(&normalized) {
            return Err(format!("native-session-reconcile-required:{normalized}"));
        }
        let absolute = next.project_root.join(&normalized);
        let was_candidate = next.candidate_paths.contains(&normalized);
        let is_candidate = native_js_incremental_candidate(&scope, &normalized, &absolute)?;
        // Watchers can report ignored directories and a stale delete event for
        // a path that was never in the graph. Those events are safe no-ops;
        // admitting them would make incremental discovery diverge from the
        // initial inventory contract.
        if was_candidate || is_candidate {
            changed.insert(normalized);
        }
    }
    let mut parsed_files = 0;
    let mut removed_paths = Vec::new();
    let mut added_paths = BTreeSet::new();
    let previous_failed_by_path = changed
        .iter()
        .filter_map(|path| {
            next.facts
                .get(path)
                .map(|fact| (path.clone(), fact.status == "parse-failed"))
        })
        .collect::<BTreeMap<_, _>>();
    for path in &changed {
        let absolute = next.project_root.join(path);
        entry_facts_affected |= next
            .facts
            .get(path)
            .is_some_and(native_js_fact_affects_entry_projection);
        if !absolute.is_file() {
            next.facts.remove(path);
            next.candidate_paths.retain(|candidate| candidate != path);
            removed_paths.push(path.clone());
            continue;
        }
        let source = read_source_text(&absolute).map_err(|error| {
            format!("Unable to read JavaScript/TypeScript source {path}: {error}")
        })?;
        let fact = parse_native_js_facts(path, &source).ok_or_else(|| {
            format!("No native JavaScript/TypeScript parser is registered for {path}.")
        })?;
        entry_facts_affected |= native_js_fact_affects_entry_projection(&fact);
        parsed_files += 1;
        next.facts.insert(path.clone(), fact);
        next.source_hashes
            .insert(path.clone(), native_public_source_hash(&source));
        if !next.candidate_paths.contains(path) {
            next.candidate_paths.push(path.clone());
            added_paths.insert(path.clone());
        }
    }
    for path in &removed_paths {
        next.source_hashes.remove(path);
    }
    next.candidate_paths
        .sort_by(|left, right| js_locale_compare(left, right));
    let known_records = next
        .candidate_paths
        .iter()
        .map(|path| (path.clone(), scope.classify(path).as_str().to_string()))
        .collect::<Vec<_>>();
    let known_paths = known_records
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    let source_scopes = known_records.iter().cloned().collect::<BTreeMap<_, _>>();
    let record_orders = known_records
        .iter()
        .enumerate()
        .map(|(order, (path, _))| (path.clone(), order))
        .collect::<BTreeMap<_, _>>();
    // A changed source can alter its own imports or invalidate imports that
    // previously resolved to it. New files are deliberately conservative:
    // unresolved relative imports may now bind, so their resolution requires
    // a pass over the existing facts, but still never reparses or rereads
    // their source bodies.
    let mut affected_resolution_paths = changed
        .iter()
        .flat_map(|path| {
            next.reverse_importers
                .get(path)
                .into_iter()
                .flatten()
                .cloned()
        })
        .collect::<BTreeSet<_>>();
    affected_resolution_paths.extend(
        changed
            .iter()
            .filter(|path| next.facts.contains_key(*path))
            .cloned(),
    );
    let membership_changed = !added_paths.is_empty() || !removed_paths.is_empty();
    if membership_changed || !resolution_manifests.is_empty() {
        // A membership or resolver-manifest change renumbers or invalidates
        // the complete record set. The ordinary changed-file path can stay
        // lazy, but this branch must hydrate every cached fact before it
        // rebuilds all records and their global orders.
        hydrate_native_js_source_facts(&mut next)?;
        // Candidate membership changes recordOrder for subsequent files. Keep
        // the public batch deterministic by rebuilding records from cached
        // facts/hashes, not by reparsing or rereading every source file.
        // Resolution manifests have the same global effect without changing
        // source membership, so they also reconcile every cached resolver
        // record while retaining parsed source facts.
        affected_resolution_paths = next.facts.keys().cloned().collect();
    }
    for path in &removed_paths {
        next.resolution.remove(path);
    }
    let affected_facts = affected_resolution_paths
        .iter()
        .filter_map(|path| {
            next.facts
                .get(path)
                .cloned()
                .map(|facts| (path.clone(), facts))
        })
        .collect::<BTreeMap<_, _>>();
    let refreshed_resolution =
        resolve_native_js_imports(&next.project_root, &affected_facts, &known_paths);
    next.resolution.extend(refreshed_resolution);
    let refreshed_records = build_native_js_structural_records_with_source_hashes(
        &next.project_root,
        &affected_facts,
        &next.resolution,
        &source_scopes,
        &record_orders,
        Some(&next.source_hashes),
    )?;
    let refreshed_paths = affected_facts.keys().cloned().collect::<BTreeSet<_>>();
    update_native_resolution_indexes(
        &mut next,
        &refreshed_paths,
        membership_changed || !resolution_manifests.is_empty(),
    );
    if records_were_complete {
        next.structural_records.retain(|record| {
            match record
                .get("relativePath")
                .and_then(serde_json::Value::as_str)
            {
                Some(path) => {
                    !refreshed_paths.contains(path)
                        && !removed_paths.iter().any(|removed| removed == path)
                }
                None => true,
            }
        });
    } else {
        next.structural_records.clear();
    }
    if !records_were_complete {
        let refreshed_manifest = refreshed_records
            .iter()
            .map(compact_structural_record)
            .collect::<Vec<_>>();
        next.structural_record_manifest.retain(|record| {
            record
                .get("relativePath")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|path| {
                    !refreshed_paths.contains(path)
                        && !removed_paths.iter().any(|removed| removed == path)
                })
        });
        next.structural_record_manifest.extend(refreshed_manifest);
        normalize_structural_record_orders(&mut next.structural_record_manifest);
    }
    next.structural_records.extend(refreshed_records);
    if records_were_complete {
        // A complete refresh owns the entire collection and may safely
        // normalize its order. In a compact persistent session this vector
        // contains only affected records; their recordOrder is already the
        // global order and must not be renumbered from zero.
        normalize_structural_record_orders(&mut next.structural_records);
    }
    // A compact persistent session keeps only changed full records plus
    // headers for the unchanged set. Update the durable equality checkpoint
    // from those changed records so a later reconciliation can still identify
    // exactly which paths moved without rebuilding every record first.
    next.structural_record_digests
        .extend(native_structural_record_digests(&next.structural_records));
    for path in &removed_paths {
        next.structural_record_digests.remove(path);
    }
    next.structural_records_complete = records_were_complete;
    if entry_facts_affected || !added_paths.is_empty() || !removed_paths.is_empty() {
        if !next.structural_records_complete {
            ensure_complete_native_js_structural_records(&mut next)?;
        }
        next.entry_facts =
            build_native_js_entry_facts(&next.project_root, &next.facts, &next.structural_records);
    }
    next.parsed_files = parsed_files;
    let supported_candidate_files = next
        .candidate_paths
        .iter()
        .filter(|path| native_fact_supported_path(path))
        .count();
    next.reused_files = supported_candidate_files.saturating_sub(parsed_files);
    let mut failed_files = next.failed_files;
    for path in &changed {
        let before = previous_failed_by_path.get(path).copied().unwrap_or(false);
        let after = next
            .facts
            .get(path)
            .is_some_and(|fact| fact.status == "parse-failed");
        failed_files = failed_files.saturating_sub(usize::from(before));
        failed_files = failed_files.saturating_add(usize::from(after));
    }
    next.failed_files = failed_files;
    next.removed_facts = removed_paths.len();
    next.candidate_files = next.candidate_paths.len();
    next.changed_paths = changed
        .into_iter()
        .chain(resolution_manifests)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    next.reused_paths = next
        .candidate_paths
        .iter()
        .filter(|path| !next.changed_paths.contains(path))
        .cloned()
        .collect();
    next.removed_paths = removed_paths;
    next.changed_record_paths = refreshed_paths
        .into_iter()
        .chain(next.removed_paths.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    // Excluded files are intentionally absent from candidate_paths. Preserve
    // their full-discovery count across ordinary source events; only a scope
    // reconciliation is allowed to replace it.
    next.source_scope_counts = ["application", "test", "fixture", "generated", "excluded"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                if name == "excluded" {
                    excluded_count
                } else {
                    0usize
                },
            )
        })
        .collect();
    for (_, source_scope) in known_records {
        *next.source_scope_counts.entry(source_scope).or_default() += 1;
    }
    next.scope_source = scope.source;
    next.flow_entries_tests = scope.flow_entries_tests;
    next.flow_entries_fixtures = scope.flow_entries_fixtures;
    Ok(next)
}

pub fn ensure_complete_native_js_structural_records(
    status: &mut NativeJsFactsStatus,
) -> Result<(), String> {
    if status.structural_records_complete {
        return Ok(());
    }
    hydrate_native_js_source_facts(status)?;
    let scope = read_native_scope(&status.project_root)?;
    let known_records = status
        .candidate_paths
        .iter()
        .map(|path| (path.clone(), scope.classify(path).as_str().to_string()))
        .collect::<Vec<_>>();
    let source_scopes = known_records.iter().cloned().collect::<BTreeMap<_, _>>();
    let record_orders = known_records
        .iter()
        .enumerate()
        .map(|(order, (path, _))| (path.clone(), order))
        .collect::<BTreeMap<_, _>>();
    status.structural_records = build_native_js_structural_records_with_source_hashes(
        &status.project_root,
        &status.facts,
        &status.resolution,
        &source_scopes,
        &record_orders,
        Some(&status.source_hashes),
    )?;
    normalize_structural_record_orders(&mut status.structural_records);
    status.structural_records_complete = true;
    status.structural_record_manifest.clear();
    Ok(())
}

pub fn compact_native_js_structural_records(status: &mut NativeJsFactsStatus) {
    if !status.structural_records_complete {
        return;
    }
    status.structural_record_manifest = status
        .structural_records
        .iter()
        .map(compact_structural_record)
        .collect();
    status.structural_records.clear();
    status.structural_records.shrink_to_fit();
    status.structural_records_complete = false;
}

pub fn take_complete_native_js_structural_records(
    status: &mut NativeJsFactsStatus,
) -> Result<Vec<serde_json::Value>, String> {
    if !status.structural_records_complete {
        return Err("Complete native structural records are unavailable.".to_string());
    }
    status.structural_record_manifest = status
        .structural_records
        .iter()
        .map(compact_structural_record)
        .collect();
    status.structural_record_digests = native_structural_record_digests(&status.structural_records);
    status.structural_records_complete = false;
    Ok(std::mem::take(&mut status.structural_records))
}

pub fn native_structural_record_digests(records: &[serde_json::Value]) -> BTreeMap<String, String> {
    records
        .iter()
        .filter_map(|record| {
            let path = record.get("relativePath")?.as_str()?.to_string();
            let bytes = serde_json::to_vec(record).ok()?;
            Some((path, format!("sha256:{:x}", Sha256::digest(bytes))))
        })
        .collect()
}

fn compact_structural_record(record: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "relativePath": record["relativePath"],
        "sourceHash": record["sourceHash"],
        "sourceScope": record["sourceScope"],
        "recordOrder": record["recordOrder"],
        "language": record["language"],
        "fileMetadata": record["fileMetadata"],
    })
}

fn native_js_fact_affects_entry_projection(fact: &NativeJsFacts) -> bool {
    !fact.structural.schedules.is_empty() || !fact.structural.unsupported_schedules.is_empty()
}

fn native_js_incremental_candidate(
    scope: &crate::scope::NativeScope,
    relative_path: &str,
    absolute_path: &Path,
) -> Result<bool, String> {
    if scope.classify(relative_path) == crate::scope::SourceScope::Excluded {
        return Ok(false);
    }
    let metadata = match fs::symlink_metadata(absolute_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Unable to inspect JavaScript/TypeScript source {relative_path}: {error}"
            ));
        }
    };
    Ok(metadata.file_type().is_file() && metadata.len() <= MAX_NATIVE_SOURCE_FILE_BYTES)
}

fn native_js_ignored_path(relative_path: &str) -> bool {
    let segments = relative_path.split('/').collect::<Vec<_>>();
    segments
        .iter()
        .take(segments.len().saturating_sub(1))
        .any(|segment| {
            segment.starts_with('.')
                || matches!(
                    *segment,
                    ".flopeek"
                        | ".git"
                        | ".next"
                        | ".nuxt"
                        | ".project-flow"
                        | ".turbo"
                        | "build"
                        | "coverage"
                        | "dist"
                        | "node_modules"
                        | "out"
                        | "target"
                        | "vendor"
                )
        })
}

fn native_resolution_manifest_path(relative_path: &str) -> bool {
    matches!(
        relative_path.rsplit('/').next().unwrap_or(relative_path),
        "go.mod" | "package.json" | "tsconfig.json" | "jsconfig.json"
    )
}

fn language_for_path(path: &str) -> Option<Language> {
    let extension = path.rsplit('.').next()?.to_ascii_lowercase();
    match extension.as_str() {
        "js" | "cjs" | "mjs" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "cs" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "svelte" => Some(tree_sitter_svelte_next::LANGUAGE.into()),
        _ => None,
    }
}

/// Extensions that Flopeek registers for inventory, but for which the native
/// source authority deliberately has no structural parser.  These must remain
/// visible as inventory-only records, rather than making a mixed repository
/// look unsupported or silently disappearing from the native graph.
fn inventory_only_adapter_name(path: &str) -> Option<&'static str> {
    let filename = path.rsplit('/').next()?.to_ascii_lowercase();
    if filename == "makefile" {
        return Some("makefile");
    }
    match filename.rsplit('.').next()? {
        "asm" => Some("assembly"),
        "astro" => Some("astro"),
        "bash" | "sh" | "zsh" => Some("shell"),
        "c" => Some("c"),
        "cc" | "cpp" | "cxx" => Some("cpp"),
        "h" => Some("headers"),
        "kt" | "kts" => Some("kotlin"),
        "rb" => Some("ruby"),
        "scala" => Some("scala"),
        "swift" => Some("swift"),
        "vue" => Some("vue"),
        _ => None,
    }
}

fn native_fact_supported_path(path: &str) -> bool {
    language_for_path(path).is_some() || inventory_only_adapter_name(path).is_some()
}

fn inventory_only_native_facts(path: &str) -> Option<NativeJsFacts> {
    inventory_only_adapter_name(path)?;
    let filename = path.rsplit('/').next().unwrap_or(path);
    let extension = filename
        .rfind('.')
        .map(|index| filename[index..].to_string())
        .unwrap_or_else(|| ".makefile".to_string());
    let subject = match extension.as_str() {
        ".makefile" => "Makefile build-control files",
        ".asm" => "assembly source files",
        _ => extension.as_str(),
    };
    Some(NativeJsFacts {
        schema_version: NATIVE_JS_FACTS_SCHEMA.to_string(),
        parser: "inventory".to_string(),
        status: "inventory-only".to_string(),
        diagnostics: 0,
        imports: Vec::new(),
        symbols: Vec::new(),
        direct_calls: Vec::new(),
        structural: NativeJsStructuralFacts {
            imports: Vec::new(),
            symbols: Vec::new(),
            canonical_symbols: Vec::new(),
            calls: Vec::new(),
            endpoints: Vec::new(),
            requests: Vec::new(),
            integrations: Vec::new(),
            framework_commands: Vec::new(),
            unsupported_framework_commands: Vec::new(),
            runtime_actions: Vec::new(),
            schedules: Vec::new(),
            unsupported_schedules: Vec::new(),
            methods: Vec::new(),
            analysis: NativeJsAnalysis {
                parser: "inventory".to_string(),
                status: "inventory-only".to_string(),
                confidence: "not-analyzed".to_string(),
                diagnostics: 0,
                reason: Some(format!("No structural adapter registered for {subject}.")),
            },
        },
    })
}

fn source_text(node: Node<'_>, source: &str) -> Option<String> {
    node.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

fn identifier_for(node: Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .or_else(|| node.named_child(0))
        .and_then(|child| source_text(child, source))
        .filter(|value| !value.is_empty())
}

fn import_specifier(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == "string")
        .and_then(|child| source_text(child, source))
        .map(|value| value.trim_matches(['\'', '"']).to_string())
        .filter(|value| !value.is_empty())
}

fn direct_call_name(node: Node<'_>, source: &str) -> Option<String> {
    let function = node.child_by_field_name("function")?;
    // Match the JavaScript compatibility oracle's supported subset: only direct
    // identifier calls can create a static call relationship. Property/method
    // dispatch (`response.json()`, `router.post()`) is intentionally excluded
    // because its receiver cannot be resolved safely from this bounded fact.
    (function.kind() == "identifier")
        .then(|| source_text(function, source))
        .flatten()
        .filter(|name| name != "require")
}

fn commonjs_specifier(node: Node<'_>, source: &str) -> Option<String> {
    let value = node.child_by_field_name("value")?;
    if value.kind() != "call_expression" {
        return None;
    }
    let function = value.child_by_field_name("function")?;
    if function.kind() != "identifier"
        || source_text(function, source).as_deref() != Some("require")
    {
        return None;
    }
    let arguments = value.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .find(|argument| argument.kind() == "string")
        .and_then(|argument| source_text(argument, source))
        .map(|specifier| specifier.trim_matches(['\'', '"']).to_string())
        .filter(|specifier| !specifier.is_empty())
}

fn string_value(node: Node<'_>, source: &str) -> Option<String> {
    matches!(node.kind(), "string" | "template_string")
        .then(|| source_text(node, source))
        .flatten()
        .map(|value| value.trim_matches(['\'', '"', '`']).to_string())
        .filter(|value| !value.is_empty())
}

fn typescript_column(source: &str, byte_offset: usize) -> usize {
    let safe_offset = byte_offset.min(source.len());
    let line_start = source[..safe_offset]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    source[line_start..safe_offset].encode_utf16().count() + 1
}

// JavaScript's default localeCompare ordering is intentionally used by the
// source scanner for record order. Keep the ASCII subset here explicit rather
// than falling back to byte ordering, which puts `404` before `_app` and a
// `.js` suffix before underscore-qualified sibling names on Windows.
fn js_locale_char_weight(character: char) -> (u8, u32) {
    match character {
        ' ' => (0, 0),
        '_' => (0, 1),
        '-' => (0, 2),
        '.' => (0, 3),
        '[' => (0, 4),
        '@' => (0, 5),
        '/' => (0, 6),
        '0'..='9' => (1, character as u32 - '0' as u32),
        'a'..='z' => (2, character as u32 - 'a' as u32),
        'A'..='Z' => (2, character as u32 - 'A' as u32),
        _ => (3, character as u32),
    }
}

pub(crate) fn js_locale_compare(left: &str, right: &str) -> Ordering {
    // V8's default `String#localeCompare` delegates non-ASCII ordering to ICU.
    // Retain the audited ASCII compatibility path below: ICU punctuation
    // tailoring is intentionally different from the current JavaScript scanner
    // contract for names such as `_app`, `404`, and `foo.js`.
    if !(left.is_ascii() && right.is_ascii())
        && let Some(collator) = javascript_unicode_collator()
    {
        let order = collator.compare(left, right);
        if order != Ordering::Equal {
            return order;
        }
    }
    let mut left_characters = left.chars();
    let mut right_characters = right.chars();
    loop {
        match (left_characters.next(), right_characters.next()) {
            (Some(left_character), Some(right_character)) => {
                let order = js_locale_char_weight(left_character)
                    .cmp(&js_locale_char_weight(right_character));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => break,
        }
    }
    // ICU's default JavaScript collation compares the case-folded spelling
    // first, then uses case as a secondary tie-breaker. Therefore AppCheck
    // comes before auth, while a still comes before A.
    for (left_character, right_character) in left.chars().zip(right.chars()) {
        let left_case = usize::from(left_character.is_ascii_uppercase());
        let right_case = usize::from(right_character.is_ascii_uppercase());
        let order = left_case.cmp(&right_case);
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

fn javascript_unicode_collator() -> Option<&'static CollatorBorrowed<'static>> {
    static COLLATOR: OnceLock<Option<CollatorBorrowed<'static>>> = OnceLock::new();
    COLLATOR
        .get_or_init(|| Collator::try_new(locale!("en").into(), Default::default()).ok())
        .as_ref()
}

fn evidence(path: &str, source: &str, node: Node<'_>) -> NativeJsEvidence {
    let start = node.start_position();
    let end = node.end_position();
    NativeJsEvidence {
        parser: "typescript-ast".to_string(),
        file: path.to_string(),
        range: NativeJsRange {
            start: NativeJsPosition {
                line: start.row + 1,
                column: typescript_column(source, node.start_byte()),
            },
            end: NativeJsPosition {
                line: end.row + 1,
                column: typescript_column(source, node.end_byte()),
            },
        },
    }
}

fn exported_declaration_evidence_node(node: Node<'_>) -> Node<'_> {
    node.parent()
        .filter(|parent| parent.kind() == "export_statement")
        .unwrap_or(node)
}

fn is_top_level(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "program"
            || (parent.kind() == "export_statement"
                && parent
                    .parent()
                    .is_some_and(|grandparent| grandparent.kind() == "program"))
    })
}

fn top_level_variable(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "variable_declaration" | "lexical_declaration" => {
                return is_top_level(parent);
            }
            "export_statement" => {
                return parent.parent().is_some_and(|item| item.kind() == "program");
            }
            "program" => return false,
            _ => current = parent.parent(),
        }
    }
    false
}

fn class_methods(node: Node<'_>, source: &str) -> Vec<String> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter(|child| matches!(child.kind(), "method_definition" | "method_signature"))
        .filter_map(|child| child.child_by_field_name("name"))
        .filter_map(|name| source_text(name, source))
        .filter(|name| !name.starts_with('#') && name != "constructor")
        .collect()
}

fn function_like_variable(node: Node<'_>) -> bool {
    node.child_by_field_name("value").is_some_and(|value| {
        matches!(
            value.kind(),
            "arrow_function" | "function_expression" | "generator_function"
        )
    })
}

fn enclosing_top_level_symbol(node: Node<'_>, source: &str) -> Option<NativeJsSymbolReference> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "class_declaration" if is_top_level(parent) => {
                return identifier_for(parent, source).map(|name| NativeJsSymbolReference {
                    symbol_type: "class".to_string(),
                    name,
                });
            }
            "function_declaration" | "generator_function_declaration" if is_top_level(parent) => {
                return identifier_for(parent, source).map(|name| NativeJsSymbolReference {
                    symbol_type: "function".to_string(),
                    name,
                });
            }
            "variable_declarator" if top_level_variable(parent) => {
                return parent
                    .child_by_field_name("name")
                    .filter(|name| name.kind() == "identifier")
                    .and_then(|name| source_text(name, source))
                    .map(|name| NativeJsSymbolReference {
                        symbol_type: "function".to_string(),
                        name,
                    });
            }
            "program" => return None,
            _ => current = parent.parent(),
        }
    }
    None
}

fn binding_has_name(node: Node<'_>, name: &str, source: &str) -> bool {
    if node.kind() == "identifier" {
        return source_text(node, source).as_deref() == Some(name);
    }
    node.child_by_field_name("pattern")
        .or_else(|| node.child_by_field_name("name"))
        .filter(|binding| binding.kind() == "identifier")
        .and_then(|binding| source_text(binding, source))
        .as_deref()
        == Some(name)
}

fn declaration_has_name(node: Node<'_>, name: &str, source: &str) -> bool {
    match node.kind() {
        "lexical_declaration" | "variable_declaration" => named_children(node)
            .into_iter()
            .filter(|child| child.kind() == "variable_declarator")
            .filter_map(|child| child.child_by_field_name("name"))
            .any(|binding| binding_has_name(binding, name, source)),
        "function_declaration" | "generator_function_declaration" | "class_declaration" => {
            identifier_for(node, source).as_deref() == Some(name)
        }
        _ => false,
    }
}

fn call_name_is_shadowed(node: Node<'_>, name: &str, source: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "program" {
            return false;
        }
        if matches!(
            parent.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "generator_function"
                | "method_definition"
        ) && parent
            .child_by_field_name("parameters")
            .is_some_and(|parameters| {
                named_children(parameters)
                    .into_iter()
                    .any(|parameter| binding_has_name(parameter, name, source))
            })
        {
            return true;
        }
        if parent.kind() == "statement_block"
            && named_children(parent)
                .into_iter()
                .any(|statement| declaration_has_name(statement, name, source))
        {
            return true;
        }
        if parent.kind() == "catch_clause"
            && parent
                .child_by_field_name("parameter")
                .is_some_and(|parameter| binding_has_name(parameter, name, source))
        {
            return true;
        }
        if matches!(parent.kind(), "for_statement" | "for_in_statement")
            && parent
                .child_by_field_name("initializer")
                .is_some_and(|initializer| declaration_has_name(initializer, name, source))
        {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn call_arguments(node: Node<'_>) -> Vec<Node<'_>> {
    node.child_by_field_name("arguments")
        .map(named_children)
        .unwrap_or_default()
}

fn member_receiver_and_name(node: Node<'_>, source: &str) -> Option<(String, String)> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "member_expression" {
        return None;
    }
    let receiver = function.child_by_field_name("object")?;
    let property = function.child_by_field_name("property")?;
    if receiver.kind() != "identifier" {
        return None;
    }
    Some((
        source_text(receiver, source)?,
        source_text(property, source)?,
    ))
}

fn import_bindings(
    node: Node<'_>,
    source: &str,
    specifier: &str,
    bindings: &mut BTreeMap<String, NativeJsImportedReference>,
) {
    fn visit(
        node: Node<'_>,
        source: &str,
        specifier: &str,
        bindings: &mut BTreeMap<String, NativeJsImportedReference>,
    ) {
        if node.kind() == "import_specifier" {
            let exported = node
                .child_by_field_name("name")
                .and_then(|item| source_text(item, source));
            let local = node
                .child_by_field_name("alias")
                .and_then(|item| source_text(item, source))
                .or_else(|| exported.clone());
            if let (Some(exported_name), Some(local_name)) = (exported, local) {
                bindings.insert(
                    local_name,
                    NativeJsImportedReference {
                        specifier: specifier.to_string(),
                        exported_name,
                    },
                );
            }
        }
        for child in named_children(node) {
            visit(child, source, specifier, bindings);
        }
    }
    visit(node, source, specifier, bindings);
}

fn default_import_name(node: Node<'_>, source: &str) -> Option<String> {
    let clause = named_children(node)
        .into_iter()
        .find(|child| child.kind() == "import_clause")?;
    named_children(clause)
        .into_iter()
        .find(|child| child.kind() == "identifier")
        .and_then(|child| source_text(child, source))
}

fn collect_bindings(
    node: Node<'_>,
    source: &str,
    imports: &mut BTreeMap<String, NativeJsImportedReference>,
    cron_receivers: &mut BTreeSet<String>,
    fastify_factories: &mut BTreeSet<String>,
) {
    if node.kind() == "import_statement"
        && let Some(specifier) = import_specifier(node, source)
    {
        import_bindings(node, source, &specifier, imports);
        if specifier == "node-cron"
            && let Some(name) = default_import_name(node, source)
        {
            cron_receivers.insert(name);
        }
        if specifier == "fastify"
            && let Some(name) = default_import_name(node, source)
        {
            fastify_factories.insert(name);
        }
        for (local, imported) in imports.iter() {
            if imported.specifier == "fastify" && imported.exported_name == "fastify" {
                fastify_factories.insert(local.clone());
            }
        }
    }
    if node.kind() == "variable_declarator"
        && top_level_variable(node)
        && let Some(specifier) = commonjs_specifier(node, source)
        && let Some(pattern) = node.child_by_field_name("name")
        && pattern.kind() == "object_pattern"
    {
        for item in named_children(pattern) {
            let pair = match item.kind() {
                "pair_pattern" | "pair" => {
                    let exported = item
                        .child_by_field_name("key")
                        .and_then(|value| source_text(value, source));
                    let local = item
                        .child_by_field_name("value")
                        .and_then(|value| source_text(value, source));
                    exported.zip(local)
                }
                "shorthand_property_identifier_pattern" | "shorthand_property_identifier" => {
                    source_text(item, source).map(|name| (name.clone(), name))
                }
                _ => None,
            };
            if let Some((exported_name, local_name)) = pair {
                imports.insert(
                    local_name,
                    NativeJsImportedReference {
                        specifier: specifier.clone(),
                        exported_name,
                    },
                );
            }
        }
    }
    for child in named_children(node) {
        collect_bindings(child, source, imports, cron_receivers, fastify_factories);
    }
}

fn next_route(path: &str) -> Option<String> {
    let parts = path.split('/').collect::<Vec<_>>();
    let filename = parts.last()?;
    if filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(filename)
        != "route"
    {
        return None;
    }
    let app_index = parts
        .iter()
        .enumerate()
        .find(|(index, part)| **part == "app" && (*index == 0 || parts[*index - 1] == "src"))?
        .0;
    let segments = parts[app_index + 1..parts.len() - 1]
        .iter()
        .filter(|segment| !(segment.starts_with('(') && segment.ends_with(')')))
        .map(|segment| {
            if segment.starts_with("[...") && segment.ends_with(']') {
                format!("*{}", &segment[4..segment.len() - 1])
            } else if segment.starts_with('[') && segment.ends_with(']') {
                format!(":{}", &segment[1..segment.len() - 1])
            } else {
                (*segment).to_string()
            }
        })
        .collect::<Vec<_>>();
    Some(if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    })
}

fn first_parameter_name(node: Node<'_>, source: &str) -> Option<String> {
    let parameters = node.child_by_field_name("parameters")?;
    let first = named_children(parameters).into_iter().next()?;
    if first.kind() == "identifier" {
        return source_text(first, source);
    }
    first
        .child_by_field_name("pattern")
        .or_else(|| first.child_by_field_name("name"))
        .filter(|item| item.kind() == "identifier")
        .and_then(|item| source_text(item, source))
}

fn unavailable_next_contract(handler_name: &str, request_name: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": "flopeek-next-route-contract/v1",
        "adapter": "next-route-handler",
        "handlerName": handler_name,
        "request": {
            "status": "unavailable",
            "fields": [],
            "reason": if request_name.is_some() {
                "No single inline TypeScript object-literal schema was found for this handler's request.json() call."
            } else {
                "The handler has no simple identifier request parameter for parser-backed payload extraction."
            },
        },
        "responses": {
            "status": "unavailable",
            "variants": [],
            "reason": "No returned Response.json/NextResponse.json call with an object-literal body and explicit numeric status was found in this handler.",
        },
    })
}

fn valid_cron_expression(value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    let fields = value.split_whitespace().collect::<Vec<_>>();
    (fields.len() == 5 || fields.len() == 6)
        && fields.iter().all(|field| {
            !field.is_empty()
                && field.chars().all(|character| {
                    character.is_ascii_alphanumeric() || "*/?,-".contains(character)
                })
        })
}

fn is_module_scope(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "program" {
            return true;
        }
        if matches!(
            parent.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "generator_function"
                | "class_declaration"
                | "class"
                | "internal_module"
        ) {
            return false;
        }
        current = parent.parent();
    }
    false
}

fn object_string_property(node: Node<'_>, source: &str, property_name: &str) -> Option<String> {
    if node.kind() != "object" {
        return None;
    }
    for child in named_children(node) {
        if child.kind() != "pair" {
            continue;
        }
        let key = child
            .child_by_field_name("key")
            .and_then(|item| source_text(item, source));
        if key.as_deref().map(|item| item.trim_matches(['\'', '"'])) != Some(property_name) {
            continue;
        }
        return child
            .child_by_field_name("value")
            .and_then(|item| string_value(item, source));
    }
    None
}

fn compact_signature_type(value: &str) -> String {
    value
        .trim_start_matches(':')
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn canonical_function_signature(node: Node<'_>, source: &str) -> String {
    let parameters = node
        .child_by_field_name("parameters")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .map(|parameter| {
            let prefix = if parameter.kind() == "rest_pattern" {
                "..."
            } else {
                ""
            };
            let kind = parameter
                .child_by_field_name("type")
                .or_else(|| {
                    named_children(parameter)
                        .into_iter()
                        .find(|child| child.kind() == "type_annotation")
                })
                .and_then(|kind| source_text(kind, source))
                .map(|kind| compact_signature_type(&kind))
                .filter(|kind| !kind.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            format!("{prefix}{kind}")
        })
        .collect::<Vec<_>>()
        .join(",");
    let return_type = node
        .child_by_field_name("return_type")
        .or_else(|| {
            named_children(node)
                .into_iter()
                .find(|child| child.kind() == "type_annotation")
        })
        .and_then(|kind| source_text(kind, source))
        .map(|kind| compact_signature_type(&kind))
        .filter(|kind| !kind.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    format!("({parameters}):{return_type}")
}

fn canonical_class_methods(
    class: Node<'_>,
    path: &str,
    source: &str,
    class_name: &str,
) -> Vec<NativeJsStructuralSymbol> {
    let owner = NativeJsSymbolReference {
        symbol_type: "class".to_string(),
        name: class_name.to_string(),
    };
    class
        .child_by_field_name("body")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .filter(|member| matches!(member.kind(), "method_definition" | "method_signature"))
        .filter_map(|method| {
            let name = method
                .child_by_field_name("name")
                .and_then(|name| source_text(name, source))?;
            let discriminator = if name == "constructor" {
                "constructor"
            } else if source_text(method, source)
                .is_some_and(|text| text.split_whitespace().any(|item| item == "static"))
            {
                "static-method"
            } else {
                "instance-method"
            };
            Some(NativeJsStructuralSymbol {
                symbol_type: if name == "constructor" {
                    "constructor"
                } else {
                    "method"
                }
                .to_string(),
                name: name.clone(),
                methods: vec![],
                evidence: evidence(path, source, method),
                identity: Some(NativeJsCanonicalSymbolIdentity {
                    qualified_name: format!("{class_name}.{name}"),
                    lexical_owner: Some(owner.clone()),
                    signature: Some(canonical_function_signature(method, source)),
                    discriminator: discriminator.to_string(),
                }),
            })
        })
        .collect()
}

struct StructuralInput<'a> {
    path: &'a str,
    source: &'a str,
}

struct StructuralOutputs<'a> {
    legacy_imports: &'a mut BTreeSet<String>,
    legacy_symbols: &'a mut BTreeSet<(String, String)>,
    legacy_calls: &'a mut BTreeSet<String>,
    facts: &'a mut NativeJsStructuralFacts,
}

fn collect_structural(
    node: Node<'_>,
    input: &StructuralInput<'_>,
    bindings: &BTreeMap<String, NativeJsImportedReference>,
    cron_receivers: &BTreeSet<String>,
    fastify_receivers: &BTreeSet<String>,
    outputs: &mut StructuralOutputs<'_>,
) {
    let path = input.path;
    let source = input.source;
    let legacy_imports = &mut *outputs.legacy_imports;
    let legacy_symbols = &mut *outputs.legacy_symbols;
    let legacy_calls = &mut *outputs.legacy_calls;
    let facts = &mut *outputs.facts;
    // The compatibility-only parser fields are collected during this same
    // traversal. Keeping a second complete AST walk for legacy imports,
    // symbols, and calls made every cold native scan pay the tree cost twice.
    match node.kind() {
        "import_statement" => {
            if let Some(specifier) = import_specifier(node, source) {
                legacy_imports.insert(specifier);
            }
        }
        "function_declaration" | "generator_function_declaration" | "function_signature" => {
            if let Some(name) = identifier_for(node, source) {
                legacy_symbols.insert(("function".to_string(), name));
            }
        }
        "class_declaration" => {
            if let Some(name) = identifier_for(node, source) {
                legacy_symbols.insert(("class".to_string(), name));
            }
        }
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            if let Some(name) = identifier_for(node, source) {
                legacy_symbols.insert(("type".to_string(), name));
            }
        }
        "variable_declarator" => {
            if let Some(specifier) = commonjs_specifier(node, source) {
                legacy_imports.insert(specifier);
            }
            let value = node.child_by_field_name("value");
            if value
                .is_some_and(|item| matches!(item.kind(), "arrow_function" | "function_expression"))
                && let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|item| source_text(item, source))
            {
                legacy_symbols.insert(("function".to_string(), name));
            }
        }
        "call_expression" => {
            if let Some(name) = direct_call_name(node, source) {
                legacy_calls.insert(name);
            }
        }
        _ => {}
    }
    if matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "function_signature"
            | "method_definition"
            | "method_signature"
    ) && facts.methods.len() < 12
        && let Some(name) = node
            .child_by_field_name("name")
            .and_then(|item| source_text(item, source))
        && !facts.methods.contains(&name)
        && !name.starts_with('#')
        && name != "constructor"
    {
        facts.methods.push(name);
    }
    match node.kind() {
        "import_statement" => {
            if let Some(specifier) = import_specifier(node, source) {
                facts.imports.push(NativeJsImport {
                    specifier,
                    standard: None,
                    evidence: evidence(path, source, node),
                });
            }
        }
        "export_statement" => {
            if let Some(specifier) = import_specifier(node, source) {
                facts.imports.push(NativeJsImport {
                    specifier,
                    standard: None,
                    evidence: evidence(path, source, node),
                });
            }
        }
        "class_declaration" if is_top_level(node) => {
            if let Some(name) = identifier_for(node, source) {
                let symbol = NativeJsStructuralSymbol {
                    symbol_type: "class".to_string(),
                    name: name.clone(),
                    methods: class_methods(node, source),
                    evidence: evidence(path, source, exported_declaration_evidence_node(node)),
                    identity: Some(NativeJsCanonicalSymbolIdentity {
                        qualified_name: name.clone(),
                        lexical_owner: None,
                        signature: None,
                        discriminator: "type".to_string(),
                    }),
                };
                facts.symbols.push(symbol.clone());
                facts.canonical_symbols.push(symbol);
                facts
                    .canonical_symbols
                    .extend(canonical_class_methods(node, path, source, &name));
            }
        }
        "function_declaration" | "generator_function_declaration" | "function_signature"
            if is_top_level(node) =>
        {
            if let Some(name) = identifier_for(node, source) {
                let symbol = NativeJsStructuralSymbol {
                    symbol_type: "function".to_string(),
                    name: name.clone(),
                    methods: Vec::new(),
                    evidence: evidence(path, source, exported_declaration_evidence_node(node)),
                    identity: Some(NativeJsCanonicalSymbolIdentity {
                        qualified_name: name.clone(),
                        lexical_owner: None,
                        signature: Some(canonical_function_signature(node, source)),
                        discriminator: "top-level-function".to_string(),
                    }),
                };
                facts.symbols.push(symbol.clone());
                facts.canonical_symbols.push(symbol);
                if matches!(name.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
                    && node
                        .parent()
                        .is_some_and(|parent| parent.kind() == "export_statement")
                    && let Some(route) = next_route(path)
                {
                    let request_name = first_parameter_name(node, source);
                    facts.endpoints.push(NativeJsEndpoint {
                        method: name.clone(),
                        route,
                        handler_name: Some(name.clone()),
                        handler_type: Some("function".to_string()),
                        contract: Some(unavailable_next_contract(&name, request_name.as_deref())),
                        confidence: None,
                        detected_responsibility: None,
                        evidence: evidence(path, source, exported_declaration_evidence_node(node)),
                    });
                }
            }
        }
        "variable_declarator" if top_level_variable(node) && function_like_variable(node) => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|item| source_text(item, source))
            {
                let function = node.child_by_field_name("value").unwrap_or(node);
                let symbol = NativeJsStructuralSymbol {
                    symbol_type: "function".to_string(),
                    name: name.clone(),
                    methods: Vec::new(),
                    evidence: evidence(path, source, node),
                    identity: Some(NativeJsCanonicalSymbolIdentity {
                        qualified_name: name,
                        lexical_owner: None,
                        signature: Some(canonical_function_signature(function, source)),
                        discriminator: "top-level-function".to_string(),
                    }),
                };
                facts.symbols.push(symbol.clone());
                facts.canonical_symbols.push(symbol);
            }
        }
        "call_expression" => {
            let arguments = call_arguments(node);
            let function = node.child_by_field_name("function");
            if let Some(function) = function
                && function.kind() == "identifier"
                && let Some(name) = source_text(function, source)
            {
                if name == "require" {
                    if let Some(specifier) = arguments
                        .first()
                        .and_then(|item| string_value(*item, source))
                    {
                        facts.imports.push(NativeJsImport {
                            specifier,
                            standard: None,
                            evidence: evidence(path, source, node),
                        });
                    }
                } else if !call_name_is_shadowed(node, &name, source) {
                    facts.calls.push(NativeJsCall {
                        imported: bindings.get(&name).cloned(),
                        source: enclosing_top_level_symbol(node, source),
                        name: name.clone(),
                        evidence: evidence(path, source, node),
                    });
                    if name == "fetch"
                        && let Some(route) = arguments
                            .first()
                            .and_then(|item| string_value(*item, source))
                        && route.starts_with('/')
                    {
                        let method = arguments
                            .get(1)
                            .and_then(|item| object_string_property(*item, source, "method"))
                            .map(|item| item.to_ascii_uppercase())
                            .filter(|item| {
                                matches!(item.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
                            })
                            .unwrap_or_else(|| "GET".to_string());
                        facts.requests.push(NativeJsRequest {
                            method,
                            route,
                            evidence: evidence(path, source, node),
                        });
                    }
                }
            }
            if let Some((receiver, method)) = member_receiver_and_name(node, source) {
                let upper = method.to_ascii_uppercase();
                if matches!(upper.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
                    && (["app", "router", "server"]
                        .contains(&receiver.to_ascii_lowercase().as_str())
                        || fastify_receivers.contains(&receiver.to_ascii_lowercase()))
                    && let Some(route) = arguments
                        .first()
                        .and_then(|item| string_value(*item, source))
                {
                    facts.endpoints.push(NativeJsEndpoint {
                        method: upper,
                        route,
                        handler_name: None,
                        handler_type: None,
                        contract: None,
                        confidence: None,
                        detected_responsibility: None,
                        evidence: evidence(path, source, node),
                    });
                }
                if method == "schedule" && cron_receivers.contains(&receiver) {
                    let expression = arguments
                        .first()
                        .and_then(|item| string_value(*item, source));
                    let task_name = arguments
                        .get(1)
                        .filter(|item| item.kind() == "identifier")
                        .and_then(|item| source_text(*item, source));
                    if !is_module_scope(node) {
                        facts
                            .unsupported_schedules
                            .push(NativeJsUnsupportedSchedule {
                                path: path.to_string(),
                                reason: "registration-is-not-module-scope".to_string(),
                                evidence: None,
                            });
                    } else if expression.as_deref().is_some_and(valid_cron_expression)
                        && task_name.is_some()
                    {
                        facts.schedules.push(NativeJsSchedule {
                            expression: expression.unwrap(),
                            task_name: task_name.unwrap(),
                            evidence: evidence(path, source, node),
                        });
                    } else {
                        facts
                            .unsupported_schedules
                            .push(NativeJsUnsupportedSchedule {
                                path: path.to_string(),
                                reason: if expression
                                    .as_deref()
                                    .is_none_or(|item| !valid_cron_expression(item))
                                {
                                    "non-literal-or-unsupported-cron-expression".to_string()
                                } else {
                                    "task-is-not-an-unshadowed-identifier".to_string()
                                },
                                evidence: Some(evidence(path, source, node)),
                            });
                    }
                }
            }
        }
        _ => {}
    }
    for child in named_children(node) {
        collect_structural(
            child,
            input,
            bindings,
            cron_receivers,
            fastify_receivers,
            outputs,
        );
    }
}

fn typescript_tolerates_tree_sitter_error(node: Node<'_>, source: &str) -> bool {
    if node.kind() != "ERROR" {
        return false;
    }
    let text = source_text(node, source)
        .unwrap_or_default()
        .trim()
        .to_string();
    let parent = node.parent();
    // TypeScript accepts `in` as a JSX attribute name, while the JavaScript
    // grammar can recover through an ERROR node because `in` is also an
    // expression keyword. The oracle reports no parse diagnostic here.
    if text == "in=" && parent.is_some_and(|item| item.kind() == "jsx_opening_element") {
        return true;
    }
    // A bare ampersand in JSX text is accepted by TypeScript's source parser.
    // tree-sitter-javascript reports an ERROR while recovering this text.
    text.starts_with('&') && parent.is_some_and(|item| item.kind() == "jsx_element")
}

fn diagnostic_count(node: Node<'_>, source: &str) -> usize {
    if !node.has_error() {
        return 0;
    }
    let mut count = 0;
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        count += usize::from(
            (current.is_error() || current.is_missing())
                && !typescript_tolerates_tree_sitter_error(current, source),
        );
        let mut cursor = current.walk();
        stack.extend(current.named_children(&mut cursor));
    }
    count
}

fn structural_facts(
    path: &str,
    source: &str,
    root: Node<'_>,
    legacy_imports: &mut BTreeSet<String>,
    legacy_symbols: &mut BTreeSet<(String, String)>,
    legacy_calls: &mut BTreeSet<String>,
) -> NativeJsStructuralFacts {
    let diagnostics = diagnostic_count(root, source);
    let mut bindings = BTreeMap::new();
    let mut cron_receivers = BTreeSet::new();
    let mut fastify_factories = BTreeSet::new();
    collect_bindings(
        root,
        source,
        &mut bindings,
        &mut cron_receivers,
        &mut fastify_factories,
    );
    let mut fastify_receivers = BTreeSet::new();
    fn collect_fastify_instances(
        node: Node<'_>,
        source: &str,
        factories: &BTreeSet<String>,
        output: &mut BTreeSet<String>,
    ) {
        if node.kind() == "variable_declarator"
            && let Some(name) = node
                .child_by_field_name("name")
                .filter(|item| item.kind() == "identifier")
                .and_then(|item| source_text(item, source))
            && let Some(value) = node
                .child_by_field_name("value")
                .filter(|item| item.kind() == "call_expression")
            && let Some(function) = value
                .child_by_field_name("function")
                .filter(|item| item.kind() == "identifier")
                .and_then(|item| source_text(item, source))
            && factories.contains(&function)
        {
            output.insert(name.to_ascii_lowercase());
        }
        for child in named_children(node) {
            collect_fastify_instances(child, source, factories, output);
        }
    }
    collect_fastify_instances(root, source, &fastify_factories, &mut fastify_receivers);
    let mut facts = NativeJsStructuralFacts {
        imports: Vec::new(),
        symbols: Vec::new(),
        canonical_symbols: Vec::new(),
        calls: Vec::new(),
        endpoints: Vec::new(),
        requests: Vec::new(),
        integrations: Vec::new(),
        framework_commands: Vec::new(),
        unsupported_framework_commands: Vec::new(),
        runtime_actions: Vec::new(),
        schedules: Vec::new(),
        unsupported_schedules: Vec::new(),
        methods: Vec::new(),
        analysis: NativeJsAnalysis {
            parser: "typescript-ast".to_string(),
            status: if diagnostics > 0 {
                "parsed-with-diagnostics"
            } else {
                "parsed"
            }
            .to_string(),
            confidence: "exact".to_string(),
            diagnostics,
            reason: None,
        },
    };
    let input = StructuralInput { path, source };
    let mut outputs = StructuralOutputs {
        legacy_imports,
        legacy_symbols,
        legacy_calls,
        facts: &mut facts,
    };
    collect_structural(
        root,
        &input,
        &bindings,
        &cron_receivers,
        &fastify_receivers,
        &mut outputs,
    );
    facts
}

pub fn parse_native_js_facts(path: &str, source: &str) -> Option<NativeJsFacts> {
    if let Some(facts) = inventory_only_native_facts(path) {
        return Some(facts);
    }
    if path.rsplit('.').next()?.eq_ignore_ascii_case("cs") {
        return crate::csharp_facts::parse_native_csharp_facts(path, source);
    }
    if path.rsplit('.').next()?.eq_ignore_ascii_case("go") {
        return crate::go_facts::parse_native_go_facts(path, source);
    }
    if path.rsplit('.').next()?.eq_ignore_ascii_case("java") {
        return crate::java_facts::parse_native_java_facts(path, source);
    }
    if path.rsplit('.').next()?.eq_ignore_ascii_case("php") {
        return crate::php_facts::parse_native_php_facts(path, source);
    }
    if path.rsplit('.').next()?.eq_ignore_ascii_case("rs") {
        return crate::rust_facts::parse_native_rust_facts(path, source);
    }
    if path.rsplit('.').next()?.eq_ignore_ascii_case("svelte") {
        return crate::svelte_facts::parse_native_svelte_facts(path, source);
    }
    if path.rsplit('.').next()?.eq_ignore_ascii_case("py") {
        return crate::python_facts::parse_native_python_facts(path, source);
    }
    let language = language_for_path(path)?;
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return None;
    }
    parse_native_js_facts_with_parser(path, source, &mut parser)
}

/// Parse a JavaScript-family source with a caller-owned parser. Tree-sitter
/// language setup is immutable for the lifetime of a parser, so the scan
/// pipeline can reuse one parser per language across a bounded parser chunk
/// without changing the fact envelope or traversal behavior.
fn parse_native_js_facts_with_parser(
    path: &str,
    source: &str,
    parser: &mut Parser,
) -> Option<NativeJsFacts> {
    let tree = parser.parse(source, None)?;
    let mut imports = BTreeSet::new();
    let mut symbols = BTreeSet::new();
    let mut direct_calls = BTreeSet::new();
    let structural = structural_facts(
        path,
        source,
        tree.root_node(),
        &mut imports,
        &mut symbols,
        &mut direct_calls,
    );
    Some(NativeJsFacts {
        schema_version: NATIVE_JS_FACTS_SCHEMA.to_string(),
        parser: "tree-sitter".to_string(),
        status: if structural.analysis.diagnostics > 0 {
            "parsed-with-diagnostics".to_string()
        } else {
            "parsed".to_string()
        },
        diagnostics: structural.analysis.diagnostics,
        imports: imports.into_iter().collect(),
        symbols: symbols
            .into_iter()
            .map(|(kind, name)| NativeJsSymbol { kind, name })
            .collect(),
        direct_calls: direct_calls.into_iter().collect(),
        structural,
    })
}

pub fn scan_native_js_facts(input_root: &Path) -> Result<NativeJsFactsStatus, String> {
    let mut inventory = scan_native_inventory_with_paths(input_root)?;
    // The durable inventory hashes changed files while reading them. Move the
    // transient native source buffers into this scan so cold parsing does not
    // issue a second read for every cache miss. They are dropped after the
    // parser workers finish and never enter the persistent status/cache.
    let prefetched_sources = std::mem::take(&mut inventory.ephemeral_source_texts);
    let project_root = inventory.project_root.clone();
    let project_identity = inventory.project_identity.clone();
    let scope = read_native_scope(&project_root)?;
    let mut connection = open_native_store(&project_root).map_err(|error| error.to_string())?;
    let project_pk: i64 = connection
        .query_row(
            "SELECT project_pk FROM projects WHERE project_id = ?1",
            [project_identity.project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let candidates = {
        let mut statement = connection
            .prepare(
                "SELECT path, content_hash FROM inventory_files
                 WHERE project_pk = ?1 ORDER BY path",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map([project_pk], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|(path, _)| native_fact_supported_path(path))
            .collect::<Vec<_>>()
    };
    let mut known_records = {
        let mut statement = connection
            .prepare("SELECT path, source_scope FROM inventory_files WHERE project_pk = ?1 ORDER BY path")
            .map_err(|error| error.to_string())?;
        statement
            .query_map([project_pk], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    };
    known_records.sort_by(|left, right| js_locale_compare(&left.0, &right.0));
    let known_paths = known_records
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    let source_scopes = known_records.iter().cloned().collect::<BTreeMap<_, _>>();
    let record_orders = known_records
        .iter()
        .enumerate()
        .map(|(order, (path, _))| (path.clone(), order))
        .collect::<BTreeMap<_, _>>();
    // A warm repository used to perform one SQLite SELECT for every candidate
    // source file.  Load this adapter's complete cache once instead: the
    // source inventory already gives us the exact (path, hash) key for each
    // lookup, and the in-memory map preserves the same cache-hit contract.
    // This is intentionally a read-only preload; writes remain limited to
    // parser misses below so a refresh never rewrites reusable facts.
    let cached_facts = {
        let mut statement = connection
            .prepare(
                "SELECT path, source_hash, payload_json
                 FROM parser_facts
                 WHERE project_pk = ?1 AND adapter_version = ?2",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(params![project_pk, NATIVE_JS_ADAPTER_VERSION], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(path, source_hash, payload)| ((path, source_hash), payload))
            .collect::<BTreeMap<_, _>>()
    };
    // Parsing and cached-fact decoding are independent per source file. Keep
    // collection order deterministic by first collecting the indexed Rayon
    // results and merging them in candidate order below; SQLite remains a
    // single writer transaction after the parallel work completes.
    // Reuse one parser per language across bounded chunks. Constructing and
    // configuring a tree-sitter parser for every source file dominates cold
    // scans of repositories with hundreds of small files.
    let parser_chunk_size = 64;
    let parsed_candidates = with_native_parser_pool(|| {
        let cached_facts = &cached_facts;
        let project_root = &project_root;
        let prefetched_sources = &prefetched_sources;
        // Tree-sitter parsers retain their language tables between parses.
        // Reuse one parser per language within each Rayon chunk instead of
        // constructing and configuring one for every source file. This applies
        // to JavaScript/TypeScript as well as the Java and C# adapters.
        candidates
            .par_chunks(parser_chunk_size)
            .flat_map_iter(|chunk| {
                let mut javascript_parser = None;
                let mut typescript_parser = None;
                let mut tsx_parser = None;
                let mut java_parser = None;
                let mut csharp_parser = None;
                chunk.iter().map(move |(path, source_hash)| {
                    let cached = cached_facts
                        .get(&(path.clone(), source_hash.clone()))
                        .cloned();
                    if let Some(payload) = cached {
                        let fact = serde_json::from_str(&payload).map_err(|error| {
                            format!(
                                "Invalid cached native JavaScript parser fact for {path}: {error}"
                            )
                        })?;
                        return Ok((path.clone(), source_hash.clone(), fact, None));
                    }
                    let source_owned;
                    let source = if let Some(prefetched) = prefetched_sources.get(path) {
                        prefetched.as_str()
                    } else {
                        source_owned =
                            read_source_text(project_root.join(path)).map_err(|error| {
                                format!(
                                    "Unable to read JavaScript/TypeScript source {path}: {error}"
                                )
                            })?;
                        &source_owned
                    };
                    let extension = path
                        .rsplit('.')
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    let fact = match extension.as_str() {
                        "js" | "cjs" | "mjs" | "jsx" => {
                            let parser = javascript_parser.get_or_insert_with(|| {
                                let mut parser = tree_sitter::Parser::new();
                                parser
                                    .set_language(&tree_sitter_javascript::LANGUAGE.into())
                                    .expect("tree-sitter JavaScript language must be available");
                                parser
                            });
                            parse_native_js_facts_with_parser(path, source, parser)
                        }
                        "ts" => {
                            let parser = typescript_parser.get_or_insert_with(|| {
                                let mut parser = tree_sitter::Parser::new();
                                parser
                                    .set_language(
                                        &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                                    )
                                    .expect("tree-sitter TypeScript language must be available");
                                parser
                            });
                            parse_native_js_facts_with_parser(path, source, parser)
                        }
                        "tsx" => {
                            let parser = tsx_parser.get_or_insert_with(|| {
                                let mut parser = tree_sitter::Parser::new();
                                parser
                                    .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
                                    .expect("tree-sitter TSX language must be available");
                                parser
                            });
                            parse_native_js_facts_with_parser(path, source, parser)
                        }
                        "java" => {
                            let parser = java_parser.get_or_insert_with(|| {
                                let mut parser = tree_sitter::Parser::new();
                                parser
                                    .set_language(&tree_sitter_java::LANGUAGE.into())
                                    .expect("tree-sitter Java language must be available");
                                parser
                            });
                            crate::java_facts::parse_native_java_facts_with_parser(
                                path, source, parser,
                            )
                        }
                        "cs" => {
                            let parser = csharp_parser.get_or_insert_with(|| {
                                let mut parser = tree_sitter::Parser::new();
                                parser
                                    .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
                                    .expect("tree-sitter C# language must be available");
                                parser
                            });
                            crate::csharp_facts::parse_native_csharp_facts_with_parser(
                                path, source, parser,
                            )
                        }
                        _ => parse_native_js_facts(path, source),
                    }
                    .ok_or_else(|| {
                        format!("No native JavaScript/TypeScript parser is registered for {path}.")
                    })?;
                    // Serialize parser facts only while the SQLite writer
                    // owns the cache transaction below. Keeping a JSON copy
                    // beside every typed fact doubled the cold-scan peak for
                    // large repositories.
                    Ok((path.clone(), source_hash.clone(), fact, Some(())))
                })
            })
            .collect::<Vec<Result<(String, String, NativeJsFacts, Option<()>), String>>>()
    })?;
    let mut parsed_files = 0;
    let mut reused_files = 0;
    let mut failed_files = 0;
    let mut facts = BTreeMap::new();
    let mut parser_cache_misses = Vec::new();
    for parsed in parsed_candidates {
        let (path, source_hash, fact, cache_miss) = parsed?;
        if cache_miss.is_some() {
            parsed_files += 1;
            parser_cache_misses.push((path.clone(), source_hash));
        } else {
            reused_files += 1;
        }
        if fact.status == "parse-failed" {
            failed_files += 1;
        }
        facts.insert(path, fact);
    }
    // Parser cache mutation is one transaction per scan, not two implicit
    // transactions per parsed file. This keeps the inventory's cache contract
    // intact while avoiding WAL/fsync amplification on a cold scan.
    let removed_facts = {
        // Multiple native processes can parse the same cold repository before
        // either one reaches this cache write.  Acquire the SQLite write
        // reservation before reading/writing parser_facts so the second
        // writer observes the first writer's rows instead of racing on the
        // unique (project, path, hash, adapter) key.  The cache is derived and
        // deterministic; an identical concurrent miss is therefore an
        // idempotent no-op, never a source-facts failure.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let mut delete_stale = transaction
            .prepare(
                "DELETE FROM parser_facts
                 WHERE project_pk = ?1 AND path = ?2 AND adapter_version = ?3 AND source_hash != ?4",
            )
            .map_err(|error| error.to_string())?;
        let mut insert_fact = transaction
            .prepare(
                "INSERT INTO parser_facts(project_pk, path, source_hash, adapter_version, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(project_pk, path, source_hash, adapter_version)
                 DO UPDATE SET payload_json = excluded.payload_json",
            )
            .map_err(|error| error.to_string())?;
        for (path, source_hash) in &parser_cache_misses {
            let fact = facts.get(path).ok_or_else(|| {
                format!("Native parser cache miss has no parsed fact for {path}.")
            })?;
            let payload = serde_json::to_string(fact).map_err(|error| error.to_string())?;
            delete_stale
                .execute(params![
                    project_pk,
                    path,
                    NATIVE_JS_ADAPTER_VERSION,
                    source_hash
                ])
                .map_err(|error| error.to_string())?;
            insert_fact
                .execute(params![
                    project_pk,
                    path,
                    source_hash,
                    NATIVE_JS_ADAPTER_VERSION,
                    payload
                ])
                .map_err(|error| error.to_string())?;
        }
        drop(delete_stale);
        drop(insert_fact);
        let removed = transaction
            .execute(
                "DELETE FROM parser_facts
                 WHERE project_pk = ?1 AND adapter_version = ?2
                   AND path NOT IN (SELECT path FROM inventory_files WHERE project_pk = ?1)",
                params![project_pk, NATIVE_JS_ADAPTER_VERSION],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        removed
    };
    let resolution = resolve_native_js_imports(&project_root, &facts, &known_paths);
    let (reverse_importers, importer_targets) = native_resolution_indexes(&resolution);
    let mut structural_records = build_native_js_structural_records(
        &project_root,
        &facts,
        &resolution,
        &source_scopes,
        &record_orders,
    )?;
    normalize_structural_record_orders(&mut structural_records);
    let entry_facts = build_native_js_entry_facts(&project_root, &facts, &structural_records);
    Ok(NativeJsFactsStatus {
        project_root,
        project_identity,
        initial_scan: true,
        adapter_version: NATIVE_JS_ADAPTER_VERSION.to_string(),
        parsed_files,
        reused_files,
        failed_files,
        removed_facts,
        candidate_files: inventory.candidate_files,
        candidate_paths: inventory.candidate_paths.unwrap_or_default(),
        changed_paths: Vec::new(),
        reused_paths: inventory.reused_paths,
        removed_paths: inventory.removed_paths,
        changed_record_paths: Vec::new(),
        source_scope_counts: inventory.source_scope_counts,
        scope_source: inventory.scope_source,
        flow_entries_tests: scope.flow_entries_tests,
        flow_entries_fixtures: scope.flow_entries_fixtures,
        promoted_facts_digest: None,
        structural_record_digests: BTreeMap::new(),
        facts,
        compacted_facts: BTreeMap::new(),
        source_hashes: structural_records
            .iter()
            .filter_map(|record| {
                Some((
                    record.get("relativePath")?.as_str()?.to_string(),
                    record.get("sourceHash")?.as_str()?.to_string(),
                ))
            })
            .collect(),
        resolution,
        reverse_importers,
        importer_targets,
        structural_records,
        structural_records_complete: true,
        structural_record_manifest: Vec::new(),
        entry_facts,
    })
}

fn parse_native_js_paths_parallel(
    project_root: &Path,
    paths: &[String],
    prefetched_sources: &BTreeMap<String, String>,
) -> Result<Vec<(String, String, NativeJsFacts)>, String> {
    let parsed =
        with_native_parser_pool(|| {
            paths
                .par_iter()
                .map(|path| {
                    let parse =
                        |source: &str| -> Result<(String, String, NativeJsFacts), String> {
                            let fact = parse_native_js_facts(path, source).ok_or_else(|| {
                        format!("No native JavaScript/TypeScript parser is registered for {path}.")
                    })?;
                            Ok((path.clone(), native_public_source_hash(source), fact))
                        };
                    match prefetched_sources.get(path) {
                        // The inventory already owns this text. Parse/hash it by reference so
                        // cold no-cache scans do not duplicate every source buffer per worker.
                        Some(source) => parse(source),
                        None => {
                            let source = read_source_text(project_root.join(path)).map_err(|error| {
                            format!("Unable to read JavaScript/TypeScript source {path}: {error}")
                        })?;
                            parse(&source)
                        }
                    }
                })
                .collect::<Vec<Result<(String, String, NativeJsFacts), String>>>()
        })?;
    parsed.into_iter().collect()
}

// Strict native --no-cache path. It deliberately bypasses both the inventory
// store and parser-fact cache: no `.flopeek` files, SQLite journal, or durable
// identity may be created by a one-shot/session-memory scan.
pub fn scan_native_js_facts_ephemeral(
    input_root: &Path,
    session_project_id: Option<&str>,
) -> Result<NativeJsFactsStatus, String> {
    let inventory = scan_native_ephemeral_inventory_with_paths(input_root, session_project_id)?;
    let project_root = inventory.project_root.clone();
    let project_identity = inventory.project_identity.clone();
    let scope = read_native_scope(&project_root)?;
    let mut known_records = inventory
        .candidate_paths
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|path| {
            let source_scope = scope.classify(&path).as_str().to_string();
            (path, source_scope)
        })
        .collect::<Vec<_>>();
    known_records.sort_by(|left, right| js_locale_compare(&left.0, &right.0));
    let known_paths = known_records
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    let source_scopes = known_records.iter().cloned().collect::<BTreeMap<_, _>>();
    let record_orders = known_records
        .iter()
        .enumerate()
        .map(|(order, (path, _))| (path.clone(), order))
        .collect::<BTreeMap<_, _>>();
    let mut failed_files = 0;
    let mut facts = BTreeMap::new();
    let mut source_hashes = BTreeMap::new();
    let source_paths = known_paths
        .iter()
        .filter(|path| native_fact_supported_path(path))
        .cloned()
        .collect::<Vec<_>>();
    let parsed_sources = parse_native_js_paths_parallel(
        &project_root,
        &source_paths,
        &inventory.ephemeral_source_texts,
    )?;
    for (path, source_hash, fact) in parsed_sources {
        source_hashes.insert(path.clone(), source_hash);
        if fact.status == "parse-failed" {
            failed_files += 1;
        }
        facts.insert(path, fact);
    }
    let resolution = resolve_native_js_imports(&project_root, &facts, &known_paths);
    let (reverse_importers, importer_targets) = native_resolution_indexes(&resolution);
    let mut structural_records = build_native_js_structural_records_with_source_hashes(
        &project_root,
        &facts,
        &resolution,
        &source_scopes,
        &record_orders,
        Some(&source_hashes),
    )?;
    normalize_structural_record_orders(&mut structural_records);
    let entry_facts = build_native_js_entry_facts(&project_root, &facts, &structural_records);
    Ok(NativeJsFactsStatus {
        project_root,
        project_identity,
        initial_scan: true,
        adapter_version: NATIVE_JS_ADAPTER_VERSION.to_string(),
        parsed_files: source_paths.len(),
        reused_files: 0,
        failed_files,
        removed_facts: 0,
        candidate_files: inventory.candidate_files,
        candidate_paths: inventory.candidate_paths.unwrap_or_default(),
        changed_paths: Vec::new(),
        reused_paths: Vec::new(),
        removed_paths: Vec::new(),
        changed_record_paths: Vec::new(),
        source_scope_counts: inventory.source_scope_counts,
        scope_source: inventory.scope_source,
        flow_entries_tests: scope.flow_entries_tests,
        flow_entries_fixtures: scope.flow_entries_fixtures,
        promoted_facts_digest: None,
        structural_record_digests: BTreeMap::new(),
        facts,
        compacted_facts: BTreeMap::new(),
        source_hashes,
        resolution,
        reverse_importers,
        importer_targets,
        structural_records,
        structural_records_complete: true,
        structural_record_manifest: Vec::new(),
        entry_facts,
    })
}

/// Execute only a previously native-discovered bounded source set. This path
/// never opens SQLite and keeps package scans session-only until a complete,
/// verified plan is assembled by the protocol lifecycle.
pub fn scan_native_js_facts_ephemeral_bounded(
    input_root: &Path,
    session_project_id: Option<&str>,
    package_path: Option<&str>,
    max_files: Option<usize>,
    max_bytes: Option<i64>,
    budget_ms: Option<u64>,
) -> Result<(NativeJsFactsStatus, NativeBoundedDiscovery), String> {
    let discovery =
        discover_native_bounded_project(input_root, package_path, max_files, max_bytes, budget_ms)?;
    let project_root = discovery.project_root.clone();
    let scope = read_native_scope(&project_root)?;
    let project_identity =
        resolve_ephemeral_project_identity(scope.project_id.as_deref(), session_project_id)?;
    let mut known_records = discovery
        .candidates
        .iter()
        .map(|candidate| (candidate.path.clone(), candidate.source_scope.clone()))
        .collect::<Vec<_>>();
    known_records.sort_by(|left, right| js_locale_compare(&left.0, &right.0));
    let known_paths = known_records
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    let source_scopes = known_records.iter().cloned().collect::<BTreeMap<_, _>>();
    let record_orders = known_records
        .iter()
        .enumerate()
        .map(|(order, (path, _))| (path.clone(), order))
        .collect::<BTreeMap<_, _>>();
    let mut failed_files = 0;
    let mut facts = BTreeMap::new();
    let mut source_hashes = BTreeMap::new();
    let source_paths = known_paths
        .iter()
        .filter(|path| native_fact_supported_path(path))
        .cloned()
        .collect::<Vec<_>>();
    let prefetched_sources = BTreeMap::new();
    let parsed_sources =
        parse_native_js_paths_parallel(&project_root, &source_paths, &prefetched_sources)?;
    for (path, source_hash, fact) in parsed_sources {
        source_hashes.insert(path.clone(), source_hash);
        if fact.status == "parse-failed" {
            failed_files += 1;
        }
        facts.insert(path, fact);
    }
    let resolution = resolve_native_js_imports(&project_root, &facts, &known_paths);
    let (reverse_importers, importer_targets) = native_resolution_indexes(&resolution);
    let mut structural_records = build_native_js_structural_records_with_source_hashes(
        &project_root,
        &facts,
        &resolution,
        &source_scopes,
        &record_orders,
        Some(&source_hashes),
    )?;
    normalize_structural_record_orders(&mut structural_records);
    let allowed_manifests = discovery
        .package_path
        .as_ref()
        .map(|package_path| BTreeSet::from([format!("{package_path}/package.json")]));
    let entry_facts = build_native_js_entry_facts_for_manifests(
        &project_root,
        &facts,
        &structural_records,
        allowed_manifests.as_ref(),
    );
    let mut source_scope_counts = ["application", "test", "fixture", "generated", "excluded"]
        .into_iter()
        .map(|name| (name.to_string(), 0usize))
        .collect::<BTreeMap<_, _>>();
    for (_, source_scope) in known_records {
        *source_scope_counts.entry(source_scope).or_default() += 1;
    }
    let candidate_paths = discovery
        .candidates
        .iter()
        .map(|candidate| candidate.path.clone())
        .collect::<Vec<_>>();
    let status = NativeJsFactsStatus {
        project_root,
        project_identity,
        initial_scan: true,
        adapter_version: NATIVE_JS_ADAPTER_VERSION.to_string(),
        parsed_files: source_paths.len(),
        reused_files: 0,
        failed_files,
        removed_facts: 0,
        candidate_files: candidate_paths.len(),
        candidate_paths,
        changed_paths: Vec::new(),
        reused_paths: Vec::new(),
        removed_paths: Vec::new(),
        changed_record_paths: Vec::new(),
        source_scope_counts,
        scope_source: scope.source,
        flow_entries_tests: scope.flow_entries_tests,
        flow_entries_fixtures: scope.flow_entries_fixtures,
        promoted_facts_digest: None,
        structural_record_digests: BTreeMap::new(),
        facts,
        compacted_facts: BTreeMap::new(),
        source_hashes,
        resolution,
        reverse_importers,
        importer_targets,
        structural_records,
        structural_records_complete: true,
        structural_record_manifest: Vec::new(),
        entry_facts,
    };
    Ok((status, discovery))
}

#[cfg(test)]
mod tests {
    use super::{
        js_locale_compare, parse_native_js_facts, parse_native_js_facts_with_parser,
        refresh_native_js_facts_session, scan_native_js_facts, scan_native_js_facts_ephemeral,
    };
    use crate::js_batch::native_public_source_hash;
    use std::fs;

    #[test]
    fn extracts_bounded_javascript_and_typescript_structural_facts() {
        let javascript = parse_native_js_facts(
            "src/orders.js",
            "import { normalize } from './normalize'; const legacy = require('./legacy'); export const submit = () => normalize(); class Order {}",
        )
        .unwrap();
        assert_eq!(javascript.imports, vec!["./legacy", "./normalize"]);
        assert!(
            javascript
                .symbols
                .iter()
                .any(|symbol| symbol.kind == "function" && symbol.name == "submit")
        );
        assert!(
            javascript
                .symbols
                .iter()
                .any(|symbol| symbol.kind == "class" && symbol.name == "Order")
        );
        assert_eq!(javascript.direct_calls, vec!["normalize"]);

        let method_calls = parse_native_js_facts(
            "src/http.ts",
            "router.post('/orders', () => response.json()); submitOrder();",
        )
        .unwrap();
        assert_eq!(method_calls.direct_calls, vec!["submitOrder"]);

        let typescript = parse_native_js_facts(
            "src/model.ts",
            "export interface Order { id: string } export function load() { return fetch('/orders'); }",
        )
        .unwrap();
        assert!(
            typescript
                .symbols
                .iter()
                .any(|symbol| symbol.kind == "type" && symbol.name == "Order")
        );
        assert!(typescript.direct_calls.contains(&"fetch".to_string()));

        let declarations = parse_native_js_facts(
            "index.d.ts",
            "export default function pLimit(concurrency: number): void;\nexport function limitFunction(): void;\n",
        )
        .unwrap();
        assert_eq!(
            declarations
                .symbols
                .iter()
                .filter(|symbol| symbol.kind == "function")
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["limitFunction", "pLimit"]
        );
        assert_eq!(
            declarations.structural.methods,
            vec!["pLimit", "limitFunction"]
        );
    }

    #[test]
    fn reused_tree_sitter_parsers_preserve_facts_for_javascript_and_typescript() {
        let javascript_source =
            "import { normalize } from './normalize'; export const submit = () => normalize();";
        let typescript_source = "export interface Order { id: string } export function load() { return fetch('/orders'); }";

        let javascript_fresh = parse_native_js_facts("src/orders.js", javascript_source).unwrap();
        let typescript_fresh = parse_native_js_facts("src/model.ts", typescript_source).unwrap();

        let mut javascript_parser = tree_sitter::Parser::new();
        javascript_parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .unwrap();
        let mut typescript_parser = tree_sitter::Parser::new();
        typescript_parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();

        let javascript_reused = parse_native_js_facts_with_parser(
            "src/orders.js",
            javascript_source,
            &mut javascript_parser,
        )
        .unwrap();
        let typescript_reused = parse_native_js_facts_with_parser(
            "src/model.ts",
            typescript_source,
            &mut typescript_parser,
        )
        .unwrap();

        assert_eq!(javascript_reused, javascript_fresh);
        assert_eq!(typescript_reused, typescript_fresh);

        let javascript_second = parse_native_js_facts_with_parser(
            "src/orders-second.js",
            "export const cancel = () => normalize();",
            &mut javascript_parser,
        )
        .unwrap();
        let javascript_second_fresh = parse_native_js_facts(
            "src/orders-second.js",
            "export const cancel = () => normalize();",
        )
        .unwrap();
        assert_eq!(javascript_second, javascript_second_fresh);
    }

    #[test]
    fn canonical_typescript_symbols_preserve_owner_and_overload_signatures() {
        let facts = parse_native_js_facts(
            "src/OrderService.ts",
            "class OrderService { save(order: Order): void; save(order: Order, user: User): void; save(order: Order, user?: User): void {} }\nclass AuditService { save(order: Order): void {} }",
        )
        .unwrap();
        let save_identities = facts
            .structural
            .canonical_symbols
            .iter()
            .filter(|symbol| symbol.name == "save")
            .filter_map(|symbol| symbol.identity.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(save_identities.len(), 4);
        assert_eq!(save_identities[0].qualified_name, "OrderService.save");
        assert_eq!(
            save_identities[0].signature.as_deref(),
            Some("(Order):void")
        );
        assert_eq!(
            save_identities[1].signature.as_deref(),
            Some("(Order,User):void")
        );
        assert_eq!(save_identities[3].qualified_name, "AuditService.save");
    }

    #[test]
    fn preserves_typescript_positions_and_jsx_diagnostic_compatibility() {
        let bom =
            parse_native_js_facts("src/client.js", "\u{feff}import axios from \"axios\";").unwrap();
        let import = &bom.structural.imports[0];
        assert_eq!(import.evidence.range.start.column, 2);
        assert_eq!(import.evidence.range.end.column, 28);

        let jsx = parse_native_js_facts(
            "src/view.jsx",
            "const View = () => <div in={value}>Password & Data Name</div>;",
        )
        .unwrap();
        assert_eq!(jsx.structural.analysis.diagnostics, 0);
        assert_eq!(jsx.structural.analysis.status, "parsed");
    }

    #[test]
    fn preserves_registered_unparsed_files_as_inventory_only_facts() {
        let shell =
            parse_native_js_facts("scripts/deploy.sh", "#!/usr/bin/env bash\necho deploy\n")
                .expect("shell files must retain their registered inventory record");
        assert_eq!(shell.status, "inventory-only");
        assert_eq!(shell.structural.analysis.parser, "inventory");
        assert_eq!(shell.structural.analysis.confidence, "not-analyzed");
        assert_eq!(
            shell.structural.analysis.reason.as_deref(),
            Some("No structural adapter registered for .sh.")
        );
        assert!(shell.structural.imports.is_empty());

        let makefile = parse_native_js_facts("ops/Makefile", "build:\n\techo build\n")
            .expect("Makefile must retain its registered inventory record");
        assert_eq!(makefile.status, "inventory-only");
        assert_eq!(
            makefile.structural.analysis.reason.as_deref(),
            Some("No structural adapter registered for Makefile build-control files.")
        );
        let go = parse_native_js_facts("cmd/server.go", "package main\nfunc main() {}\n")
            .expect("Go files must use the bundled strict-native parser");
        assert_eq!(go.parser, "go-parser");
        assert_eq!(go.structural.symbols[0].name, "main");
    }

    #[test]
    fn excludes_constructors_and_only_attributes_calls_to_function_variables() {
        let facts = parse_native_js_facts(
            "src/client.js",
            "const { publicRuntimeConfig } = getConfig(); class Client { constructor() {} run() {} }",
        )
        .unwrap();
        assert_eq!(facts.structural.calls[0].source, None);
        assert_eq!(facts.structural.symbols[0].methods, vec!["run"]);
        assert_eq!(facts.structural.methods, vec!["run"]);
    }

    #[test]
    fn matches_javascript_locale_path_order_for_ascii_paths() {
        let mut paths = vec![
            "pages/404.js",
            "pages/_document.js",
            "pages/_app.js",
            "src/content/MasterService/ContentList.js",
            "src/content/MasterService/ContentList_table_fix_filter_not.js",
            "src/content/MasterService/ContentList_multil_table_cell.js",
            "api/services/auth.service.js",
            "api/services/axios.service.js",
            "api/services/AppCheck/AppCheckService.js",
            "pages/datalake/connections/[detail].js",
            "pages/datalake/connections/create.js",
            "src/utils/firebase.js",
            "src/utils/firebase copy.js",
        ];
        paths.sort_by(|left, right| js_locale_compare(left, right));
        assert_eq!(
            paths,
            vec![
                "api/services/AppCheck/AppCheckService.js",
                "api/services/auth.service.js",
                "api/services/axios.service.js",
                "pages/_app.js",
                "pages/_document.js",
                "pages/404.js",
                "pages/datalake/connections/[detail].js",
                "pages/datalake/connections/create.js",
                "src/content/MasterService/ContentList_multil_table_cell.js",
                "src/content/MasterService/ContentList_table_fix_filter_not.js",
                "src/content/MasterService/ContentList.js",
                "src/utils/firebase copy.js",
                "src/utils/firebase.js",
            ]
        );
    }

    #[test]
    fn caches_native_javascript_facts_by_blake3_source_identity() {
        let root =
            std::env::temp_dir().join(format!("flopeek-native-js-facts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/index.ts"),
            "export const run = () => fetch('/health');\n",
        )
        .unwrap();
        let first = scan_native_js_facts(&root).unwrap();
        assert_eq!(first.parsed_files, 1);
        assert_eq!(first.reused_files, 0);
        let second = scan_native_js_facts(&root).unwrap();
        assert_eq!(second.parsed_files, 0);
        assert_eq!(second.reused_files, 1);
        assert!(
            second.facts["src/index.ts"]
                .direct_calls
                .contains(&"fetch".to_string())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn strict_native_scan_keeps_malformed_utf8_source_as_a_parseable_record() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-malformed-source-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("application")).unwrap();
        fs::write(
            root.join("application/legacy.php"),
            b"<?php \x80 function legacy() { return true; }\n",
        )
        .unwrap();

        let status = scan_native_js_facts(&root).unwrap();
        assert_eq!(status.parsed_files, 1);
        assert_eq!(status.failed_files, 0);
        assert_eq!(
            status.facts["application/legacy.php"].parser,
            "tree-sitter-php"
        );
        assert_eq!(
            status.source_hashes["application/legacy.php"],
            native_public_source_hash("<?php \u{fffd} function legacy() { return true; }\n")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ephemeral_scan_keeps_inventory_and_parser_facts_out_of_the_repository() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-js-ephemeral-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/index.ts"), "export function run() {}\n").unwrap();
        let status = scan_native_js_facts_ephemeral(&root, Some("session:test-ephemeral")).unwrap();
        assert_eq!(status.parsed_files, 1);
        assert_eq!(status.reused_files, 0);
        assert_eq!(status.project_identity.source, "session");
        assert!(!root.join(".flopeek").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ephemeral_scan_keeps_registered_inventory_only_files_in_the_public_batch() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-inventory-only-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(
            root.join("src/index.ts"),
            "export const run = () => true;\n",
        )
        .unwrap();
        fs::write(
            root.join("scripts/deploy.sh"),
            "#!/usr/bin/env bash\necho deploy\n",
        )
        .unwrap();
        fs::write(root.join("Makefile"), "build:\n\techo build\n").unwrap();
        let status = scan_native_js_facts_ephemeral(&root, Some("session:inventory-only")).unwrap();
        assert_eq!(status.candidate_files, 3);
        assert_eq!(status.structural_records.len(), 3);
        assert_eq!(
            status.facts["scripts/deploy.sh"].structural.analysis.status,
            "inventory-only"
        );
        assert_eq!(
            status.facts["Makefile"]
                .structural
                .analysis
                .reason
                .as_deref(),
            Some("No structural adapter registered for Makefile build-control files.")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_session_ignores_events_outside_the_initial_inventory_contract() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-js-ignored-event-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/main.ts"),
            "export const main = () => true;\n",
        )
        .unwrap();
        let initial = scan_native_js_facts_ephemeral(&root, Some("session:ignored-event")).unwrap();
        fs::create_dir_all(root.join("node_modules/injected")).unwrap();
        fs::write(
            root.join("node_modules/injected/index.ts"),
            "export const injected = () => false;\n",
        )
        .unwrap();
        let refreshed = refresh_native_js_facts_session(
            &initial,
            &["node_modules/injected/index.ts".to_string()],
        )
        .unwrap();
        assert_eq!(refreshed.parsed_files, 0);
        assert_eq!(refreshed.changed_paths, Vec::<String>::new());
        assert_eq!(refreshed.candidate_paths, vec!["src/main.ts"]);
        assert!(
            !refreshed
                .facts
                .contains_key("node_modules/injected/index.ts")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_session_reuses_manifest_entries_for_an_ordinary_source_edit() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-js-entry-reuse-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"scripts":{"start":"node src/main.ts"}}"#,
        )
        .unwrap();
        fs::write(root.join("src/main.ts"), "export const stable = true;\n").unwrap();
        let initial = scan_native_js_facts_ephemeral(&root, Some("session:entry-reuse")).unwrap();
        assert_eq!(
            initial.entry_facts["entryPoints"]["supported"]["packageScripts"][0]["id"],
            "command:package.json:start"
        );
        // The unreported manifest deletion is intentionally not part of this
        // event contract. A subsequent package/config event will reconcile;
        // this source-only event must not re-walk manifests or lose the last
        // complete entry projection.
        fs::remove_file(root.join("package.json")).unwrap();
        fs::write(root.join("src/main.ts"), "export const changed = true;\n").unwrap();
        let refreshed =
            refresh_native_js_facts_session(&initial, &["src/main.ts".to_string()]).unwrap();
        assert_eq!(refreshed.parsed_files, 1);
        assert_eq!(refreshed.entry_facts, initial.entry_facts);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_source_edit_preserves_excluded_inventory_count() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-js-excluded-count-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("generated")).unwrap();
        fs::create_dir_all(root.join(".flopeek")).unwrap();
        fs::write(
            root.join(".flopeek/config.json"),
            r#"{"schemaVersion":1,"exclude":["generated/**"]}"#,
        )
        .unwrap();
        fs::write(root.join("src/main.ts"), "export const stable = true;\n").unwrap();
        fs::write(
            root.join("generated/client.ts"),
            "export const generated = true;\n",
        )
        .unwrap();
        let initial =
            scan_native_js_facts_ephemeral(&root, Some("session:excluded-count")).unwrap();
        assert_eq!(initial.source_scope_counts.get("excluded"), Some(&1));
        fs::write(root.join("src/main.ts"), "export const changed = true;\n").unwrap();
        let refreshed =
            refresh_native_js_facts_session(&initial, &["src/main.ts".to_string()]).unwrap();
        assert_eq!(refreshed.source_scope_counts.get("excluded"), Some(&1));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn excluded_membership_events_require_immediate_reconciliation() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-js-excluded-membership-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("generated")).unwrap();
        fs::create_dir_all(root.join(".flopeek")).unwrap();
        fs::write(
            root.join(".flopeek/config.json"),
            r#"{"schemaVersion":1,"exclude":["generated/**"]}"#,
        )
        .unwrap();
        fs::write(root.join("src/main.ts"), "export const stable = true;\n").unwrap();
        let initial =
            scan_native_js_facts_ephemeral(&root, Some("session:excluded-membership")).unwrap();
        fs::write(
            root.join("generated/new-client.ts"),
            "export const generated = true;\n",
        )
        .unwrap();
        let added =
            refresh_native_js_facts_session(&initial, &["generated/new-client.ts".to_string()])
                .unwrap_err();
        assert_eq!(
            added,
            "native-session-reconcile-required:generated/new-client.ts"
        );
        fs::remove_file(root.join("generated/new-client.ts")).unwrap();
        let removed =
            refresh_native_js_facts_session(&initial, &["generated/new-client.ts".to_string()])
                .unwrap_err();
        assert_eq!(
            removed,
            "native-session-reconcile-required:generated/new-client.ts"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_session_marks_reverse_importers_for_selective_record_writes() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-js-reverse-importers-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/dependency.ts"),
            "export const dependency = true;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/importer.ts"),
            "import { dependency } from './dependency'; export const value = dependency;\n",
        )
        .unwrap();
        let initial =
            scan_native_js_facts_ephemeral(&root, Some("session:reverse-importers")).unwrap();
        fs::remove_file(root.join("src/dependency.ts")).unwrap();
        let refreshed =
            refresh_native_js_facts_session(&initial, &["src/dependency.ts".to_string()]).unwrap();
        assert_eq!(
            refreshed.changed_record_paths,
            vec!["src/dependency.ts", "src/importer.ts"]
        );
        assert!(
            refreshed
                .resolution
                .get("src/importer.ts")
                .is_some_and(|facts| facts.resolved_imports.is_empty())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_session_replaces_only_the_changed_importers_reverse_targets() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-js-reverse-index-replace-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/first.ts"), "export const first = true;\n").unwrap();
        fs::write(root.join("src/second.ts"), "export const second = true;\n").unwrap();
        fs::write(
            root.join("src/importer.ts"),
            "import { first } from './first'; export const value = first;\n",
        )
        .unwrap();
        let initial =
            scan_native_js_facts_ephemeral(&root, Some("session:reverse-index-replace")).unwrap();
        assert_eq!(
            initial.reverse_importers["src/first.ts"]
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["src/importer.ts"]
        );
        fs::write(
            root.join("src/importer.ts"),
            "import { second } from './second'; export const value = second;\n",
        )
        .unwrap();
        let refreshed =
            refresh_native_js_facts_session(&initial, &["src/importer.ts".to_string()]).unwrap();
        assert!(!refreshed.reverse_importers.contains_key("src/first.ts"));
        assert_eq!(
            refreshed.reverse_importers["src/second.ts"]
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["src/importer.ts"]
        );
        assert_eq!(
            refreshed.importer_targets["src/importer.ts"]
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["src/second.ts"]
        );
        assert_eq!(refreshed.changed_record_paths, vec!["src/importer.ts"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_session_marks_go_multi_file_package_importers_as_changed_records() {
        let root = std::env::temp_dir().join(format!(
            "flopeek-native-go-package-reverse-importers-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cmd")).unwrap();
        fs::create_dir_all(root.join("pkg/helper")).unwrap();
        fs::write(
            root.join("go.mod"),
            "module example.test/reverse\n\ngo 1.26\n",
        )
        .unwrap();
        fs::write(
            root.join("cmd/main.go"),
            "package main\nimport \"example.test/reverse/pkg/helper\"\nfunc main() { helper.Ping() }\n",
        )
        .unwrap();
        fs::write(
            root.join("pkg/helper/a.go"),
            "package helper\nfunc Ping() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("pkg/helper/b.go"),
            "package helper\nfunc Stable() {}\n",
        )
        .unwrap();

        let initial =
            scan_native_js_facts_ephemeral(&root, Some("session:go-package-reverse")).unwrap();
        for target in ["pkg/helper/a.go", "pkg/helper/b.go"] {
            assert_eq!(
                initial.reverse_importers[target]
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec!["cmd/main.go"],
                "{target} must retain the package importer"
            );
        }

        fs::write(
            root.join("pkg/helper/a.go"),
            "package helper\nfunc Pong() {}\n",
        )
        .unwrap();
        let refreshed =
            refresh_native_js_facts_session(&initial, &["pkg/helper/a.go".to_string()]).unwrap();
        assert_eq!(
            refreshed.changed_record_paths,
            vec!["cmd/main.go", "pkg/helper/a.go"]
        );
        assert_eq!(refreshed.parsed_files, 1);
        assert_eq!(refreshed.reused_files, 2);

        fs::write(
            root.join("go.mod"),
            "module example.test/renamed\n\ngo 1.26\n",
        )
        .unwrap();
        let reconciled =
            refresh_native_js_facts_session(&refreshed, &["go.mod".to_string()]).unwrap();
        assert_eq!(reconciled.parsed_files, 0);
        assert_eq!(reconciled.reused_files, 3);
        assert_eq!(reconciled.changed_paths, vec!["go.mod"]);
        assert_eq!(
            reconciled.changed_record_paths,
            vec!["cmd/main.go", "pkg/helper/a.go", "pkg/helper/b.go"]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
