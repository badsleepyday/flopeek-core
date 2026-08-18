#!/usr/bin/env node

const path = require("node:path");
const fs = require("node:fs");
const { execFile } = require("node:child_process");
const packageInfo = require("../package.json");
const { runMcpServer } = require("./mcp");
const { createScanCoordinator } = require("./scan-coordinator");
const { startServer } = require("./server");
const { benchmarkRepository, printBenchmark } = require("./benchmark");
const { createProductProof, printProductProof } = require("./product-proof");
const { compareGitSnapshots, createGitSnapshot } = require("./history");
const { createContinuationCheckpoint, createPlannedOverlay, getAgentBootstrap, getActiveBranchGitEvidence, getChangeImpact, getContinuationCheckpoint, getGitContextContinuity, getPlanReconciliation, getPlannedOverlay, getRelatedImplementations, getWorkDependencyStatus, getWorkTimeline, listContinuationCheckpoints, listPlanReconciliations, listPlannedOverlays, listWorkRecords, projectView, recordPlanReconciliation, resolvePlanRef } = require("./graph-service");
const { listWorkflows: listStoredWorkflows } = require("./workflow-engine");
const { doctorAgentIntegration, installAgentIntegration, uninstallAgentIntegration } = require("./agent-integration");
const { agentComparisonSummary, evaluateAgentComparison, loadAgentComparisonRuns } = require("./agent-comparison");
const { evaluateOrientation, loadOrientationCases, orientationSummary } = require("./orientation-benchmark");
const { applyShowcaseChange, printShowcase, resetShowcase, showcasePublicResult, showcaseStatus, startShowcase } = require("./showcase");
const { getGitChangedPaths, graphToMermaid, scanRepository, writeGraphCache } = require("./scanner");
const { readGraphCacheResult, summarizeCacheResult } = require("./graph-cache");
const { readGitMetadata } = require("./git-metadata");
const { cacheHygiene, pruneArtifactCache } = require("./artifact-cache");
const { pruneGraphDeltas } = require("./graph-state");
const { discoverRepository } = require("./repository-discovery");
const { activateOnWorkspaceHub, startWorkspaceServer } = require("./workspace-server");
const { createSurfaceCoreRuntime, observeCoreRuntime } = require("./core-runtime");

let core = null;
let coreRuntime = null;

function parseArgs(argv) {
  const result = { command: "serve", evaluation: null, cacheAction: "status", showcaseAction: "run", workAction: "list", continueAction: "list", planAction: "list", reconcileAction: "list", recordId: null, checkpointId: null, overlayId: null, reconciliationId: null, planRef: null, contextRef: null, inputFile: null, root: process.cwd(), port: 4780, portFallback: true, global: false, workspaceId: null, serviceLabel: null, open: true, cache: true, format: "summary", changed: [], base: null, commit: "HEAD", from: "HEAD~1", to: "HEAD", fromVersion: null, toVersion: null, force: false, limit: null, keepDeltas: null, history: false, apply: false, iterations: 3, platforms: [], dryRun: false, strict: false, casesFile: null, runsFile: null, condition: "both", keepWorkspace: false, timeBudgetMs: null, maxFiles: null, maxBytes: null, packagePath: null, coreMode: null, mode: "overview", scope: "application", level: "feature", focus: null, maxNodes: null, maxEdges: null };
  const values = [...argv];
  if (["discover", "scan", "view", "impact", "snapshot", "history", "git-evidence", "git-continuity", "related-implementations", "delta", "benchmark", "proof", "evaluate", "cache", "showcase", "work", "continue", "bootstrap", "install", "uninstall", "doctor", "serve", "mcp", "help", "version", "--help", "-h", "--version", "-v"].includes(values[0])) result.command = values.shift().replace(/^--?/, "") || "help";
  if (result.command === "cache" && ["status", "prune"].includes(values[0])) result.cacheAction = values.shift();
  if (result.command === "evaluate" && ["orientation", "agent-comparison"].includes(values[0])) result.evaluation = values.shift();
  if (result.command === "showcase" && ["apply", "reset", "status"].includes(values[0])) result.showcaseAction = values.shift();
  if (result.command === "work" && ["list", "timeline", "workflows", "dependencies"].includes(values[0])) result.workAction = values.shift();
  if (result.command === "continue" && values[0] === "plan") {
    result.continueAction = values.shift();
    if (["list", "show", "create", "resolve"].includes(values[0])) result.planAction = values.shift();
  } else if (result.command === "continue" && values[0] === "reconcile") {
    result.continueAction = values.shift();
    if (["list", "show", "record"].includes(values[0])) result.reconcileAction = values.shift();
  } else if (result.command === "continue" && ["list", "show", "checkpoint"].includes(values[0])) result.continueAction = values.shift();
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (value === "--port") result.port = Number(values[++index] || result.port);
    else if (value === "--strict-port") result.portFallback = false;
    else if (value === "--global" || value === "-g") result.global = true;
    else if (value === "--workspace") result.workspaceId = values[++index] || null;
    else if (value === "--service-label") result.serviceLabel = values[++index] || null;
    else if (value === "--format") result.format = values[++index] || result.format;
    else if (value === "--json") result.format = "json";
    else if (value === "--mermaid") result.format = "mermaid";
    else if (value === "--no-cache") result.cache = false;
    else if (value === "--no-open") result.open = false;
    else if (value === "--changed") result.changed.push(...String(values[++index] || "").split(",").map((path) => path.trim()).filter(Boolean));
    else if (value === "--base") result.base = values[++index] || null;
    else if (value === "--commit") result.commit = values[++index] || result.commit;
    else if (value === "--from") result.from = values[++index] || result.from;
    else if (value === "--to") result.to = values[++index] || result.to;
    else if (value === "--from-version") result.fromVersion = Number(values[++index]);
    else if (value === "--to-version") result.toVersion = Number(values[++index]);
    else if (value === "--force") result.force = true;
    else if (value === "--iterations") result.iterations = Number(values[++index] || result.iterations);
    else if (value === "--limit") result.limit = Number(values[++index]);
    else if (value === "--keep-records") result.limit = Number(values[++index]);
    else if (value === "--keep-deltas") result.keepDeltas = Number(values[++index]);
    else if (value === "--history") result.history = true;
    else if (value === "--apply") result.apply = true;
    else if (value === "--platform") result.platforms.push(...String(values[++index] || "").split(",").map((item) => item.trim()).filter(Boolean));
    else if (value === "--dry-run") result.dryRun = true;
    else if (value === "--strict") result.strict = true;
    else if (value === "--keep-workspace") result.keepWorkspace = true;
    else if (value === "--cases") result.casesFile = path.resolve(values[++index] || "");
    else if (value === "--runs") result.runsFile = path.resolve(values[++index] || "");
    else if (value === "--condition") result.condition = values[++index] || result.condition;
    else if (value === "--budget-ms") result.timeBudgetMs = Number(values[++index]);
    else if (value === "--max-files") result.maxFiles = Number(values[++index]);
    else if (value === "--max-bytes") result.maxBytes = Number(values[++index]);
    else if (value === "--package") result.packagePath = values[++index] || null;
    else if (value === "--core-mode") result.coreMode = values[++index] || null;
    else if (value === "--mode") result.mode = values[++index] || "overview";
    else if (value === "--scope") result.scope = values[++index] || "application";
    else if (value === "--level") result.level = values[++index] || result.level;
    else if (value === "--focus") result.focus = values[++index] || null;
    else if (value === "--max-nodes") result.maxNodes = Number(values[++index]);
    else if (value === "--max-edges") result.maxEdges = Number(values[++index]);
    else if (value === "--record") result.recordId = values[++index] || null;
    else if (value === "--checkpoint") result.checkpointId = values[++index] || null;
    else if (value === "--overlay") result.overlayId = values[++index] || null;
    else if (value === "--reconciliation") result.reconciliationId = values[++index] || null;
    else if (value === "--plan-ref") result.planRef = values[++index] || null;
    else if (value === "--context-ref") result.contextRef = values[++index] || null;
    else if (value === "--input") result.inputFile = path.resolve(values[++index] || "");
    else if (!value.startsWith("-")) result.root = path.resolve(value);
  }
  return result;
}

function cachedPersistentGraph(root) {
  const cache = readGraphCacheResult(root);
  if (cache.status !== "valid") return null;
  const cachedGit = cache.graph.project?.git;
  const currentGit = readGitMetadata(root);
  const matchesCleanGitRevision = cachedGit?.availability === "available"
    && currentGit.availability === "available"
    && cachedGit.dirty === false
    && currentGit.dirty === false
    && cachedGit.branch === currentGit.branch
    && cachedGit.revision === currentGit.revision
    && cachedGit.shallow === currentGit.shallow;
  if (!matchesCleanGitRevision) return null;
  cache.graph.analysis.cacheState = summarizeCacheResult(cache);
  return cache.graph;
}

async function currentPersistentGraph(root) {
  // Metadata commands operate against the exact persisted JavaScript graph
  // version. Native never reaches this branch because its selected client is
  // acquired first and graph.json is ignored below.
  if (core?.implementation === "javascript") {
    const cache = readGraphCacheResult(root);
    if (cache.status === "valid") {
      cache.graph.analysis.cacheState = summarizeCacheResult(cache);
      return cache.graph;
    }
  }
  return (await acquireCurrentGraph(root, { cacheEnabled: true, reason: "cli-persistent-read" })).graph;
}

async function bootstrapGraph(root, cacheEnabled) {
  return (await acquireCurrentGraph(root, { cacheEnabled, reason: "cli-bootstrap" })).graph;
}

function nativeAuthorityActive() {
  return core?.implementation === "native-experimental"
    && core?.sourceAuthority === "rust"
    && core?.fallback?.active !== true;
}

function disabledGraphCacheState(root, reason = "cache-disabled") {
  return {
    status: "disabled",
    reason,
    path: path.join(root, ".flopeek", "graph.json"),
    diagnostics: [],
    contract: null,
    migrated: false,
  };
}

function nativeGraphCacheState(root, graph) {
  return {
    status: "native-sqlite",
    reason: "native-core-authoritative",
    path: path.join(root, ".flopeek", "native-core.sqlite3"),
    diagnostics: [],
    contract: "flopeek-native-graph-state/v1",
    migrated: false,
    graphVersion: graph.state?.graphVersion ?? null,
    limitation: "Rust/SQLite is authoritative. graph.json was neither read nor written.",
  };
}

async function acquireCurrentGraph(root, options = {}) {
  const cacheEnabled = options.cacheEnabled !== false;
  const initiallyJavascript = core?.implementation === "javascript";
  const previousGraph = cacheEnabled && initiallyJavascript ? cachedPersistentGraph(root) : null;
  if (previousGraph && options.forceScan !== true && !Array.isArray(options.changedPaths)) {
    return { graph: previousGraph, previousGraph, authority: "javascript-graph-json" };
  }
  const graph = await core.scan(root, {
    persistIdentity: cacheEnabled,
    ...(Array.isArray(options.changedPaths) ? { changedPaths: options.changedPaths } : {}),
  });
  graph.analysis.coreRuntime = observeCoreRuntime(coreRuntime.selection, core);
  if (!cacheEnabled) {
    graph.analysis.cacheState = disabledGraphCacheState(graph.project.root);
    return { graph, previousGraph: null, authority: "session-memory" };
  }
  if (nativeAuthorityActive()) {
    graph.analysis.cacheState = nativeGraphCacheState(graph.project.root, graph);
    return { graph, previousGraph: null, authority: "native-sqlite" };
  }
  graph.analysis.cacheState = summarizeCacheResult(writeGraphCache(graph.project.root, graph, {
    reason: options.reason || "cli-authority-acquire",
    changedPaths: options.changedPaths,
  }));
  return { graph, previousGraph, authority: "javascript-graph-json" };
}

function openBrowser(url) {
  if (process.platform === "win32") execFile("cmd.exe", ["/c", "start", "", url], { windowsHide: true });
  else if (process.platform === "darwin") execFile("open", [url]);
  else execFile("xdg-open", [url]);
}

function printHelp() {
  console.log(`Flopeek — project technical map

Package identity:
  flopeek --version

Agent tools (MCP over stdio):
  flopeek mcp [repository] [--package relative/path] [--budget-ms number] [--max-files number] [--max-bytes number] [--no-cache] [--core-mode js|shadow|native|native-experimental]
  flopeek bootstrap [repository] [--format summary|json]

Agent host integration (project-local and non-destructive):
  flopeek install [repository] [--platform auto|codex|claude|cursor|gemini|all] [--dry-run] [--format summary|json]
  flopeek doctor [repository] [--platform codex|claude|cursor|gemini|all] [--strict] [--format summary|json]
  flopeek uninstall [repository] [--platform auto|codex|claude|cursor|gemini|all] [--dry-run] [--format summary|json]

Graph workflow:
  flopeek discover [repository] [--package relative/path] [--budget-ms number] [--max-files number] [--max-bytes number] [--format summary|json]
  flopeek scan [repository] [--package relative/path] [--budget-ms number] [--max-files number] [--max-bytes number] [--format summary|json|mermaid] [--no-cache] [--core-mode js|shadow|native|native-experimental]
  flopeek view [repository] [--mode overview|requests|dependencies] [--scope application|runtime|framework|devtool|all] [--level domain|feature|component|symbol] [--focus node-id] [--max-nodes number] [--max-edges number] [--format summary|json] [--no-cache]
  flopeek impact [repository] [--changed path[,path] | --base git-ref] [--format summary|json]
  flopeek snapshot [repository] [--commit git-ref] [--force] [--format summary|json]
  flopeek history [repository] [--from git-ref] [--to git-ref] [--format summary|json]
  flopeek git-evidence [repository] --context-ref fp://local/... [--limit 12] [--format summary|json]
  flopeek git-continuity [repository] --context-ref fp://local/... [--from git-ref] [--to git-ref] [--format summary|json]
  flopeek related-implementations [repository] --context-ref fp://local/... [--format summary|json]
  flopeek delta [repository] [--from-version number --to-version number] [--format summary|json]
  flopeek benchmark [repository] [--iterations 3] [--format summary|json]
  flopeek proof [repository] [--iterations 3] [--format summary|json]
  flopeek cache status [repository] [--format summary|json]
  flopeek cache prune [repository] [--keep-records number] [--dry-run] [--format summary|json]
  flopeek cache prune [repository] --history [--keep-deltas number] [--max-bytes number] [--apply] [--format summary|json]
  flopeek work list|timeline|workflows|dependencies [repository] [--record work-record-id] [--format summary|json]
  flopeek continue list|show|checkpoint [repository] [--checkpoint checkpoint-id] [--input checkpoint.json] [--format summary|json]
  flopeek continue plan list|show|create|resolve [repository] [--overlay overlay-id] [--plan-ref fpp://local/...] [--input planned-overlay.json] [--format summary|json]
  flopeek continue reconcile list|show|record [repository] [--reconciliation reconciliation-id] [--plan-ref fpp://local/...] [--input reconciliation.json] [--format summary|json]
  flopeek evaluate orientation [suite-root] --cases <file> [--condition baseline|flopeek|both] [--format summary|json]
  flopeek evaluate agent-comparison [suite-root] --cases <file> --runs <file> [--format summary|json]

Guided product demonstration:
  flopeek showcase [--port 4780] [--strict-port] [--no-open] [--keep-workspace] [--format summary|json]
  flopeek showcase apply|reset|status <temporary-workspace> [--format summary|json]

Optional compact local viewer:
  flopeek serve [repository] [--package relative/path] [--budget-ms number] [--max-files number] [--max-bytes number] [-g|--global] [--port 4780] [--strict-port] [--workspace id] [--service-label name] [--no-open]

Global mode activates projects behind one workspace hub/web port. A later command
using the same hub port adds or selects its project without stopping the hub.
Per-project mode remains available; occupied ports advance unless --strict-port.

For compatibility, a repository path without a command starts the viewer.`);
}

function printSummary(graph, cacheWritten = true, cacheMessage = null) {
  const { stats, project } = graph;
  console.log(`${project.name} (${project.git.branch})`);
  console.log(`${stats.scannedFiles} files / ${stats.nodes} nodes / ${stats.edges} edges`);
  console.log(`${stats.endpoints} HTTP entries / ${stats.commandEntries || 0} command entries / ${stats.scheduledEntries || 0} scheduled entries / ${stats.services} services / ${stats.calls || 0} direct calls / ${stats.tests} tests`);
  console.log(cacheWritten ? `Cache: ${path.join(project.root, ".flopeek", "graph.json")}` : cacheMessage || "Cache: not written (--no-cache)");
}

function printImpact(impact) {
  console.log(`${impact.matchedPaths.length} changed files mapped / ${impact.affectedNodes.length} affected nodes`);
  if (impact.deletedPaths.length) console.log(`${impact.deletedPaths.length} deleted files recovered from ${impact.historicalBaseline ? "the prior local graph" : "no baseline"}`);
  console.log(`${impact.affectedEndpoints.length} affected endpoints / ${impact.recommendedTests.length} recommended tests / ${impact.dependencyNodes.length} static dependencies`);
  if (impact.unmatchedPaths.length) console.log(`Not present in current graph: ${impact.unmatchedPaths.join(", ")}`);
}

function printSnapshot(result) {
  const { commit } = result.snapshot;
  console.log(`${result.created ? "Created" : "Reused"} snapshot ${commit.shortRevision} (${commit.requestedRef})`);
  if (commit.subject) console.log(commit.subject);
  console.log(`Snapshot: ${result.path}`);
}

function snapshotPayload(result) {
  return {
    created: result.created,
    path: result.path,
    commit: result.snapshot.commit,
    generatedAt: result.snapshot.graph.generatedAt,
    stats: result.snapshot.graph.stats,
    limitation: "The snapshot is static commit content, excludes uncommitted changes, and does not execute code or configuration.",
  };
}

function printHistory(history) {
  console.log(`${history.before.commit.shortRevision} → ${history.after.commit.shortRevision}`);
  console.log(`${history.changedPaths.length} changed paths / ${history.topology.summary.addedNodes} added nodes / ${history.topology.summary.removedNodes} removed nodes`);
  console.log(`${history.flows.summary.addedFlows} added flows / ${history.flows.summary.removedFlows} removed flows / ${history.flows.summary.changedFlows} changed flows`);
}

function printActiveBranchGitEvidence(result) {
  if (result.status !== "available") {
    console.log(`Git evidence unavailable: ${result.reason}`);
    return;
  }
  console.log(`${result.branch.name} @ ${result.branch.shortHeadRevision}`);
  console.log(`${result.retrieval.returnedCommits} path-touch commits across ${result.retrieval.returnedPaths} Context Card paths`);
  if (result.retrieval.truncated) console.log(`Limited to ${result.retrieval.perPathCommitLimit} commits per path.`);
}

function printGitContextContinuity(result) {
  if (result.status !== "available") {
    console.log(`Git continuity unavailable: ${result.reason}`);
    return;
  }
  console.log(`${result.snapshots.before.commit.shortRevision} → ${result.snapshots.after.commit.shortRevision}`);
  console.log(`Before: ${result.snapshots.before.match.status}`);
  console.log(`After: ${result.snapshots.after.match.status}`);
}

function printRelatedImplementations(result) {
  if (result.status !== "available") {
    console.log(`Related implementations unavailable: ${result.evidence.sourceReadStatus}`);
    return;
  }
  console.log(`${result.candidates.length} static convention candidate${result.candidates.length === 1 ? "" : "s"} for ${result.subject.path}`);
  for (const candidate of result.candidates) console.log(`${candidate.path} (${candidate.matchedTokenCount} exact shared tokens: ${candidate.matchedTokens.join(", ")})`);
  if (result.truncation.candidateFilesOmitted || result.truncation.resultsLimited) console.log("Results are bounded; inspect JSON for truncation details.");
  console.log(result.limitation);
}

function printDelta(delta) {
  console.log(`Graph v${delta.fromGraphVersion} → v${delta.toGraphVersion} (${delta.reason})`);
  console.log(`${delta.changedPaths.length} changed paths / ${delta.summary.addedNodes} added nodes / ${delta.summary.removedNodes} removed nodes / ${delta.summary.changedNodes} changed nodes`);
  console.log(delta.topologyChanged ? "Static topology changed." : "Source changed without a static topology change.");
}

function printDiscovery(result) {
  console.log(`${result.project.name}: ${result.status}`);
  console.log(`${result.inventory.candidateFiles} candidate files / ${result.inventory.candidateBytes} bytes / ${result.inventory.visitedDirectories} visited directories`);
  console.log(`${result.workspace.packages.length} packages / ${result.workspace.manifests.length} total manifests (package manifests included)`);
  if (result.selection?.status === "selected") console.log(`Static package scope: ${result.selection.path} (session-only; repository cache unchanged)`);
  if (result.adapters.length) console.log(`Adapters: ${result.adapters.map((adapter) => `${adapter.id} (${adapter.files})`).join(", ")}`);
  if (result.reasons.length) console.log(`Bounds: ${result.reasons.join(", ")}`);
  console.log(result.decision.safeToStartFullScan ? "Full scan preflight: ready" : "Full scan preflight: blocked by declared bounds");
}

function printBoundedScan(result, cacheWritten = false) {
  console.log(`Bounded scan: ${result.status}`);
  if (result.reason) console.log(`Reason: ${result.reason}`);
  if (result.graph) {
    const packageScoped = result.discovery?.selection?.status === "selected";
    printSummary(result.graph, cacheWritten, packageScoped ? "Cache: not written (package-scoped session)" : null);
    if (packageScoped) console.log(`Static package scope: ${result.discovery.selection.path}`);
  }
  else console.log(`${result.discovery.inventory.candidateFiles} discovered candidate files; no partial graph was promoted.`);
}

function printIntegration(result) {
  console.log(`${result.action}: ${result.ok ? "ready" : "blocked"}${result.dryRun ? " (dry run)" : ""}`);
  console.log(`Platforms: ${result.platforms.join(", ") || "none"}`);
  for (const item of result.plan) console.log(`${item.status.padEnd(9)} ${item.platform} ${item.kind}: ${item.path}${item.reason ? ` - ${item.reason}` : ""}`);
  for (const warning of result.warnings || []) console.log(`${warning.status.padEnd(9)} ${warning.id}: ${warning.message}`);
}

function printDoctor(result) {
  console.log(`Agent integration doctor: ${result.ok ? "ready" : "attention required"}`);
  console.log(`${result.summary.passed} passed / ${result.summary.warnings} warnings / ${result.summary.errors} errors`);
  for (const check of result.checks) console.log(`${check.status.padEnd(7)} ${check.id}: ${check.message}`);
}

function printBootstrap(result) {
  console.log(`${result.project.name} graph v${result.graph.graphVersion} (${result.graph.status})`);
  console.log(`${result.graph.inventory.nodes} nodes / ${result.graph.inventory.edges} edges / ${result.graph.inventory.applicationFlows} application flows`);
  console.log(`Strategy: ${result.policy.strategy}`);
  console.log("Start with get_handoff_context for a known task, then inspect raw node or Flow Lens evidence before editing source.");
}

function readJsonInput(inputFile, description = "Continuation checkpoint creation") {
  if (!inputFile) throw new Error(`${description} requires --input <json-file>.`);
  let value;
  try { value = JSON.parse(fs.readFileSync(inputFile, "utf8")); } catch (error) { throw new Error(`Unable to read continuation checkpoint input (${error.message}).`); }
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Continuation checkpoint input must be a JSON object.");
  return value;
}

function printContinuation(result, action) {
  if (action === "list") {
    const freshness = result.records.reduce((counts, record) => ({ ...counts, [record.freshnessStatus]: (counts[record.freshnessStatus] || 0) + 1 }), {});
    console.log(`${result.records.length} local continuation checkpoints (${freshness.current || 0} current / ${freshness.stale || 0} stale / ${freshness.future || 0} future)`);
  } else if (action === "show") {
    console.log(`${result.checkpoint.id}: ${result.checkpoint.lifecycleStatus} / ${result.checkpoint.freshnessStatus}`);
  } else {
    console.log(`${result.created ? "Created" : "Reused"} continuation checkpoint ${result.checkpoint.id} at graph v${result.checkpoint.baseline.graphVersion}.`);
  }
  console.log(result.limitation || "Continuation checkpoint metadata remains separate from parser facts and runtime proof.");
}

function printPlannedOverlay(result, action) {
  if (action === "list") {
    const freshness = result.records.reduce((counts, record) => ({ ...counts, [record.checkpointFreshnessStatus]: (counts[record.checkpointFreshnessStatus] || 0) + 1 }), {});
    console.log(`${result.records.length} local planned overlays (${freshness.current || 0} current-anchor / ${freshness.stale || 0} stale-anchor / ${freshness.future || 0} future-anchor)`);
  } else if (action === "show") {
    console.log(`${result.overlay.id}: checkpoint ${result.overlay.checkpointId} / ${result.overlay.checkpointFreshnessStatus} / ${result.overlay.nodes.length} planned nodes`);
  } else if (action === "resolve") {
    console.log(`${result.status}: ${result.resolvedRef || "no resolved Plan Ref"}`);
  } else {
    console.log(`${result.created ? "Created" : "Reused"} planned overlay ${result.overlay.id} at checkpoint ${result.overlay.checkpointId} v${result.overlay.overlayVersion}.`);
  }
  console.log(result.limitation || "Planned-overlay metadata remains separate from parser facts, Flow Lens, impact, and runtime proof.");
}

function printPlanReconciliation(result, action) {
  if (action === "list") {
    const outcomes = result.records.reduce((counts, record) => ({ ...counts, [record.outcome]: (counts[record.outcome] || 0) + 1 }), {});
    console.log(`${result.records.length} local plan reconciliations (${outcomes["confirmed-implemented"] || 0} confirmed / ${outcomes["partially-implemented"] || 0} partial / ${outcomes["implemented-differently"] || 0} different)`);
  } else if (action === "show") {
    console.log(`${result.reconciliation.id}: ${result.reconciliation.outcome} / ${result.reconciliation.actorKind} / ${result.reconciliation.actualContextRefs.length} actual Context Refs`);
  } else {
    console.log(`${result.created ? "Recorded" : "Reused"} plan reconciliation ${result.reconciliation.id}: ${result.reconciliation.outcome}.`);
  }
  console.log(result.limitation || "Plan-reconciliation metadata remains separate from parser facts, Flow Lens, impact, test proof, runtime proof, and approval authority.");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (!coreRuntime) {
    coreRuntime = createSurfaceCoreRuntime({ coreMode: options.coreMode });
    core = coreRuntime.core;
  }
  if (options.command === "help" || options.command === "h") return printHelp();
  if (options.command === "version" || options.command === "v") return console.log(packageInfo.version);
  if (options.command === "mcp") {
    const ownedCore = core;
    core = null;
    try {
      return await runMcpServer({
        ...options,
        coreClient: ownedCore,
        coreRuntime: coreRuntime.selection,
        ownsCoreClient: true,
      });
    } catch (error) {
      await ownedCore?.close?.().catch(() => {});
      throw error;
    }
  }
  if (options.packagePath && !["discover", "scan", "serve", "mcp"].includes(options.command)) throw new Error("--package is currently supported only by discover, scan, serve, and mcp.");
  if (options.command === "showcase") {
    if (options.showcaseAction !== "run") {
      const result = options.showcaseAction === "apply"
        ? applyShowcaseChange(options.root)
        : options.showcaseAction === "reset"
          ? resetShowcase(options.root)
          : showcaseStatus(options.root);
      if (options.format === "json") console.log(JSON.stringify(result, null, 2));
      else if (options.format === "summary") console.log(`${result.showcaseId}: ${result.status} (${result.changePath})${result.changed === undefined ? "" : result.changed ? " - source updated" : " - no change required"}`);
      else throw new Error("Showcase actions support summary or json formats.");
      return;
    }
    const instance = await startShowcase({ port: options.port, portFallback: options.portFallback, keepWorkspace: options.keepWorkspace });
    if (options.format === "json") console.log(JSON.stringify(showcasePublicResult(instance), null, 2));
    else if (options.format === "summary") printShowcase(instance);
    else {
      await instance.close();
      throw new Error("Showcase output supports summary or json formats.");
    }
    if (options.open) openBrowser(instance.url);
    let closing = false;
    const close = async () => {
      if (closing) return;
      closing = true;
      await instance.close();
      process.exit(0);
    };
    process.once("SIGINT", close);
    process.once("SIGTERM", close);
    return;
  }
  if (options.command === "evaluate") {
    if (!options.casesFile) throw new Error(`${options.evaluation === "agent-comparison" ? "Agent comparison" : "Orientation"} evaluation requires --cases <file>.`);
    let result;
    let summary;
    if (options.evaluation === "orientation") {
      result = evaluateOrientation(options.root, loadOrientationCases(options.casesFile), { condition: options.condition });
      summary = orientationSummary(result);
    } else if (options.evaluation === "agent-comparison") {
      if (!options.runsFile) throw new Error("Agent comparison evaluation requires --runs <file>.");
      result = evaluateAgentComparison(options.root, loadOrientationCases(options.casesFile), loadAgentComparisonRuns(options.runsFile));
      summary = agentComparisonSummary(result);
    } else throw new Error("Supported evaluations are orientation and agent-comparison.");
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") console.log(summary);
    else throw new Error("Evaluation supports summary or json formats.");
    return;
  }
  if (["install", "uninstall", "doctor"].includes(options.command)) {
    const integrationOptions = { platforms: options.platforms.length ? options.platforms : options.command === "doctor" ? "all" : "auto", dryRun: options.dryRun, strict: options.strict };
    const result = options.command === "install"
      ? installAgentIntegration(options.root, integrationOptions)
      : options.command === "uninstall"
        ? uninstallAgentIntegration(options.root, integrationOptions)
        : doctorAgentIntegration(options.root, integrationOptions);
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") options.command === "doctor" ? printDoctor(result) : printIntegration(result);
    else throw new Error("Agent integration output supports summary or json formats.");
    if (!result.ok) process.exitCode = 1;
    return;
  }
  if (options.command === "bootstrap") {
    const graph = await bootstrapGraph(options.root, options.cache);
    const result = await core.getScanStatus(graph);
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") printBootstrap(result);
    else throw new Error("Bootstrap output supports summary or json formats.");
    return;
  }
  if (options.command === "discover") {
    const result = discoverRepository(options.root, {
      timeBudgetMs: options.timeBudgetMs,
      maxFiles: options.maxFiles,
      maxBytes: options.maxBytes,
      packagePath: options.packagePath,
    });
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") printDiscovery(result);
    else throw new Error("Discovery output supports summary or json formats.");
    if (!result.decision.safeToStartFullScan) process.exitCode = 2;
    return;
  }
  if (options.command === "scan") {
    const bounded = options.timeBudgetMs !== null || options.maxFiles !== null || options.maxBytes !== null || Boolean(options.packagePath);
    if (bounded) {
      const controller = new AbortController();
      const cancel = () => controller.abort();
      process.once("SIGINT", cancel);
      process.once("SIGTERM", cancel);
      let result;
      try {
        const coordinator = createScanCoordinator(options.root, {
          coreClient: core,
          coreRuntime: coreRuntime.selection,
          cache: options.cache,
          timeBudgetMs: options.timeBudgetMs,
          maxFiles: options.maxFiles,
          maxBytes: options.maxBytes,
          packagePath: options.packagePath,
        });
        const coordinated = await coordinator.refresh(null, "cli-bounded-scan", controller.signal);
        result = coordinated.boundedResult || {
          schemaVersion: "flopeek-bounded-scan-result/v1",
          status: coordinated.outcome.status,
          generatedAt: coordinated.outcome.completedAt,
          durationMs: coordinated.outcome.durationMs,
          discovery: coordinated.outcome.discovery,
          graph: coordinated.graph,
          reason: coordinated.outcome.reason,
          failure: coordinated.outcome.failure,
          cachePromotion: {
            allowed: coordinated.outcome.cachePromotion.allowed,
            reason: "The shared scan coordinator allows canonical cache promotion only for a complete result.",
          },
          limitations: coordinated.outcome.limitations,
        };
        result.scanOutcome = coordinated.outcome;
      } finally {
        process.removeListener("SIGINT", cancel);
        process.removeListener("SIGTERM", cancel);
      }
      if (options.format === "json") console.log(JSON.stringify(result, null, 2));
      else if (options.format === "mermaid" && result.graph) console.log(graphToMermaid(result.graph));
      else if (options.format === "summary") printBoundedScan(result, result.cachePromotion?.allowed === true && result.status === "complete");
      else if (options.format === "mermaid") throw new Error("A bounded scan that produced no complete graph cannot be rendered as Mermaid.");
      else throw new Error("--format must be summary, json, or mermaid.");
      if (result.status === "partial-by-budget" || result.status === "cancelled") process.exitCode = 2;
      else if (result.status === "failed") process.exitCode = 1;
      return;
    }
    // `--no-cache` is the safe inspection mode: it must not leave Flopeek
    // metadata behind merely to obtain a generated project identity.
    const { graph } = await acquireCurrentGraph(options.root, {
      cacheEnabled: options.cache,
      reason: "cli-scan",
      forceScan: true,
    });
    if (options.format === "json") console.log(JSON.stringify(graph, null, 2));
    else if (options.format === "mermaid") console.log(graphToMermaid(graph));
    else if (options.format === "summary") printSummary(graph, options.cache);
    else throw new Error("--format must be summary, json, or mermaid.");
    return;
  }
  if (options.command === "view") {
    const { graph } = await acquireCurrentGraph(options.root, {
      cacheEnabled: options.cache,
      reason: "cli-view",
    });
    const result = await core.getProjectOverview(graph, { mode: options.mode, scope: options.scope, level: options.level, focus: options.focus, maxNodes: options.maxNodes, maxEdges: options.maxEdges });
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") {
      console.log(`${result.view.mode} / ${result.view.scope}: ${result.display.catalog.nodes.returned}/${result.display.catalog.nodes.total} nodes, ${result.display.catalog.edges.returned}/${result.display.catalog.edges.total} edges`);
      if (result.display.catalog.warning) console.log(result.display.catalog.warning);
    } else throw new Error("View output supports summary or json formats.");
    return;
  }
  if (options.command === "impact") {
    const changedPaths = options.changed.length ? options.changed : getGitChangedPaths(options.root, options.base);
    const acquired = await acquireCurrentGraph(options.root, {
      cacheEnabled: options.cache,
      reason: "cli-impact",
      changedPaths,
      forceScan: true,
    });
    const graph = acquired.graph;
    const previousGraphVersion = nativeAuthorityActive() ? graph.state.graphVersion - 1 : undefined;
    const impact = await core.getChangeImpact(graph, changedPaths, {
      previousGraph: acquired.previousGraph,
      previousGraphVersion,
    });
    if (options.format === "json") console.log(JSON.stringify(impact, null, 2));
    else if (options.format === "summary") printImpact(impact);
    else throw new Error("Impact output supports summary or json formats.");
    return;
  }
  if (options.command === "snapshot") {
    const result = createGitSnapshot(options.root, { ref: options.commit, force: options.force });
    if (options.format === "json") console.log(JSON.stringify(snapshotPayload(result), null, 2));
    else if (options.format === "summary") printSnapshot(result);
    else throw new Error("Snapshot output supports summary or json formats.");
    return;
  }
  if (options.command === "history") {
    const result = compareGitSnapshots(options.root, { from: options.from, to: options.to });
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") printHistory(result);
    else throw new Error("History output supports summary or json formats.");
    return;
  }
  if (options.command === "delta") {
    const { graph } = await acquireCurrentGraph(options.root, {
      cacheEnabled: options.cache,
      reason: "cli-delta",
    });
    const delta = await core.getGraphDelta(graph, {
      ...(options.fromVersion !== null ? { fromVersion: options.fromVersion } : {}),
      ...(options.toVersion !== null ? { toVersion: options.toVersion } : {}),
    });
    if (!delta) throw new Error("No matching persisted graph delta was found.");
    if (options.format === "json") console.log(JSON.stringify(delta, null, 2));
    else if (options.format === "summary") printDelta(delta);
    else throw new Error("Delta output supports summary or json formats.");
    return;
  }
  if (options.command === "benchmark") {
    const result = benchmarkRepository(options.root, { iterations: options.iterations });
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") printBenchmark(result);
    else throw new Error("Benchmark output supports summary or json formats.");
    return;
  }
  if (options.command === "git-evidence") {
    if (!options.contextRef) throw new Error("Git evidence requires --context-ref <fp://local/...>.");
    const { graph } = await acquireCurrentGraph(options.root, {
      cacheEnabled: options.cache,
      reason: "cli-active-branch-git-evidence",
    });
    const result = getActiveBranchGitEvidence(graph, options.contextRef, { limit: options.limit });
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") printActiveBranchGitEvidence(result);
    else throw new Error("Git evidence output supports summary or json formats.");
    return;
  }
  if (options.command === "git-continuity") {
    if (!options.contextRef) throw new Error("Git continuity requires --context-ref <fp://local/...>.");
    const { graph } = await acquireCurrentGraph(options.root, {
      cacheEnabled: options.cache,
      reason: "cli-git-context-continuity",
    });
    const result = getGitContextContinuity(graph, options.contextRef, { from: options.from, to: options.to });
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") printGitContextContinuity(result);
    else throw new Error("Git continuity output supports summary or json formats.");
    return;
  }
  if (options.command === "related-implementations") {
    if (!options.contextRef) throw new Error("Related implementations requires --context-ref <fp://local/...>.");
    const result = getRelatedImplementations(await currentPersistentGraph(options.root), options.contextRef);
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") printRelatedImplementations(result);
    else throw new Error("Related implementations output supports summary or json formats.");
    return;
  }
  if (options.command === "proof") {
    const graph = await core.scan(options.root, { persistIdentity: false });
    const result = createProductProof(graph, { localBenchmark: benchmarkRepository(options.root, { iterations: options.iterations }) });
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") printProductProof(result);
    else throw new Error("Proof output supports summary or json formats.");
    return;
  }
  if (options.command === "cache") {
    const result = options.cacheAction === "prune"
      ? options.history
        ? pruneGraphDeltas(options.root, { keepDeltas: options.keepDeltas || undefined, maxBytes: options.maxBytes || undefined, dryRun: !options.apply })
        : pruneArtifactCache(options.root, { keepRecords: options.limit || 4, dryRun: options.dryRun })
      : cacheHygiene(options.root);
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") {
      if (options.cacheAction === "prune") console.log(`${result.dryRun ? "Would prune" : "Pruned"} ${result.pruned.length} ${options.history ? "validated graph deltas" : "derived artifacts"} / ${result.reclaimedBytes} bytes reclaimed`);
      else console.log(`${result.storage.total.files} Flopeek cache files / ${result.storage.total.bytes} bytes / ${result.storage.deltaHistory.files} graph deltas / ${result.storage.derivedArtifacts.records} registered derived artifacts`);
      console.log(result.limitation || result.retention?.destructiveScope || "Cache metadata only.");
    } else throw new Error("Cache output supports summary or json formats.");
    return;
  }
  if (options.command === "work") {
    const graph = options.workAction === "workflows" ? null : await currentPersistentGraph(options.root);
    const result = options.workAction === "workflows"
      ? listStoredWorkflows(path.resolve(options.root))
      : options.workAction === "timeline"
        ? getWorkTimeline(graph, options.recordId)
        : options.workAction === "dependencies"
          ? (() => {
            if (!options.recordId) throw new Error("Dependency status requires --record <work-record-id>.");
            return getWorkDependencyStatus(graph, options.recordId);
          })()
        : listWorkRecords(graph, { limit: 50 });
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") {
      if (options.workAction === "workflows") console.log(`${result.workflows.length} available local workflows`);
      else if (options.workAction === "timeline") console.log(`${result.records.length} planned records / ${result.actualEvents.length} append-only actual events`);
      else if (options.workAction === "dependencies") console.log(`${result.summary.ready} ready / ${result.summary.blocking} blocking / ${result.summary.unresolved} unresolved / ${result.summary.unknown} unknown declared dependencies`);
      else console.log(`${result.totalMatched} local work records / ${result.events.length} recent actual events`);
      console.log(result.limitation || "Delivery metadata remains separate from parser facts and runtime proof.");
    } else throw new Error("Work output supports summary or json formats.");
    return;
  }
  if (options.command === "continue") {
    if (!options.cache) throw new Error("Continuation checkpoint commands require persistent graph identity; omit --no-cache.");
    const graph = await currentPersistentGraph(options.root);
    const result = options.continueAction === "plan"
      ? options.planAction === "create"
        ? createPlannedOverlay(graph, readJsonInput(options.inputFile, "Planned-overlay creation"))
        : options.planAction === "show"
          ? (() => {
            if (!options.overlayId) throw new Error("Planned-overlay display requires --overlay <overlay-id>.");
            return getPlannedOverlay(graph, options.overlayId);
          })()
          : options.planAction === "resolve"
            ? (() => {
              if (!options.planRef) throw new Error("Plan Ref resolution requires --plan-ref <fpp://local/...>.");
              return resolvePlanRef(graph, options.planRef);
            })()
            : listPlannedOverlays(graph)
      : options.continueAction === "reconcile"
        ? options.reconcileAction === "record"
          ? recordPlanReconciliation(graph, readJsonInput(options.inputFile, "Plan-reconciliation recording"))
          : options.reconcileAction === "show"
            ? (() => {
              if (!options.reconciliationId) throw new Error("Plan-reconciliation display requires --reconciliation <id>.");
              return getPlanReconciliation(graph, options.reconciliationId);
            })()
            : listPlanReconciliations(graph, { planRef: options.planRef || null })
        : options.continueAction === "checkpoint"
      ? createContinuationCheckpoint(graph, readJsonInput(options.inputFile))
      : options.continueAction === "show"
        ? (() => {
          if (!options.checkpointId) throw new Error("Continuation checkpoint display requires --checkpoint <id>.");
          return getContinuationCheckpoint(graph, options.checkpointId);
        })()
        : listContinuationCheckpoints(graph);
    if (options.format === "json") console.log(JSON.stringify(result, null, 2));
    else if (options.format === "summary") {
      if (options.continueAction === "plan") printPlannedOverlay(result, options.planAction);
      else if (options.continueAction === "reconcile") printPlanReconciliation(result, options.reconcileAction);
      else printContinuation(result, options.continueAction);
    } else throw new Error("Continuation output supports summary or json formats.");
    return;
  }
  if (options.command !== "serve") throw new Error(`Unknown command: ${options.command}`);
  if (!Number.isInteger(options.port) || options.port < 1 || options.port > 65535) throw new Error("--port must be a valid TCP port.");

  if (options.global) {
    if (options.packagePath) throw new Error("Package-scoped scans cannot join a global workspace yet. Start a per-project server instead.");
    let activated = null;
    try {
      activated = await activateOnWorkspaceHub({
        port: options.port,
        workspaceId: options.workspaceId,
        root: options.root,
        serviceLabel: options.serviceLabel,
        timeBudgetMs: options.timeBudgetMs,
        maxFiles: options.maxFiles,
        maxBytes: options.maxBytes,
        coreMode: options.coreMode,
      });
    } catch (error) {
      if (error?.name !== "TimeoutError" && error?.cause?.code !== "ECONNREFUSED" && error?.code !== "ECONNREFUSED") throw error;
    }
    const url = `http://127.0.0.1:${activated?.hubPort || options.port}`;
    if (activated) {
      console.log(`Activated ${activated.project.serviceLabel} in existing workspace ${activated.workspace.workspaceId}: ${url}`);
      console.log(`Project ID: ${activated.project.projectId}`);
      if (options.open) openBrowser(url);
      return;
    }
    const hub = await startWorkspaceServer({
      port: options.port,
      portFallback: options.portFallback,
      workspaceId: options.workspaceId,
      projects: [{
        root: options.root,
        serviceLabel: options.serviceLabel,
        timeBudgetMs: options.timeBudgetMs,
        maxFiles: options.maxFiles,
      maxBytes: options.maxBytes,
      coreMode: options.coreMode,
      }],
    });
    const hubUrl = `http://127.0.0.1:${hub.port}`;
    console.log(`Flopeek workspace hub: ${hubUrl}`);
    console.log(`Serve workspace: ${hub.workspaceId}`);
    console.log(`Active projects: ${hub.workspace().projectCount}`);
    if (hub.portBinding.fallback) console.log(`Port ${hub.portBinding.requestedPort} was occupied; the hub uses ${hub.port} without stopping the existing process.`);
    if (options.open) openBrowser(hubUrl);
    const closeHub = () => hub.close().then(() => process.exit(0));
    process.once("SIGINT", closeHub);
    process.once("SIGTERM", closeHub);
    return;
  }

  const ownedCore = core;
  core = null;
  let app;
  try {
    app = await startServer({
      ...options,
      coreClient: ownedCore,
      coreRuntime: coreRuntime.selection,
      ownsCoreClient: true,
    });
  } catch (error) {
    await ownedCore?.close?.().catch(() => {});
    throw error;
  }
  const url = `http://127.0.0.1:${app.port}`;
  console.log(`Compact Project Flow Explorer viewer: ${url}`);
  console.log(`Scanning: ${app.root}`);
  console.log(`Serve workspace: ${app.serveInstance.workspaceId}`);
  console.log(`Project ID: ${app.serveInstance.project.projectId}`);
  if (app.portBinding.fallback) console.log(`Port ${app.portBinding.requestedPort} was occupied; this instance uses ${app.port} without stopping the existing process.`);
  if (options.open) openBrowser(url);
  const close = () => app.close().then(() => process.exit(0));
  process.once("SIGINT", close);
  process.once("SIGTERM", close);
}

main()
  .catch((error) => {
    console.error(`Flopeek command failed: ${error.message}`);
    process.exitCode = 1;
  })
  .finally(() => core?.close?.().catch(() => {}));
