"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { adapterContractDigest } = require("./adapter-registry");
const { validateNativeAdapterParity } = require("./native-rollout-gate");
const {
  readPlatformNativePackageMetadata,
  resolvePlatformNativeBinary,
  verifyPlatformNativeBinary,
} = require("./native-incremental-coordinator");
const { NATIVE_PROTOCOL_VERSION } = require("./native-protocol-client");

const NATIVE_ROLLOUT_EVIDENCE_SCHEMA = "flopeek-native-rollout-evidence/v2";
const NATIVE_DATABASE_OPEN_EVIDENCE_SCHEMA = "flopeek-native-database-open-evidence/v1";
const NATIVE_DATABASE_OPEN_OBSERVATION_SCHEMA = "flopeek-native-database-open-observation/v1";

function validSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/u.test(value);
}

function exactKeys(value, expected, field) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${field} must be an object.`);
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
    throw new Error(`${field} must contain exactly: ${wanted.join(", ")}.`);
  }
}

function validateDatabaseOpenEvidence(evidence, binaryBindings) {
  exactKeys(evidence, [
    "schemaVersion",
    "platformPackage",
    "repositoryRevision",
    "sourceDigest",
    "binarySha256",
    "operation",
    "fullPayloadDeserialized",
    "observations",
  ], "database-open evidence");
  if (evidence.schemaVersion !== NATIVE_DATABASE_OPEN_EVIDENCE_SCHEMA) {
    throw new Error(`Database-open evidence must use ${NATIVE_DATABASE_OPEN_EVIDENCE_SCHEMA}.`);
  }
  if (evidence.operation !== "open-current-graph" || evidence.fullPayloadDeserialized !== false) {
    throw new Error("Database-open evidence must report the exact open-current-graph operation without full payload deserialization.");
  }
  if (typeof evidence.platformPackage !== "string" || !evidence.platformPackage
    || !/^[a-f0-9]{40,64}$/u.test(evidence.repositoryRevision || "")
    || !validSha256(evidence.sourceDigest)
    || !validSha256(evidence.binarySha256)) {
    throw new Error("Database-open evidence must be bound to a platform package, repository revision, source digest, and binary SHA-256.");
  }
  exactKeys(evidence.observations, [
    "schemaVersion",
    "sqliteOperations",
    "currentGraphFound",
    "graphPayloadRowsRead",
    "graphPayloadBytesDeserialized",
  ], "database-open observations");
  const observations = evidence.observations;
  if (observations.schemaVersion !== NATIVE_DATABASE_OPEN_OBSERVATION_SCHEMA
    || !Array.isArray(observations.sqliteOperations)
    || observations.sqliteOperations.length !== 1
    || observations.sqliteOperations[0] !== "current-complete-graph-metadata"
    || observations.currentGraphFound !== true
    || observations.graphPayloadRowsRead !== 0
    || observations.graphPayloadBytesDeserialized !== 0) {
    throw new Error("Database-open observations do not prove a metadata-only current-graph read.");
  }
  const binding = binaryBindings?.[evidence.platformPackage];
  if (!binding
    || binding.binarySha256 !== evidence.binarySha256
    || binding.repositoryRevision !== evidence.repositoryRevision
    || binding.sourceDigest !== evidence.sourceDigest) {
    throw new Error("Database-open evidence does not match the exact release binary and source revision.");
  }
  return evidence;
}

function loadDatabaseOpenEvidence(file, binaryBindings, readFile = fs.readFileSync) {
  const bytes = readFile(file);
  let evidence;
  try {
    evidence = JSON.parse(bytes.toString("utf8"));
  } catch (error) {
    throw new Error(`Database-open evidence is not valid JSON: ${error.message}`);
  }
  validateDatabaseOpenEvidence(evidence, binaryBindings);
  return {
    evidence,
    sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
  };
}

function validBinaryBinding(value, repositoryRevision, sourceDigest) {
  return value && typeof value === "object" && !Array.isArray(value)
    && validSha256(value.binarySha256)
    && validSha256(value.tarballSha256)
    && value.repositoryRevision === repositoryRevision
    && value.sourceDigest === sourceDigest
    && typeof value.target === "string" && value.target.length > 0
    && typeof value.compiler?.version === "string" && value.compiler.version.length > 0;
}

function loadBundledNativeRolloutEvidence(root = path.resolve(__dirname, ".."), options = {}) {
  const readFile = options.readFile || fs.readFileSync;
  const packageJson = JSON.parse(readFile(path.join(root, "package.json"), "utf8"));
  const packet = JSON.parse(readFile(path.join(root, "packaging", "native-rollout-evidence.json"), "utf8"));
  if (!packet || packet.schemaVersion !== NATIVE_ROLLOUT_EVIDENCE_SCHEMA
    || !["incomplete", "blocked", "complete"].includes(packet.status)
    || !packet.binding || typeof packet.binding !== "object") {
    throw new Error(`Bundled native rollout evidence must use ${NATIVE_ROLLOUT_EVIDENCE_SCHEMA}.`);
  }
  const bindingMatches = packet.binding.packageName === packageJson.name
    && packet.binding.packageVersion === packageJson.version
    && packet.binding.adapterContractDigest === adapterContractDigest()
    && packet.binding.protocolVersion === NATIVE_PROTOCOL_VERSION;
  if (!bindingMatches) throw new Error("Bundled native rollout evidence does not match this package, adapter contract, and protocol.");
  if (packet.status === "incomplete") {
    if (packet.evidence !== null || packet.binding.binaries !== null
      || packet.binding.repositoryRevision !== null || packet.binding.sourceDigest !== null) {
      throw new Error("Incomplete native rollout evidence must not carry decision evidence or binary bindings.");
    }
    return Object.freeze({ packet, evidence: Object.freeze({}), complete: false });
  }
  if (packet.status === "blocked"
    && (packet.decision?.eligible !== false
      || !Array.isArray(packet.decision?.reasons)
      || packet.decision.reasons.length === 0)) {
    throw new Error("Blocked native rollout evidence must retain an explicit ineligible decision and gate reasons.");
  }
  if (packet.status === "complete" && packet.decision && packet.decision.eligible !== true) {
    throw new Error("Complete native rollout evidence cannot carry an ineligible decision.");
  }
  const binaries = packet.binding.binaries;
  const repositoryRevision = packet.binding.repositoryRevision;
  const sourceDigest = packet.binding.sourceDigest;
  const benchmarkArtifact = packet.evidence?.benchmark?.nativeArtifact;
  const benchmarkBinding = binaries?.[benchmarkArtifact?.platformPackage];
  if (!packet.evidence || !binaries || typeof binaries !== "object" || Array.isArray(binaries)
    || typeof repositoryRevision !== "string" || !/^[a-f0-9]{40,64}$/u.test(repositoryRevision)
    || !validSha256(sourceDigest)
    || Object.keys(binaries).length !== Object.keys(packageJson.optionalDependencies || {}).length
    || Object.entries(packageJson.optionalDependencies || {})
      .some(([name]) => !validBinaryBinding(binaries[name], repositoryRevision, sourceDigest))
    || packet.evidence.benchmark?.schemaVersion !== "flopeek-native-core-client-benchmark/v2"
    || !benchmarkArtifact || !validBinaryBinding(benchmarkBinding, repositoryRevision, sourceDigest)
    || benchmarkBinding.binarySha256 !== benchmarkArtifact.binarySha256
    || benchmarkBinding.target !== benchmarkArtifact.target
    || benchmarkBinding.compiler.version !== benchmarkArtifact.compilerVersion) {
    throw new Error("Complete native rollout evidence requires exact revision, source, compiler, target, tarball, and binary bindings for every platform.");
  }
  try {
    validateNativeAdapterParity(packet.evidence.adapterParity);
  } catch (error) {
    throw new Error(`Complete native rollout evidence has invalid adapter parity: ${error.message}`);
  }
  const parityBinary = binaries["@flopeek/native-linux-x64-gnu"];
  if (!parityBinary
    || packet.evidence.adapterParity.binary.sha256 !== parityBinary.binarySha256
    || packet.evidence.adapterParity.binary.sourceRevision !== parityBinary.repositoryRevision) {
    throw new Error("Complete native rollout evidence adapter parity does not match the exact Linux x64 release binary and source revision.");
  }
  const databaseOpen = packet.evidence.performance?.databaseOpenEvidence;
  if (!validSha256(databaseOpen?.sha256)) {
    throw new Error("Complete native rollout evidence requires a SHA-256-bound database-open evidence file.");
  }
  try {
    validateDatabaseOpenEvidence(databaseOpen.evidence, binaries);
  } catch (error) {
    throw new Error(`Complete native rollout evidence has invalid database-open evidence: ${error.message}`);
  }
  return Object.freeze({
    packet,
    evidence: Object.freeze(packet.evidence),
    complete: packet.status === "complete",
  });
}

function probeVerifiedNativeRuntime(root = path.resolve(__dirname, ".."), options = {}) {
  const packageJson = JSON.parse((options.readFile || fs.readFileSync)(path.join(root, "package.json"), "utf8"));
  const binary = (options.resolveBinary || resolvePlatformNativeBinary)();
  const metadata = (options.readMetadata || readPlatformNativePackageMetadata)();
  const expectedPackageVersion = metadata?.packageName
    ? packageJson.optionalDependencies?.[metadata.packageName]
    : null;
  const integrityVerified = Boolean(binary && metadata
    && metadata.version === packageJson.version
    && expectedPackageVersion === packageJson.version
    && (options.verifyBinary || verifyPlatformNativeBinary)(binary, metadata));
  const expected = options.expectedBinaries?.[metadata?.packageName];
  const evidenceVerified = integrityVerified && (!options.expectedBinaries
    || (expected?.binarySha256 === metadata.binarySha256
      && expected.repositoryRevision === metadata.repositoryRevision
      && expected.sourceDigest === metadata.sourceDigest
      && expected.target === metadata.target
      && expected.compiler?.version === metadata.compiler?.version));
  return Object.freeze({
    available: evidenceVerified,
    binary: evidenceVerified ? binary : null,
    packageName: metadata?.packageName || null,
    packageVersion: metadata?.version || null,
    binarySha256: metadata?.binarySha256 || null,
    protocolVersion: NATIVE_PROTOCOL_VERSION,
    reason: evidenceVerified
      ? null
      : integrityVerified
        ? "native-runtime-not-bound-to-rollout-evidence"
        : "verified-platform-native-runtime-unavailable",
  });
}

module.exports = {
  NATIVE_DATABASE_OPEN_EVIDENCE_SCHEMA,
  NATIVE_DATABASE_OPEN_OBSERVATION_SCHEMA,
  NATIVE_ROLLOUT_EVIDENCE_SCHEMA,
  loadDatabaseOpenEvidence,
  loadBundledNativeRolloutEvidence,
  probeVerifiedNativeRuntime,
  validateDatabaseOpenEvidence,
};
