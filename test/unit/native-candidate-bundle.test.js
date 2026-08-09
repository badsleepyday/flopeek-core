"use strict";

const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const {
  NATIVE_PROMOTION_APPROVAL_SCHEMA,
  buildChecksums,
  buildPromotionAttestation,
  validateCandidateInputs,
  validateChecksums,
  validatePlatformInstallEvidence,
} = require("../../scripts/native-candidate-bundle");
const { buildEvidenceManifest } = require("../../scripts/build-native-candidate-evidence-manifest");
const { adapterContractDigest } = require("../../src/adapter-registry");
const { NATIVE_PLATFORM_TARGETS } = require("../../src/native-platform-targets");

test("candidate inputs bind an exact commit, semantic version, and matching channel", () => {
  const sourceSha = "a".repeat(40);
  assert.deepEqual(validateCandidateInputs({
    sourceSha,
    packageVersion: "1.2.3-beta.4",
    channel: "beta",
  }), { sourceSha, packageVersion: "1.2.3-beta.4", channel: "beta" });
  assert.throws(() => validateCandidateInputs({
    sourceSha: "main",
    packageVersion: "1.2.3-beta.4",
    channel: "beta",
  }), /exact lowercase 40-character/);
  assert.throws(() => validateCandidateInputs({
    sourceSha,
    packageVersion: "1.2.3-beta.4",
    channel: "latest",
  }), /does not match package version channel/);
  assert.throws(() => validateCandidateInputs({
    sourceSha,
    packageVersion: "1.2",
    channel: "latest",
  }), /exact supported semantic version/);
});

test("candidate checksums cover every regular file and reject tampering and omission", (context) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "flopeek-candidate-checksums-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, "profiles"));
  fs.writeFileSync(path.join(root, "artifact.tgz"), "artifact");
  fs.writeFileSync(path.join(root, "profiles", "profile.json"), "{}\n");
  const checksums = buildChecksums(root);
  assert.deepEqual(Object.keys(checksums), ["artifact.tgz", "profiles/profile.json"]);
  assert.doesNotThrow(() => validateChecksums(root, checksums));
  fs.appendFileSync(path.join(root, "artifact.tgz"), "tampered");
  assert.throws(() => validateChecksums(root, checksums), /checksum mismatch/);
  assert.throws(() => validateChecksums(root, { "artifact.tgz": checksums["artifact.tgz"] }), /missing fields/);
});

test("promotion attestation is generated from exact approved candidate identity", () => {
  const value = buildPromotionAttestation({
    candidateRunId: "12345",
    releaseManifestSha256: "b".repeat(64),
    sourceSha: "a".repeat(40),
    packageVersion: "1.2.3",
    channel: "latest",
    promotedBy: "octocat",
    promotedAt: "2026-07-30T00:00:00.000Z",
    result: "dry-run-verified",
  });
  assert.equal(value.schemaVersion, NATIVE_PROMOTION_APPROVAL_SCHEMA);
  assert.equal(value.result, "dry-run-verified");
  assert.throws(() => buildPromotionAttestation({
    ...value,
    releaseManifestSha256: "not-a-digest",
  }), /lowercase SHA-256/);
  assert.throws(() => buildPromotionAttestation({
    ...value,
    candidateRunId: "0",
  }), /GitHub Actions run/);
});

test("platform install evidence must cover every exact artifact and fail closed on tampering", (context) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "flopeek-platform-installs-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const manifest = {
    release: {
      version: "1.2.3",
      repositoryRevision: "a".repeat(40),
    },
    artifacts: {
      native: Object.fromEntries(NATIVE_PLATFORM_TARGETS.map((target, index) => [
        target.packageName,
        { binarySha256: String(index).padStart(64, "0") },
      ])),
    },
  };
  for (const [index, target] of NATIVE_PLATFORM_TARGETS.entries()) {
    fs.writeFileSync(path.join(root, `${target.platform}-${target.arch}.json`), JSON.stringify({
      schemaVersion: "flopeek-native-candidate-install/v1",
      status: "verified",
      packageVersion: "1.2.3",
      sourceSha: "a".repeat(40),
      platformPackage: target.packageName,
      binarySha256: String(index).padStart(64, "0"),
      selectedImplementation: "native",
      sourceAuthority: "rust",
      fallback: { active: false, reason: null },
      protocolVersion: "flopeek-native-protocol/v1",
      adapterContractDigest: adapterContractDigest(),
      healthImplementation: "rust",
      binaryVersion: "1.2.3",
      store: { journalMode: "wal", quickCheck: "ok" },
      contextRef: "fp://local/project/node/file%3Asrc%2Findex.ts@1",
      contextRefResolution: "current",
      sqliteAuthority: true,
      graphJsonAuthority: false,
      uninstallClean: true,
    }));
  }
  assert.equal(validatePlatformInstallEvidence(root, manifest).length, 6);
  const first = path.join(root, `${NATIVE_PLATFORM_TARGETS[0].platform}-${NATIVE_PLATFORM_TARGETS[0].arch}.json`);
  const tampered = JSON.parse(fs.readFileSync(first, "utf8"));
  tampered.binarySha256 = "f".repeat(64);
  fs.writeFileSync(first, JSON.stringify(tampered));
  assert.throws(() => validatePlatformInstallEvidence(root, manifest), /invalid/);
});

test("blocked candidate evidence manifest binds all six platform tarballs and the negative gate decision", (context) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "flopeek-blocked-evidence-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const sourceSha = "a".repeat(40);
  const packageVersion = "1.2.3-beta.4";
  const binaries = Object.fromEntries(NATIVE_PLATFORM_TARGETS.map((target, index) => {
    const filename = `${target.packageName.replace("@flopeek/", "flopeek-")}-${packageVersion}.tgz`;
    const bytes = Buffer.from(`native-${index}`);
    fs.writeFileSync(path.join(root, filename), bytes);
    return [target.packageName, {
      tarballSha256: crypto.createHash("sha256").update(bytes).digest("hex"),
      binarySha256: String(index).padStart(64, "0"),
    }];
  }));
  fs.writeFileSync(path.join(root, "native-rollout-evidence.json"), JSON.stringify({
    schemaVersion: "flopeek-native-rollout-evidence/v2",
    status: "blocked",
    binding: {
      packageName: "flopeek",
      packageVersion,
      repositoryRevision: sourceSha,
      binaries,
    },
    decision: {
      eligible: false,
      reasons: ["cold-benchmark-regression-exceeds-10-percent"],
      selectedImplementation: "javascript",
      rollback: "automatic-javascript-fallback-required",
    },
  }, null, 2));
  fs.writeFileSync(path.join(root, "benchmark.json"), "{}\n");
  const output = path.join(root, "native-candidate-evidence-manifest.json");
  const manifest = buildEvidenceManifest({
    bundle: root,
    sourceSha,
    packageVersion,
    workflowRunId: "123",
    output,
  });
  assert.equal(manifest.status, "blocked");
  assert.equal(manifest.decision.eligible, false);
  assert.equal(Object.keys(manifest.files).length, 8);
  assert.equal(manifest.files["native-rollout-evidence.json"], crypto.createHash("sha256").update(fs.readFileSync(path.join(root, "native-rollout-evidence.json"))).digest("hex"));
});
