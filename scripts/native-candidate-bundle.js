"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const {
  loadNativeReleaseManifest,
  sha256File,
} = require("./native-release-manifest");
const { adapterContractDigest } = require("../src/adapter-registry");
const { NATIVE_PROTOCOL_VERSION } = require("../src/native-protocol-client");

const NATIVE_CANDIDATE_METADATA_SCHEMA = "flopeek-native-candidate/v1";
const NATIVE_PROMOTION_APPROVAL_SCHEMA = "flopeek-native-release-approval/v1";
const CANDIDATE_STATUS = "candidate-ready";
const RELEASE_CHANNELS = Object.freeze(["alpha", "beta", "rc", "latest"]);
const REQUIRED_BUNDLE_ENTRIES = Object.freeze([
  "adapter-parity.json",
  "benchmark.json",
  "candidate-metadata.json",
  "checksums.json",
  "database-open-evidence.json",
  "flopeek-main.tgz",
  "native-release-manifest.json",
  "native-rollout-evidence.json",
  "native-dogfood.json",
  "native-surface-matrix.json",
  "native-soak.json",
  "profiles",
  "real-corpus.json",
  "test-summary.json",
]);

function requiredText(value, field) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${field} must be a non-empty string.`);
  return value;
}

function exactKeys(value, expected, field) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${field} must be an object.`);
  const actual = Object.keys(value);
  const missing = expected.filter((key) => !actual.includes(key));
  const unknown = actual.filter((key) => !expected.includes(key));
  if (missing.length) throw new Error(`${field} is missing fields: ${missing.join(", ")}.`);
  if (unknown.length) throw new Error(`${field} contains unknown fields: ${unknown.join(", ")}.`);
}

function validSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/u.test(value);
}

function validateCandidateInputs({ sourceSha, packageVersion, channel }) {
  if (!/^[a-f0-9]{40}$/u.test(sourceSha || "")) throw new Error("source SHA must be an exact lowercase 40-character Git commit.");
  if (!/^\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.\d+)?$/u.test(packageVersion || "")) {
    throw new Error("package version must be an exact supported semantic version.");
  }
  if (!RELEASE_CHANNELS.includes(channel)) {
    throw new Error(`release channel must be one of: ${RELEASE_CHANNELS.join(", ")}.`);
  }
  const prerelease = packageVersion.match(/-(alpha|beta|rc)\./u)?.[1] || null;
  const expectedChannel = prerelease || "latest";
  if (channel !== expectedChannel) {
    throw new Error(`release channel ${channel} does not match package version channel ${expectedChannel}.`);
  }
  return { sourceSha, packageVersion, channel };
}

function validateCandidateMetadata(value) {
  exactKeys(value, [
    "schemaVersion",
    "sourceSha",
    "sourceDigest",
    "workflowRunId",
    "packageVersion",
    "releaseChannel",
    "releaseManifestSha256",
    "status",
    "generatedAt",
  ], "candidate metadata");
  if (value.schemaVersion !== NATIVE_CANDIDATE_METADATA_SCHEMA) {
    throw new Error(`candidate metadata must use ${NATIVE_CANDIDATE_METADATA_SCHEMA}.`);
  }
  validateCandidateInputs({
    sourceSha: value.sourceSha,
    packageVersion: value.packageVersion,
    channel: value.releaseChannel,
  });
  if (!validSha256(value.sourceDigest) || !validSha256(value.releaseManifestSha256)) {
    throw new Error("candidate metadata digests must be lowercase SHA-256 values.");
  }
  if (!/^[1-9]\d*$/u.test(String(value.workflowRunId || ""))) {
    throw new Error("candidate metadata workflowRunId must identify a GitHub Actions run.");
  }
  if (value.status !== CANDIDATE_STATUS) throw new Error(`candidate status must be ${CANDIDATE_STATUS}.`);
  if (!Number.isFinite(Date.parse(requiredText(value.generatedAt, "candidate generatedAt")))) {
    throw new Error("candidate generatedAt must be an ISO date-time.");
  }
  return value;
}

function relativeFiles(root) {
  const output = [];
  const visit = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const absolute = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(absolute);
      else if (entry.isFile()) output.push(path.relative(root, absolute).replaceAll("\\", "/"));
      else throw new Error(`candidate bundle contains an unsupported entry: ${absolute}`);
    }
  };
  visit(root);
  return output.sort();
}

function buildChecksums(bundleDirectory) {
  const root = path.resolve(bundleDirectory);
  return Object.fromEntries(relativeFiles(root)
    .filter((entry) => entry !== "checksums.json")
    .map((entry) => [entry, sha256File(path.join(root, entry))]));
}

function validateChecksums(bundleDirectory, checksums) {
  const expected = buildChecksums(bundleDirectory);
  exactKeys(checksums, Object.keys(expected), "candidate checksums");
  for (const [entry, digest] of Object.entries(expected)) {
    if (!validSha256(checksums[entry]) || checksums[entry] !== digest) {
      throw new Error(`candidate checksum mismatch for ${entry}.`);
    }
  }
  return checksums;
}

function validatePlatformInstallEvidence(directory, manifest) {
  const root = path.resolve(directory);
  if (!fs.existsSync(root) || !fs.statSync(root).isDirectory()) {
    throw new Error("candidate bundle requires a platform install evidence directory.");
  }
  const files = fs.readdirSync(root).filter((file) => file.endsWith(".json")).sort();
  const reports = files.map((file) => JSON.parse(fs.readFileSync(path.join(root, file), "utf8")));
  const expected = Object.keys(manifest.artifacts.native).sort();
  const actual = reports.map((report) => report.platformPackage).sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error("candidate install evidence must cover every native platform package exactly once.");
  }
  for (const report of reports) {
    const artifact = manifest.artifacts.native[report.platformPackage];
    if (report.schemaVersion !== "flopeek-native-candidate-install/v1"
      || report.status !== "verified"
      || report.packageVersion !== manifest.release.version
      || report.sourceSha !== manifest.release.repositoryRevision
      || report.binarySha256 !== artifact?.binarySha256
      || report.selectedImplementation !== "native"
      || report.sourceAuthority !== "rust"
      || report.fallback?.active !== false
      || report.protocolVersion !== NATIVE_PROTOCOL_VERSION
      || report.adapterContractDigest !== adapterContractDigest()
      || report.healthImplementation !== "rust"
      || report.binaryVersion !== manifest.release.version
      || report.store?.journalMode?.toLowerCase() !== "wal"
      || report.store?.quickCheck?.toLowerCase() !== "ok"
      || report.sqliteAuthority !== true
      || report.graphJsonAuthority !== false
      || !/^fp:\/\//u.test(report.contextRef || "")
      || report.contextRefResolution !== "current"
      || report.uninstallClean !== true) {
      throw new Error(`candidate install evidence is invalid for ${report.platformPackage || "unknown platform"}.`);
    }
  }
  return reports;
}

function validateCandidateBundle(bundleDirectory, options = {}) {
  const root = path.resolve(bundleDirectory);
  if (!fs.existsSync(root) || !fs.statSync(root).isDirectory()) {
    throw new Error("candidate bundle must be an existing directory.");
  }
  const entries = fs.readdirSync(root).sort();
  const missing = REQUIRED_BUNDLE_ENTRIES.filter((entry) => !entries.includes(entry));
  if (missing.length) throw new Error(`candidate bundle is missing: ${missing.join(", ")}.`);
  const manifestFile = path.join(root, "native-release-manifest.json");
  const metadata = validateCandidateMetadata(JSON.parse(fs.readFileSync(path.join(root, "candidate-metadata.json"), "utf8")));
  const manifest = loadNativeReleaseManifest(manifestFile);
  const manifestSha256 = sha256File(manifestFile);
  if (metadata.releaseManifestSha256 !== manifestSha256) {
    throw new Error("candidate metadata is not bound to the exact release manifest.");
  }
  if (metadata.sourceSha !== manifest.release.repositoryRevision
    || metadata.sourceDigest !== manifest.release.sourceDigest
    || metadata.packageVersion !== manifest.release.version) {
    throw new Error("candidate source and package identity do not match the release manifest.");
  }
  if (options.expectedManifestSha256 && options.expectedManifestSha256 !== manifestSha256) {
    throw new Error("candidate release manifest SHA-256 does not match the approved promotion input.");
  }
  if (options.expectedChannel && options.expectedChannel !== metadata.releaseChannel) {
    throw new Error("candidate release channel does not match the approved promotion input.");
  }
  const checksums = JSON.parse(fs.readFileSync(path.join(root, "checksums.json"), "utf8"));
  validateChecksums(root, checksums);
  if (path.basename(manifest.artifacts.main.filename) !== manifest.artifacts.main.filename
    || manifest.artifacts.main.sha256 !== sha256File(path.join(root, "flopeek-main.tgz"))) {
    throw new Error("candidate main tarball does not match the release manifest.");
  }
  if (manifest.artifacts.rolloutEvidence.sha256 !== sha256File(path.join(root, "native-rollout-evidence.json"))) {
    throw new Error("candidate rollout packet does not match the release manifest.");
  }
  for (const artifact of Object.values(manifest.artifacts.native)) {
    const file = path.join(root, artifact.filename);
    if (!fs.existsSync(file) || sha256File(file) !== artifact.tarballSha256) {
      throw new Error(`candidate native tarball does not match the release manifest: ${artifact.filename}.`);
    }
  }
  const rollout = JSON.parse(fs.readFileSync(path.join(root, "native-rollout-evidence.json"), "utf8"));
  if (rollout.status !== "complete") throw new Error("candidate rollout evidence must be complete.");
  const adapterParity = JSON.parse(fs.readFileSync(path.join(root, "adapter-parity.json"), "utf8"));
  if (adapterParity.binary?.sourceRevision !== metadata.sourceSha) {
    throw new Error("candidate adapter parity source revision does not match the candidate.");
  }
  const platformInstalls = options.requirePlatformInstallEvidence
    ? validatePlatformInstallEvidence(path.join(root, "candidate-install-verification"), manifest)
    : null;
  return { metadata, manifest, manifestSha256, checksums, platformInstalls };
}

function buildPromotionAttestation({
  candidateRunId,
  releaseManifestSha256,
  sourceSha,
  packageVersion,
  channel,
  promotedBy,
  promotedAt = new Date().toISOString(),
  result = "published",
}) {
  const candidate = validateCandidateInputs({ sourceSha, packageVersion, channel });
  if (!/^[1-9]\d*$/u.test(String(candidateRunId || ""))) throw new Error("candidateRunId must identify a GitHub Actions run.");
  if (!validSha256(releaseManifestSha256)) throw new Error("releaseManifestSha256 must be a lowercase SHA-256 digest.");
  requiredText(promotedBy, "promotedBy");
  if (!Number.isFinite(Date.parse(promotedAt))) throw new Error("promotedAt must be an ISO date-time.");
  if (!["dry-run-verified", "published"].includes(result)) throw new Error("promotion result is invalid.");
  return {
    schemaVersion: NATIVE_PROMOTION_APPROVAL_SCHEMA,
    candidateRunId: String(candidateRunId),
    releaseManifestSha256,
    sourceSha: candidate.sourceSha,
    packageVersion: candidate.packageVersion,
    channel: candidate.channel,
    promotedBy,
    promotedAt,
    result,
  };
}

function canonicalJson(value) {
  return `${JSON.stringify(value, null, 2)}\n`;
}

function sourceDigestForCommit(root, sourceSha) {
  const output = require("node:child_process").execFileSync(
    "git",
    ["-C", root, "ls-tree", "-r", "--full-tree", sourceSha],
  );
  return crypto.createHash("sha256").update(output).digest("hex");
}

module.exports = {
  CANDIDATE_STATUS,
  NATIVE_CANDIDATE_METADATA_SCHEMA,
  NATIVE_PROMOTION_APPROVAL_SCHEMA,
  RELEASE_CHANNELS,
  buildChecksums,
  buildPromotionAttestation,
  canonicalJson,
  sourceDigestForCommit,
  validateCandidateBundle,
  validateCandidateInputs,
  validateCandidateMetadata,
  validateChecksums,
  validatePlatformInstallEvidence,
};
