"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const { adapterContractDigest } = require("../../src/adapter-registry");
const { machineAdapterParityEvidence } = require("../helpers/native-adapter-parity-evidence");
const {
  NATIVE_BACKEND_PARITY_SCHEMA,
  NATIVE_BENCHMARK_SCHEMA,
  NATIVE_ROLLOUT_GATE_SCHEMA,
  REQUIRED_NATIVE_ADAPTERS,
  evaluateNativeDefaultRollout,
} = require("../../src/native-rollout-gate");
const {
  REQUIRED_DOGFOOD_SURFACES,
} = require("../../src/native-dogfood-evidence");

function benchmarkRow(repository, { cold = 1.1, unchanged = 1.2, oneFileChange = 2.1 } = {}) {
  const sample = (speedup) => ({
    jsSamplesMs: [speedup, speedup, speedup],
    nativeSamplesMs: [1, 1, 1],
    speedupNativeVsJavaScript: speedup,
  });
  return {
    repository,
    repositoryRevision: "d".repeat(40),
    sourceDigest: "e".repeat(64),
    states: {
      cold: sample(cold),
      unchanged: sample(unchanged),
      oneFileChange: sample(oneFileChange),
    },
  };
}

function queryRawSamples(operationP95Ms) {
  return Array.from({ length: 5 }, (_, index) => ({
    repository: `repo-${index + 1}`,
    repositoryRevision: "d".repeat(40),
    sourceDigest: "e".repeat(64),
    states: Object.fromEntries(["cold", "unchanged", "oneFileChange"].map((state) => [
      state,
      Object.fromEntries(Object.entries(operationP95Ms)
        .map(([operation, value]) => [operation, Array(101).fill(value)])),
    ])),
  }));
}

function dogfoodEvidence() {
  const days = Array.from({ length: 7 }, (_, index) => {
    const date = `2026-01-${String(index + 1).padStart(2, "0")}`;
    return {
      date,
      startedAt: `${date}T01:00:00.000Z`,
      completedAt: `${date}T02:00:00.000Z`,
      sourceRevision: "b".repeat(40),
      binarySha256: "a".repeat(64),
      status: "passed",
      repositories: 8,
      exactRepositories: 8,
      adapters: [...REQUIRED_NATIVE_ADAPTERS],
      targetRepositoryWrites: false,
      surfaces: { ...REQUIRED_DOGFOOD_SURFACES },
      evidenceSha256: "f".repeat(64),
    };
  });
  return {
    schemaVersion: "flopeek-native-dogfood-evidence/v1",
    status: "complete",
    requiredDays: 7,
    sourceRevision: "b".repeat(40),
    binarySha256: "a".repeat(64),
    generatedAt: "2026-01-08T00:00:00.000Z",
    days,
    summary: {
      distinctDays: 7,
      repositories: 8,
      exactRepositories: 8,
      adapters: [...REQUIRED_NATIVE_ADAPTERS],
      targetRepositoryWrites: false,
      surfaces: { ...REQUIRED_DOGFOOD_SURFACES },
    },
  };
}

function evidence(overrides = {}) {
  const databaseOpenEvidence = {
    schemaVersion: "flopeek-native-database-open-evidence/v1",
    platformPackage: "@flopeek/native-linux-x64-gnu",
    repositoryRevision: "b".repeat(40),
    sourceDigest: "c".repeat(64),
    binarySha256: "a".repeat(64),
    operation: "open-current-graph",
    fullPayloadDeserialized: false,
    observations: {
      schemaVersion: "flopeek-native-database-open-observation/v1",
      sqliteOperations: ["current-complete-graph-metadata"],
      currentGraphFound: true,
      graphPayloadRowsRead: 0,
      graphPayloadBytesDeserialized: 0,
    },
  };
  const operationP95Ms = {
    findNodes: 49,
    projectOverview: 49,
    contextCard: 49,
    flowProjection: 49,
    resolveContextRef: 19,
  };
  return {
    adapterParity: machineAdapterParityEvidence(),
    backendParity: {
      schemaVersion: NATIVE_BACKEND_PARITY_SCHEMA,
      sourceDiscoveryAuthority: "rust",
      parserAuthority: "rust",
      resolverAuthority: "rust",
      structuralFactAuthority: "rust",
      javascriptRole: "oracle-and-rollback-only",
      fixtureCount: 1,
      exactFixtureCount: 1,
      adapterContractDigest: adapterContractDigest(),
      requiredAdapters: REQUIRED_NATIVE_ADAPTERS,
      nativeAdapters: REQUIRED_NATIVE_ADAPTERS,
      fallbackOnlyAdapters: [],
      adapterCoveragePolicy: "all-native",
    },
    structuralParity: { publicIds: true, fixtureCount: 11, exactFixtureCount: 11 },
    queryParity: { flowLens: true, impact: true, relatedTests: true, contextRef: true, changedContexts: true },
    lifecycle: { sqlitePromotion: true, recovery: true, javascriptFallback: true },
    benchmark: {
      schemaVersion: NATIVE_BENCHMARK_SCHEMA,
      nativeArtifact: {
        binarySha256: "a".repeat(64),
        platformPackage: "@flopeek/native-linux-x64-gnu",
        target: "x86_64-unknown-linux-gnu",
        compilerVersion: "rustc 1.2.3",
        repositoryRevision: "b".repeat(40),
        sourceDigest: "c".repeat(64),
      },
      rows: Array.from({ length: 5 }, (_, index) => benchmarkRow(`repo-${index + 1}`)),
    },
    performance: {
      operationP95Ms,
      coreQueryP95Ms: 49,
      contextRefP95Ms: 19,
      queryRawSamples: queryRawSamples(operationP95Ms),
      databaseOpenDoesNotDeserializeFullGraph: true,
      databaseOpenEvidence: { sha256: "f".repeat(64), evidence: databaseOpenEvidence },
      memoryPeakNoWorseThanJavaScript: true,
    },
    dogfood: dogfoodEvidence(),
    ...overrides,
  };
}

test("native rollout gate permits only complete parity and non-regressing benchmarks", () => {
  const result = evaluateNativeDefaultRollout(evidence());
  assert.equal(result.schemaVersion, NATIVE_ROLLOUT_GATE_SCHEMA);
  assert.equal(result.eligible, true);
  assert.equal(result.selectedImplementation, "javascript", "the gate does not silently activate native");
  assert.deepEqual(result.reasons, []);
});

test("native rollout gate keeps JavaScript authoritative when cold timing regresses", () => {
  const result = evaluateNativeDefaultRollout(evidence({ benchmark: {
    ...evidence().benchmark,
    rows: Array.from({ length: 5 }, (_, index) => benchmarkRow(`repo-${index + 1}`, { cold: 0.813, unchanged: 1.37, oneFileChange: 2.2 })),
  } }));
  assert.equal(result.eligible, false);
  assert.equal(result.selectedImplementation, "javascript");
  assert.ok(result.reasons.includes("cold-benchmark-regression-exceeds-10-percent"));
  assert.equal(result.rollback, "automatic-javascript-fallback-required");
});

test("native rollout gate rejects a reported speedup that contradicts raw samples", () => {
  const rows = Array.from({ length: 5 }, (_, index) => benchmarkRow(`repo-${index + 1}`));
  rows[0].states.oneFileChange.speedupNativeVsJavaScript = 100;
  const result = evaluateNativeDefaultRollout(evidence({ benchmark: { ...evidence().benchmark, rows } }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("one-file-change-benchmark-regression-exceeds-10-percent"));
});

test("native rollout gate rejects benchmark rows without artifact-bound schema v2", () => {
  const result = evaluateNativeDefaultRollout(evidence({
    benchmark: { rows: evidence().benchmark.rows },
  }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("benchmark-artifact-binding-missing"));
});

test("native rollout gate blocks benchmark evidence while JavaScript remains the parser host", () => {
  const result = evaluateNativeDefaultRollout(evidence({ backendParity: {
    schemaVersion: NATIVE_BACKEND_PARITY_SCHEMA,
    sourceDiscoveryAuthority: "rust",
    parserAuthority: "javascript",
    resolverAuthority: "javascript",
    structuralFactAuthority: "javascript",
    javascriptRole: "production-parser-host",
    fixtureCount: 1,
    exactFixtureCount: 1,
    adapterContractDigest: adapterContractDigest(),
    requiredAdapters: REQUIRED_NATIVE_ADAPTERS,
    nativeAdapters: REQUIRED_NATIVE_ADAPTERS,
    fallbackOnlyAdapters: [],
    adapterCoveragePolicy: "all-native",
  } }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("native-backend-parity-incomplete"));
  assert.equal(result.benchmark.status, "blocked-until-native-backend-parity");
});

test("native rollout gate is bound to exact adapter contract coverage", () => {
  const adapterParity = machineAdapterParityEvidence();
  delete adapterParity.adapters.go;
  const result = evaluateNativeDefaultRollout(evidence({ adapterParity }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("native-backend-parity-incomplete"));
});

test("native rollout gate cannot self-assert adapters through backend candidate metadata", () => {
  const value = evidence();
  delete value.adapterParity;
  value.backendParity.nativeAdapters = REQUIRED_NATIVE_ADAPTERS;
  const result = evaluateNativeDefaultRollout(value);
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("native-backend-parity-incomplete"));
});

test("native rollout gate derives adapters from machine parity instead of a candidate array", () => {
  const value = evidence();
  value.backendParity.nativeAdapters = ["candidate-self-assertion-is-ignored"];
  const result = evaluateNativeDefaultRollout(value);
  assert.equal(result.eligible, true);
  assert.deepEqual(result.reasons, []);
});

test("native rollout gate rejects a duplicated adapter that replaces a required adapter", () => {
  const adapterParity = machineAdapterParityEvidence();
  const adapter = adapterParity.adapters.go;
  adapter.records[1].sourceDigest = adapter.records[0].sourceDigest;
  adapter.sourceDigests[1] = adapter.sourceDigests[0];
  const result = evaluateNativeDefaultRollout(evidence({ adapterParity }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("native-backend-parity-incomplete"));
});

test("native rollout gate rejects incomplete corpus and unproven performance evidence", () => {
  const result = evaluateNativeDefaultRollout(evidence({
    benchmark: { ...evidence().benchmark, rows: Array.from({ length: 4 }, (_, index) => benchmarkRow(`repo-${index + 1}`, { oneFileChange: 1.2 })) },
    performance: {},
  }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("benchmark-corpus-insufficient"));
  assert.ok(result.reasons.includes("one-file-change-acceleration-insufficient"));
  assert.ok(result.reasons.includes("core-query-p95-not-proven"));
  assert.ok(result.reasons.includes("context-ref-p95-not-proven"));
  assert.ok(result.reasons.includes("query-raw-samples-not-proven"));
  assert.ok(result.reasons.includes("query-operation-p95-not-proven:flowProjection"));
  assert.ok(result.reasons.includes("database-open-behavior-not-proven"));
  assert.ok(result.reasons.includes("memory-peak-not-proven"));
});

test("native rollout gate rejects one hidden slow operation even when supplied aggregates look fast", () => {
  const performance = {
    ...evidence().performance,
    operationP95Ms: {
      ...evidence().performance.operationP95Ms,
      flowProjection: 200,
    },
    coreQueryP95Ms: 10,
  };
  const result = evaluateNativeDefaultRollout(evidence({ performance }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("query-operation-p95-not-proven:flowProjection"));
  assert.ok(result.reasons.includes("core-query-p95-not-proven"));
});

test("native rollout gate rejects a fast summary that contradicts retained raw samples", () => {
  const performance = {
    ...evidence().performance,
    operationP95Ms: {
      ...evidence().performance.operationP95Ms,
      findNodes: 1,
    },
    coreQueryP95Ms: 49,
  };
  const result = evaluateNativeDefaultRollout(evidence({ performance }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("query-operation-p95-not-proven:findNodes"));
});

test("native rollout gate rejects raw query samples rebound to another repository revision", () => {
  const querySamples = structuredClone(evidence().performance.queryRawSamples);
  querySamples[0].repositoryRevision = "f".repeat(40);
  const result = evaluateNativeDefaultRollout(evidence({
    performance: { ...evidence().performance, queryRawSamples: querySamples },
  }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("query-raw-samples-not-proven"));
});

test("native rollout gate requires five distinct benchmark repositories", () => {
  const result = evaluateNativeDefaultRollout(evidence({
    benchmark: { ...evidence().benchmark, rows: Array.from({ length: 5 }, () => benchmarkRow("repeated-repository")) },
  }));
  assert.equal(result.eligible, false);
  assert.equal(result.benchmark.repositories, 1);
  assert.ok(result.reasons.includes("benchmark-corpus-insufficient"));
});

test("native rollout gate rejects a missing or incomplete elapsed dogfood window", () => {
  const result = evaluateNativeDefaultRollout(evidence({ dogfood: {
    ...dogfoodEvidence(),
    status: "pending",
    days: [],
    summary: {
      distinctDays: 0,
      repositories: 0,
      exactRepositories: 0,
      adapters: [],
      targetRepositoryWrites: false,
      surfaces: { cliCommands: 0, mcpTools: 0, httpRoutes: 0, unclassified: 0 },
    },
  } }));
  assert.equal(result.eligible, false);
  assert.ok(result.reasons.includes("native-dogfood-window-incomplete"));
});
