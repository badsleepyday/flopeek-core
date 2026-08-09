"use strict";

const { adapterContractDigest, getAdapterRegistry } = require("./adapter-registry");
const { validateNativeDogfoodEvidence } = require("./native-dogfood-evidence");
const NATIVE_ROLLOUT_GATE_SCHEMA = "flopeek-native-rollout-gate/v1";
const MINIMUM_BENCHMARK_REPOSITORIES = 5;
const MAXIMUM_REGRESSION_SPEEDUP = 0.9;
const REQUIRED_ONE_FILE_SPEEDUP = 2;
const REQUIRED_ONE_FILE_REPOSITORIES = 4;
const NATIVE_BACKEND_PARITY_SCHEMA = "flopeek-native-backend-parity/v1";
const NATIVE_ADAPTER_PARITY_SCHEMA = "flopeek-native-adapter-parity/v1";
const NATIVE_BENCHMARK_SCHEMA = "flopeek-native-core-client-benchmark/v2";
const MINIMUM_ADAPTER_CASES = Object.freeze({
  typescript: 5,
  python: 3,
  go: 5,
  csharp: 5,
  java: 3,
  rust: 3,
  php: 3,
  svelte: 2,
});
const REQUIRED_QUERY_OPERATION_P95_MS = Object.freeze({
  findNodes: 50,
  projectOverview: 50,
  contextCard: 50,
  flowProjection: 50,
  resolveContextRef: 20,
});
const REQUIRED_NATIVE_ADAPTERS = Object.freeze(getAdapterRegistry().adapters
  .filter((adapter) => adapter.capabilities.structure !== "inventory-only")
  .map((adapter) => adapter.id)
  .sort());

function sameStringSet(left, right) {
  if (!Array.isArray(left) || !Array.isArray(right)) return false;
  const leftSet = new Set(left);
  const rightSet = new Set(right);
  return left.length === leftSet.size
    && right.length === rightSet.size
    && leftSet.size === rightSet.size
    && [...leftSet].every((value) => rightSet.has(value));
}

function paritySha256(value) {
  return typeof value === "string" && /^sha256:[a-f0-9]{64}$/u.test(value);
}

function parityBinarySha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/u.test(value);
}

function sameValues(left, right) {
  return Array.isArray(left)
    && Array.isArray(right)
    && left.length === right.length
    && left.every((value, index) => value === right[index]);
}

function requireExactKeys(value, expected, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object.`);
  }
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (!sameValues(actual, wanted)) {
    throw new Error(`${label} must contain exactly: ${wanted.join(", ")}.`);
  }
}

function validateNativeAdapterParity(evidence, requiredAdapters = Object.keys(MINIMUM_ADAPTER_CASES).sort()) {
  requireExactKeys(evidence, [
    "schemaVersion",
    "adapterContractDigest",
    "generatedAt",
    "binary",
    "summary",
    "adapters",
  ], "native adapter parity evidence");
  if (evidence.schemaVersion !== NATIVE_ADAPTER_PARITY_SCHEMA) {
    throw new Error(`Native adapter parity evidence must use ${NATIVE_ADAPTER_PARITY_SCHEMA}.`);
  }
  if (evidence.adapterContractDigest !== adapterContractDigest()) {
    throw new Error("Native adapter parity evidence does not match the committed adapter contract.");
  }
  if (typeof evidence.generatedAt !== "string" || !Number.isFinite(Date.parse(evidence.generatedAt))) {
    throw new Error("Native adapter parity evidence requires a valid generatedAt timestamp.");
  }
  requireExactKeys(evidence.binary, ["sha256", "sourceRevision"], "native adapter parity binary");
  if (!parityBinarySha256(evidence.binary.sha256)
    || !/^[a-f0-9]{40,64}$/u.test(evidence.binary.sourceRevision || "")) {
    throw new Error("Native adapter parity evidence requires an exact binary SHA-256 and source revision.");
  }
  requireExactKeys(evidence.summary, ["adapters", "cases", "exactCases"], "native adapter parity summary");
  requireExactKeys(evidence.adapters, requiredAdapters, "native adapter parity adapters");

  const seenCaseIds = new Set();
  const seenSourceDigests = new Set();
  let totalCases = 0;
  let totalExactCases = 0;
  for (const adapterId of requiredAdapters) {
    const adapter = evidence.adapters[adapterId];
    requireExactKeys(adapter, [
      "cases",
      "exactCases",
      "caseIds",
      "sourceDigests",
      "compatibilityDigests",
      "records",
    ], `native adapter parity adapter ${adapterId}`);
    const minimum = MINIMUM_ADAPTER_CASES[adapterId] || 1;
    if (!Number.isSafeInteger(adapter.cases) || adapter.cases < minimum
      || adapter.exactCases !== adapter.cases
      || !Array.isArray(adapter.records) || adapter.records.length !== adapter.cases) {
      throw new Error(`Native adapter parity adapter ${adapterId} requires at least ${minimum} exact machine cases.`);
    }
    const caseIds = [];
    const sourceDigests = [];
    const compatibilityDigests = [];
    for (const record of adapter.records) {
      requireExactKeys(record, [
        "adapterId",
        "caseId",
        "fixtureId",
        "sourceDigest",
        "javascriptCompatibilityDigest",
        "nativeCompatibilityDigest",
        "exact",
        "nativeParserHost",
        "executionAdapterCapability",
        "binarySha256",
        "sourceRevision",
      ], `native adapter parity case ${adapterId}`);
      if (record.adapterId !== adapterId
        || typeof record.caseId !== "string" || !record.caseId.startsWith(`${adapterId}:`)
        || typeof record.fixtureId !== "string" || !record.fixtureId
        || !paritySha256(record.sourceDigest)
        || !paritySha256(record.javascriptCompatibilityDigest)
        || record.nativeCompatibilityDigest !== record.javascriptCompatibilityDigest
        || record.exact !== true
        || record.nativeParserHost !== "rust-tree-sitter-source/v19"
        || record.binarySha256 !== evidence.binary.sha256
        || record.sourceRevision !== evidence.binary.sourceRevision) {
        throw new Error(`Native adapter parity case ${record.caseId || adapterId} is not exact and binary-bound.`);
      }
      const capability = record.executionAdapterCapability;
      if (!capability || capability.id !== adapterId
        || capability.availability !== "bundled"
        || typeof capability.parser !== "string" || !capability.parser
        || capability.requiredToolchain !== null) {
        throw new Error(`Native adapter parity case ${record.caseId} did not execute the bundled native adapter.`);
      }
      if (seenCaseIds.has(record.caseId)) {
        throw new Error(`Duplicate native adapter parity case ID: ${record.caseId}`);
      }
      if (seenSourceDigests.has(record.sourceDigest)) {
        throw new Error(`Duplicate native adapter parity source digest: ${record.sourceDigest}`);
      }
      seenCaseIds.add(record.caseId);
      seenSourceDigests.add(record.sourceDigest);
      caseIds.push(record.caseId);
      sourceDigests.push(record.sourceDigest);
      compatibilityDigests.push(record.nativeCompatibilityDigest);
    }
    if (!sameValues(adapter.caseIds, caseIds)
      || !sameValues(adapter.sourceDigests, sourceDigests)
      || !sameValues(adapter.compatibilityDigests, compatibilityDigests)) {
      throw new Error(`Native adapter parity adapter ${adapterId} summaries do not match their raw records.`);
    }
    totalCases += adapter.cases;
    totalExactCases += adapter.exactCases;
  }
  if (evidence.summary.adapters !== requiredAdapters.length
    || evidence.summary.cases !== totalCases
    || evidence.summary.exactCases !== totalExactCases) {
    throw new Error("Native adapter parity summary does not match its raw adapter records.");
  }
  return true;
}

function nativeAdaptersFromParity(evidence, requiredAdapters) {
  validateNativeAdapterParity(evidence, requiredAdapters);
  return Object.keys(evidence.adapters).sort();
}

// A native graph store is not a native backend when JavaScript still parses
// source, resolves imports, or materializes the parser-to-graph input. Keep
// this contract deliberately about authority, not an implementation detail
// such as the particular Rust parser crate. JavaScript remains allowed as the
// CI/rollback oracle, but it must not be on the native production data path.
function hasNativeBackendAuthority(value, adapterParity) {
  let nativeAdapters;
  try {
    nativeAdapters = nativeAdaptersFromParity(adapterParity, REQUIRED_NATIVE_ADAPTERS);
  } catch {
    return false;
  }
  return value?.schemaVersion === NATIVE_BACKEND_PARITY_SCHEMA
    && value.sourceDiscoveryAuthority === "rust"
    && value.parserAuthority === "rust"
    && value.resolverAuthority === "rust"
    && value.structuralFactAuthority === "rust"
    && value.javascriptRole === "oracle-and-rollback-only"
    && Number.isSafeInteger(value.fixtureCount)
    && value.fixtureCount > 0
    && value.exactFixtureCount === value.fixtureCount
    && value.adapterContractDigest === adapterContractDigest()
    && sameStringSet(value.requiredAdapters, REQUIRED_NATIVE_ADAPTERS)
    && sameStringSet(nativeAdapters, REQUIRED_NATIVE_ADAPTERS)
    && Array.isArray(value.fallbackOnlyAdapters)
    && value.fallbackOnlyAdapters.length === 0
    && value.adapterCoveragePolicy === "all-native";
}

function finiteNonNegative(value) {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function percentile(values, quantile) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.ceil((quantile / 100) * sorted.length) - 1)];
}

function measuredQueryOperationP95(performance, benchmark) {
  const profiles = performance?.queryRawSamples;
  const expectedRepositories = new Map(benchmarkRows(benchmark)
    .map((row) => [row?.repository, row]));
  if (!Array.isArray(profiles) || profiles.length < MINIMUM_BENCHMARK_REPOSITORIES
    || profiles.length !== expectedRepositories.size
    || new Set(profiles.map((profile) => profile?.repository)).size !== profiles.length) return null;
  const values = Object.fromEntries(Object.keys(REQUIRED_QUERY_OPERATION_P95_MS)
    .map((operation) => [operation, []]));
  for (const profile of profiles) {
    const expected = expectedRepositories.get(profile?.repository);
    if (typeof profile?.repository !== "string" || !profile.repository.trim()
      || !expected
      || profile.repositoryRevision !== expected.repositoryRevision
      || profile.sourceDigest !== expected.sourceDigest
      || !profile.states || typeof profile.states !== "object") return null;
    for (const state of ["cold", "unchanged", "oneFileChange"]) {
      const stateSamples = profile.states[state];
      if (!stateSamples || JSON.stringify(Object.keys(stateSamples).sort())
        !== JSON.stringify(Object.keys(REQUIRED_QUERY_OPERATION_P95_MS).sort())) return null;
      for (const operation of Object.keys(REQUIRED_QUERY_OPERATION_P95_MS)) {
        const samples = stateSamples[operation];
        if (!Array.isArray(samples) || samples.length !== 101
          || !samples.every(finiteNonNegative)) return null;
        values[operation].push(percentile(samples, 95));
      }
    }
  }
  return Object.fromEntries(Object.entries(values)
    .map(([operation, cellP95]) => [operation, Math.max(...cellP95)]));
}

function hasDatabaseOpenEvidence(performance) {
  const binding = performance?.databaseOpenEvidence;
  const evidence = binding?.evidence;
  const observations = evidence?.observations;
  return performance?.databaseOpenDoesNotDeserializeFullGraph === true
    && /^[a-f0-9]{64}$/u.test(binding?.sha256 || "")
    && evidence?.schemaVersion === "flopeek-native-database-open-evidence/v1"
    && evidence.operation === "open-current-graph"
    && evidence.fullPayloadDeserialized === false
    && /^[a-f0-9]{64}$/u.test(evidence.binarySha256 || "")
    && /^[a-f0-9]{40,64}$/u.test(evidence.repositoryRevision || "")
    && /^[a-f0-9]{64}$/u.test(evidence.sourceDigest || "")
    && observations?.schemaVersion === "flopeek-native-database-open-observation/v1"
    && observations.currentGraphFound === true
    && observations.graphPayloadRowsRead === 0
    && observations.graphPayloadBytesDeserialized === 0
    && Array.isArray(observations.sqliteOperations)
    && observations.sqliteOperations.length === 1
    && observations.sqliteOperations[0] === "current-complete-graph-metadata";
}

function hasNativeDogfoodEvidence(value, binding = {}) {
  try {
    return validateNativeDogfoodEvidence(value, binding).status === "complete";
  } catch {
    return false;
  }
}

function benchmarkRows(report) {
  return Array.isArray(report?.rows) ? report.rows : [];
}

function hasBoundBenchmarkArtifact(report) {
  const artifact = report?.nativeArtifact;
  return report?.schemaVersion === NATIVE_BENCHMARK_SCHEMA
    && artifact && typeof artifact === "object"
    && /^[a-f0-9]{64}$/u.test(artifact.binarySha256 || "")
    && /^[a-f0-9]{40,64}$/u.test(artifact.repositoryRevision || "")
    && /^[a-f0-9]{64}$/u.test(artifact.sourceDigest || "")
    && typeof artifact.platformPackage === "string" && artifact.platformPackage.length > 0
    && typeof artifact.target === "string" && artifact.target.length > 0
    && typeof artifact.compilerVersion === "string" && artifact.compilerVersion.length > 0;
}

function distinctBenchmarkRepositories(rows) {
  return new Set(rows
    .map((row) => typeof row?.repository === "string" ? row.repository.trim() : "")
    .filter(Boolean)).size;
}

function median(values) {
  const ordered = [...values].sort((left, right) => left - right);
  const middle = Math.floor(ordered.length / 2);
  return ordered.length % 2 ? ordered[middle] : (ordered[middle - 1] + ordered[middle]) / 2;
}

function measuredSpeedup(sample) {
  const javascript = sample?.jsSamplesMs;
  const native = sample?.nativeSamplesMs;
  if (!Array.isArray(javascript) || javascript.length < 3
    || !Array.isArray(native) || native.length !== javascript.length
    || !javascript.every((value) => finiteNonNegative(value))
    || !native.every((value) => finiteNonNegative(value) && value > 0)) return null;
  const computed = Number((median(javascript) / median(native)).toFixed(3));
  return sample.speedupNativeVsJavaScript === computed ? computed : null;
}

function benchmarkSpeedup(rows, state) {
  if (!rows.length) return null;
  const samples = rows.map((row) => measuredSpeedup(row?.states?.[state]));
  return samples.every(finiteNonNegative) ? Math.min(...samples) : null;
}

function rowsAtOrAbove(rows, state, threshold) {
  return rows.filter((row) => {
    const speedup = measuredSpeedup(row?.states?.[state]);
    return speedup !== null && speedup >= threshold;
  }).length;
}

/// Decide whether the product may select native as its default core. This gate
/// never enables native by itself: callers must still provide an explicit
/// native implementation and retain automatic JavaScript fallback.
function evaluateNativeDefaultRollout(evidence = {}) {
  const reasons = [];
  const backend = evidence.backendParity || {};
  if (!hasNativeBackendAuthority(backend, evidence.adapterParity)) reasons.push("native-backend-parity-incomplete");
  const structural = evidence.structuralParity || {};
  const queries = evidence.queryParity || {};
  const lifecycle = evidence.lifecycle || {};
  if (structural.publicIds !== true) reasons.push("public-id-parity-not-proven");
  if (!Number.isSafeInteger(structural.fixtureCount) || structural.fixtureCount < 11
    || structural.exactFixtureCount !== structural.fixtureCount) reasons.push("structural-fixture-parity-incomplete");
  const requiredQueries = ["flowLens", "impact", "relatedTests", "contextRef", "changedContexts"];
  for (const query of requiredQueries) if (queries[query] !== true) reasons.push(`query-parity-missing:${query}`);
  if (lifecycle.sqlitePromotion !== true) reasons.push("sqlite-promotion-not-proven");
  if (lifecycle.recovery !== true) reasons.push("sqlite-recovery-not-proven");
  if (lifecycle.javascriptFallback !== true) reasons.push("javascript-fallback-not-proven");

  // Performance has no decision value until native owns the entire backend
  // path. Do not let an attractive wrapper measurement obscure a JavaScript
  // parser/resolver dependency.
  if (reasons.includes("native-backend-parity-incomplete")) {
    return Object.freeze({
      schemaVersion: NATIVE_ROLLOUT_GATE_SCHEMA,
      eligible: false,
      selectedImplementation: "javascript",
      rollback: "automatic-javascript-fallback-required",
      reasons: Object.freeze(reasons),
      backend: Object.freeze({ status: "incomplete", requiredSchema: NATIVE_BACKEND_PARITY_SCHEMA }),
      benchmark: Object.freeze({ status: "blocked-until-native-backend-parity" }),
      limitation: "A Rust store or graph assembler does not satisfy this gate while JavaScript remains on the parser, resolver, or structural-fact production path. Benchmark evidence is intentionally ignored until native backend authority is proven.",
    });
  }

  if (!hasBoundBenchmarkArtifact(evidence.benchmark)) reasons.push("benchmark-artifact-binding-missing");
  const rows = benchmarkRows(evidence.benchmark);
  const cold = benchmarkSpeedup(rows, "cold");
  const unchanged = benchmarkSpeedup(rows, "unchanged");
  const oneFileChange = benchmarkSpeedup(rows, "oneFileChange");
  const oneFileAcceleratedRepositories = rowsAtOrAbove(rows, "oneFileChange", REQUIRED_ONE_FILE_SPEEDUP);
  const repositories = distinctBenchmarkRepositories(rows);
  if (repositories < MINIMUM_BENCHMARK_REPOSITORIES) reasons.push("benchmark-corpus-insufficient");
  if (cold === null || cold < MAXIMUM_REGRESSION_SPEEDUP) reasons.push("cold-benchmark-regression-exceeds-10-percent");
  if (unchanged === null || unchanged < MAXIMUM_REGRESSION_SPEEDUP) reasons.push("unchanged-benchmark-regression-exceeds-10-percent");
  if (oneFileChange === null || oneFileChange < MAXIMUM_REGRESSION_SPEEDUP) reasons.push("one-file-change-benchmark-regression-exceeds-10-percent");
  if (oneFileAcceleratedRepositories < REQUIRED_ONE_FILE_REPOSITORIES) reasons.push("one-file-change-acceleration-insufficient");

  const performance = evidence.performance || {};
  const operationP95Ms = performance.operationP95Ms || {};
  const measuredOperationP95Ms = measuredQueryOperationP95(performance, evidence.benchmark);
  if (!measuredOperationP95Ms) reasons.push("query-raw-samples-not-proven");
  for (const [operation, threshold] of Object.entries(REQUIRED_QUERY_OPERATION_P95_MS)) {
    const value = operationP95Ms[operation];
    if (!finiteNonNegative(value) || value >= threshold
      || measuredOperationP95Ms?.[operation] !== value) {
      reasons.push(`query-operation-p95-not-proven:${operation}`);
    }
  }
  const coreMaximum = Math.max(...Object.keys(REQUIRED_QUERY_OPERATION_P95_MS)
    .filter((operation) => operation !== "resolveContextRef")
    .map((operation) => operationP95Ms[operation]));
  if (!finiteNonNegative(performance.coreQueryP95Ms)
    || performance.coreQueryP95Ms !== coreMaximum
    || performance.coreQueryP95Ms >= 50) reasons.push("core-query-p95-not-proven");
  if (!finiteNonNegative(performance.contextRefP95Ms)
    || performance.contextRefP95Ms !== operationP95Ms.resolveContextRef
    || performance.contextRefP95Ms >= 20) reasons.push("context-ref-p95-not-proven");
  if (!hasDatabaseOpenEvidence(performance)) reasons.push("database-open-behavior-not-proven");
  if (performance.memoryPeakNoWorseThanJavaScript !== true) reasons.push("memory-peak-not-proven");
  const benchmarkArtifact = evidence.benchmark?.nativeArtifact;
  if (!hasNativeDogfoodEvidence(evidence.dogfood, {
    sourceRevision: benchmarkArtifact?.repositoryRevision,
    binarySha256: benchmarkArtifact?.binarySha256,
  })) reasons.push("native-dogfood-window-incomplete");

  return Object.freeze({
    schemaVersion: NATIVE_ROLLOUT_GATE_SCHEMA,
    eligible: reasons.length === 0,
    selectedImplementation: "javascript",
    rollback: "automatic-javascript-fallback-required",
    reasons: Object.freeze(reasons),
    backend: Object.freeze({ status: "complete", requiredSchema: NATIVE_BACKEND_PARITY_SCHEMA }),
    benchmark: Object.freeze({
      repositories,
      cold,
      unchanged,
      oneFileChange,
      oneFileAcceleratedRepositories,
    }),
    limitation: "This gate evaluates supplied static parity, lifecycle, and benchmark evidence. It does not prove runtime behavior or activate a native implementation.",
  });
}

module.exports = {
  MAXIMUM_REGRESSION_SPEEDUP,
  MINIMUM_ADAPTER_CASES,
  MINIMUM_BENCHMARK_REPOSITORIES,
  NATIVE_ROLLOUT_GATE_SCHEMA,
  REQUIRED_ONE_FILE_REPOSITORIES,
  REQUIRED_ONE_FILE_SPEEDUP,
  NATIVE_BACKEND_PARITY_SCHEMA,
  NATIVE_ADAPTER_PARITY_SCHEMA,
  NATIVE_BENCHMARK_SCHEMA,
  REQUIRED_QUERY_OPERATION_P95_MS,
  REQUIRED_NATIVE_ADAPTERS,
  hasNativeDogfoodEvidence,
  evaluateNativeDefaultRollout,
  nativeAdaptersFromParity,
  validateNativeAdapterParity,
};
