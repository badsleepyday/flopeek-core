"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const { adapterContractDigest } = require("../../src/adapter-registry");
const { NATIVE_BENCHMARK_SCHEMA, REQUIRED_NATIVE_ADAPTERS } = require("../../src/native-rollout-gate");
const { REQUIRED_DOGFOOD_SURFACES } = require("../../src/native-dogfood-evidence");
const { CORE_MODE_SCHEMA, CoreModeError, requestedCoreMode, selectCoreMode } = require("../../src/core-mode");
const { createConfiguredCoreClient, createSurfaceCoreClient, createSurfaceCoreRuntime, observeCoreRuntime } = require("../../src/core-runtime");
const { createJsCoreClient } = require("../../src/js-core-client");
const { machineAdapterParityEvidence } = require("../helpers/native-adapter-parity-evidence");

function completeDogfoodEvidence() {
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

function completeEvidence() {
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
  const queryRawSamples = Array.from({ length: 5 }, (_, index) => ({
    repository: `repo-${index + 1}`,
    repositoryRevision: "d".repeat(40),
    sourceDigest: "e".repeat(64),
    states: Object.fromEntries(["cold", "unchanged", "oneFileChange"].map((state) => [
      state,
      Object.fromEntries(Object.entries(operationP95Ms)
        .map(([operation, value]) => [operation, Array(101).fill(value)])),
    ])),
  }));
  return {
    adapterParity: machineAdapterParityEvidence(),
    backendParity: {
      schemaVersion: "flopeek-native-backend-parity/v1",
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
      rows: Array.from({ length: 5 }, (_, index) => ({
      repository: `repo-${index + 1}`,
      repositoryRevision: "d".repeat(40),
      sourceDigest: "e".repeat(64),
      states: {
      cold: { jsSamplesMs: [1, 1, 1], nativeSamplesMs: [1, 1, 1], speedupNativeVsJavaScript: 1 },
      unchanged: { jsSamplesMs: [1, 1, 1], nativeSamplesMs: [1, 1, 1], speedupNativeVsJavaScript: 1 },
      oneFileChange: {
        jsSamplesMs: [index < 4 ? 2 : 1, index < 4 ? 2 : 1, index < 4 ? 2 : 1],
        nativeSamplesMs: [1, 1, 1],
        speedupNativeVsJavaScript: index < 4 ? 2 : 1,
      },
      } })),
    },
    performance: {
      operationP95Ms,
      coreQueryP95Ms: 49,
      contextRefP95Ms: 19,
      queryRawSamples,
      databaseOpenDoesNotDeserializeFullGraph: true,
      databaseOpenEvidence: { sha256: "f".repeat(64), evidence: databaseOpenEvidence },
      memoryPeakNoWorseThanJavaScript: true,
    },
    dogfood: completeDogfoodEvidence(),
  };
}

test("core mode defaults to JavaScript and rejects ambiguous activation", () => {
  assert.equal(requestedCoreMode(undefined), "js");
  assert.throws(() => requestedCoreMode("auto"), CoreModeError);
  const selected = selectCoreMode({ mode: "js" });
  assert.equal(selected.schemaVersion, CORE_MODE_SCHEMA);
  assert.equal(selected.selectedImplementation, "javascript");
  assert.equal(selected.nativeShadow, false);
});

test("shadow mode is explicit while preserving JavaScript as the public implementation", () => {
  const selected = selectCoreMode({ mode: "shadow" });
  assert.equal(selected.requestedMode, "shadow");
  assert.equal(selected.selectedImplementation, "javascript");
  assert.equal(selected.nativeShadow, true);
  assert.equal(selected.fallback, null);
});

test("native request falls back explicitly until the gate and a trusted native core are both available", () => {
  const blocked = selectCoreMode({ mode: "native" });
  assert.equal(blocked.selectedImplementation, "javascript");
  assert.equal(blocked.fallback.reason, "native-rollout-gate-blocked");
  const eligibleButUnavailable = selectCoreMode({ mode: "native", rolloutEvidence: completeEvidence() });
  assert.equal(eligibleButUnavailable.gate.eligible, true);
  assert.equal(eligibleButUnavailable.selectedImplementation, "javascript");
  assert.equal(eligibleButUnavailable.fallback.reason, "native-public-core-unavailable");
  const eligible = selectCoreMode({ mode: "native", rolloutEvidence: completeEvidence(), nativeAvailable: true });
  assert.equal(eligible.selectedImplementation, "native");
  assert.equal(eligible.fallback.reason, "native-runtime-fallback-required");
});

test("native experimental is an explicit dogfood selection, never a rollout-approved native default", async () => {
  const nativeCore = { ...createJsCoreClient(), implementation: "native-experimental", backendAuthority: "rust-sqlite" };
  const runtime = createSurfaceCoreRuntime({ coreMode: "native-experimental", nativeCore });
  assert.equal(runtime.selection.requestedMode, "native-experimental");
  assert.equal(runtime.selection.selectedImplementation, "native");
  assert.equal(runtime.selection.experimental, true);
  assert.equal(runtime.selection.gate.eligible, false);
  assert.equal(runtime.core.implementation, "native-experimental");
  await runtime.core.close();
});

test("configured core activates shadow only through the supplied native protocol client", async () => {
  const native = {
    start: async () => {},
    request: async () => ({ nodes: [], edges: [] }),
    close: async () => {},
  };
  const shadow = createConfiguredCoreClient({ mode: "shadow", native });
  assert.equal(shadow.implementation, "shadow");
  await shadow.close();
  const fallback = createConfiguredCoreClient({ mode: "native" });
  assert.equal(fallback.implementation, "javascript");
  await fallback.close();
});

test("configured core selects an explicitly supplied native client only after the complete gate", async () => {
  const javascript = createJsCoreClient();
  const nativeCore = { ...javascript, implementation: "native-experimental" };
  const selected = createConfiguredCoreClient({
    mode: "native",
    rolloutEvidence: completeEvidence(),
    nativeCore,
  });
  assert.equal(selected.implementation, "native-experimental");
  await selected.close();
});

test("strict native activation never constructs or reads a JavaScript rollback authority", async () => {
  const nativeCore = { ...createJsCoreClient(), implementation: "native-experimental", backendAuthority: "rust-sqlite" };
  const options = {
    mode: "native",
    rolloutEvidence: completeEvidence(),
    nativeCore,
    strictNative: true,
  };
  Object.defineProperty(options, "javascript", {
    get() { throw new Error("strict native must not read JavaScript authority"); },
  });
  const selected = createConfiguredCoreClient(options);
  assert.equal(selected, nativeCore);
  assert.equal(selected.backendAuthority, "rust-sqlite");
  await selected.close();
  assert.throws(
    () => createConfiguredCoreClient({ mode: "native", strictNative: true }),
    (error) => error.code === "strict-native-unavailable",
  );
});

test("configured native core falls back to JavaScript only before native authority exists", async () => {
  const javascript = {
    ...createJsCoreClient(),
    scan: async () => ({ project: { projectId: "project:js-fallback" } }),
  };
  const nativeCore = {
    ...createJsCoreClient(),
    implementation: "native-experimental",
    scan: async () => { throw new Error("native bootstrap failed"); },
  };
  const selected = createConfiguredCoreClient({
    mode: "native",
    rolloutEvidence: completeEvidence(),
    nativeCore,
    javascript,
  });
  const graph = await selected.scan("ignored");
  assert.equal(graph.project.projectId, "project:js-fallback");
  assert.equal(selected.implementation, "javascript");
  assert.deepEqual(selected.fallback, { active: true, reason: "native-bootstrap-failed-before-authority" });
  await selected.close();
});

test("surface runtime records an actual bootstrap fallback instead of only its native preflight selection", async () => {
  const javascript = {
    ...createJsCoreClient(),
    scan: async () => ({ project: { projectId: "project:js-fallback" } }),
  };
  const nativeCore = {
    ...javascript,
    implementation: "native-experimental",
    sourceAuthority: "rust",
    scan: async () => { throw new Error("native bootstrap failed"); },
  };
  const runtime = createSurfaceCoreRuntime({
    coreMode: "native-experimental",
    nativeCore,
    javascript,
  });
  await runtime.core.scan("ignored");
  const observed = observeCoreRuntime(runtime.selection, runtime.core);
  assert.equal(observed.requestedMode, "native-experimental");
  assert.equal(observed.policySelectedImplementation, "native");
  assert.equal(observed.selectedImplementation, "javascript");
  assert.equal(observed.sourceAuthority, null);
  assert.deepEqual(observed.execution, {
    selectedImplementation: "javascript",
    sourceAuthority: null,
    parserHost: null,
    factEnvelopeHost: null,
    fallback: { active: true, reason: "native-bootstrap-failed-before-authority" },
  });
  assert.deepEqual(observed.fallback, {
    reason: "native-bootstrap-failed-before-authority",
    required: "automatic-javascript-fallback-required",
    gateReasons: runtime.selection.gate.reasons,
    active: true,
  });
  await runtime.core.close();
});

test("surface runtime exposes strict Rust source authority in its execution record", async () => {
  const nativeCore = {
    ...createJsCoreClient(),
    implementation: "native-experimental",
    sourceAuthority: "rust",
    parserHost: "rust-tree-sitter-source/v19",
    factEnvelopeHost: "rust-native-structural-batch/v1",
  };
  const runtime = createSurfaceCoreRuntime({ coreMode: "native-experimental", nativeCore });
  const observed = observeCoreRuntime(runtime.selection, runtime.core);
  assert.equal(observed.selectedImplementation, "native");
  assert.deepEqual(observed.execution, {
    selectedImplementation: "native",
    sourceAuthority: "rust",
    parserHost: "rust-tree-sitter-source/v19",
    factEnvelopeHost: "rust-native-structural-batch/v1",
    fallback: { active: false, reason: null },
  });
  await runtime.core.close();
});

test("configured native core does not create a JavaScript authority after native promotion", async () => {
  const javascript = {
    ...createJsCoreClient(),
    refresh: async () => { throw new Error("JavaScript fallback must not run after native promotion"); },
  };
  const graph = { project: { projectId: "project:native" } };
  const nativeCore = {
    ...createJsCoreClient(),
    implementation: "native-experimental",
    scan: async () => graph,
    refresh: async () => { throw new Error("native refresh failed after promotion"); },
  };
  const selected = createConfiguredCoreClient({
    mode: "native",
    rolloutEvidence: completeEvidence(),
    nativeCore,
    javascript,
  });
  assert.equal(await selected.scan("ignored"), graph);
  await assert.rejects(() => selected.refresh("ignored"), /native refresh failed after promotion/);
  assert.equal(selected.implementation, "native-experimental");
  assert.deepEqual(selected.fallback, { active: false, reason: null });
  await selected.close();
});

test("ephemeral native scans never open durable authority state", async () => {
  let authorityReads = 0;
  const graph = { project: { projectId: "session:ephemeral-native" } };
  const nativeCore = {
    ...createJsCoreClient(),
    implementation: "native-experimental",
    getLastCompleteGraph: async () => {
      authorityReads += 1;
      throw new Error("ephemeral scans must not read SQLite authority");
    },
    scan: async () => graph,
  };
  const selected = createConfiguredCoreClient({
    mode: "native",
    rolloutEvidence: completeEvidence(),
    nativeCore,
    javascript: createJsCoreClient(),
  });
  assert.equal(await selected.scan("ignored", { persistIdentity: false }), graph);
  assert.equal(authorityReads, 0);
  assert.equal(selected.authorityState, "javascript");
  assert.deepEqual(selected.fallback, { active: false, reason: null });
  await selected.close();
});

test("mutating timeout falls back only after SQLite proves no native promotion", async () => {
  let authorityReads = 0;
  let javascriptScans = 0;
  const javascript = {
    ...createJsCoreClient(),
    scan: async () => {
      javascriptScans += 1;
      return { project: { projectId: "project:javascript-after-proven-rollback" } };
    },
  };
  const nativeCore = {
    ...createJsCoreClient(),
    implementation: "native-experimental",
    getLastCompleteGraph: async () => {
      authorityReads += 1;
      return null;
    },
    scan: async () => {
      const error = new Error("native request timed out before promotion");
      error.code = "request-timeout";
      error.nativeAuthorityMutation = true;
      throw error;
    },
  };
  const selected = createConfiguredCoreClient({
    mode: "native",
    rolloutEvidence: completeEvidence(),
    nativeCore,
    javascript,
  });
  const graph = await selected.scan("ignored");
  assert.equal(graph.project.projectId, "project:javascript-after-proven-rollback");
  assert.equal(authorityReads, 2);
  assert.equal(javascriptScans, 1);
  assert.equal(selected.authorityState, "javascript");
  assert.deepEqual(selected.fallback, { active: true, reason: "native-mutation-failed-before-promotion" });
  await selected.close();
});

test("mutating timeout after commit recovers SQLite authority without running JavaScript", async () => {
  let authorityReads = 0;
  let javascriptScans = 0;
  const recovered = { project: { projectId: "project:native-late-commit" }, state: { graphVersion: 1, status: "native-last-complete" } };
  const javascript = {
    ...createJsCoreClient(),
    scan: async () => {
      javascriptScans += 1;
      throw new Error("JavaScript must not run after a late native commit");
    },
  };
  const nativeCore = {
    ...createJsCoreClient(),
    implementation: "native-experimental",
    getLastCompleteGraph: async () => {
      authorityReads += 1;
      return authorityReads === 1 ? null : recovered;
    },
    scan: async () => {
      const error = new Error("native response timed out after promotion");
      error.code = "request-timeout";
      error.nativeAuthorityMutation = true;
      throw error;
    },
  };
  const selected = createConfiguredCoreClient({
    mode: "native",
    rolloutEvidence: completeEvidence(),
    nativeCore,
    javascript,
  });
  assert.equal(await selected.scan("ignored"), recovered);
  assert.equal(authorityReads, 2);
  assert.equal(javascriptScans, 0);
  assert.equal(selected.authorityState, "native-authoritative");
  assert.deepEqual(selected.fallback, { active: false, reason: null });
  await selected.close();
});

test("failed SQLite recovery keeps authority unknown and blocks JavaScript", async () => {
  let authorityReads = 0;
  let javascriptScans = 0;
  const javascript = {
    ...createJsCoreClient(),
    scan: async () => {
      javascriptScans += 1;
      throw new Error("JavaScript must remain blocked while native authority is unknown");
    },
  };
  const nativeCore = {
    ...createJsCoreClient(),
    implementation: "native-experimental",
    getLastCompleteGraph: async () => {
      authorityReads += 1;
      if (authorityReads === 1) return null;
      throw new Error("SQLite recovery unavailable");
    },
    scan: async () => {
      const error = new Error("native request timed out");
      error.code = "request-timeout";
      error.nativeAuthorityMutation = true;
      throw error;
    },
  };
  const selected = createConfiguredCoreClient({
    mode: "native",
    rolloutEvidence: completeEvidence(),
    nativeCore,
    javascript,
  });
  await assert.rejects(() => selected.scan("ignored"), (error) => {
    assert.equal(error.code, "native-authority-unknown");
    assert.equal(error.triggerCode, "request-timeout");
    assert.match(error.recoveryError.message, /SQLite recovery unavailable/);
    return true;
  });
  assert.equal(authorityReads, 2);
  assert.equal(javascriptScans, 0);
  assert.equal(selected.authorityState, "native-authority-unknown");
  assert.deepEqual(selected.fallback, { active: false, reason: null });
  await selected.close();
});

test("surface presentation mode cannot accidentally select a core implementation", async () => {
  const client = createSurfaceCoreClient({ mode: "overview" });
  assert.equal(client.implementation, "javascript");
  await client.close();
});
