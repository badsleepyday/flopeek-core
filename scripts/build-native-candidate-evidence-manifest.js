#!/usr/bin/env node
"use strict";

// Build an evidence-only manifest for a candidate that completed all
// measurements but was rejected by the native-default gate. This is not a
// release manifest and cannot be consumed by the promotion workflow.
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { nativePlatformPackageNames } = require("../src/native-platform-targets");
const { NATIVE_ROLLOUT_EVIDENCE_SCHEMA } = require("../src/native-rollout-evidence");

function argument(argv, name) {
  const index = argv.indexOf(name);
  return index >= 0 ? argv[index + 1] || null : null;
}

function sha256File(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function files(root) {
  const result = [];
  const visit = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const absolute = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(absolute);
      else if (entry.isFile()) result.push(path.relative(root, absolute).replaceAll("\\", "/"));
      else throw new Error(`Evidence bundle contains unsupported entry: ${absolute}`);
    }
  };
  visit(root);
  return result.sort();
}

function buildEvidenceManifest({ bundle, sourceSha, packageVersion, workflowRunId, output }) {
  const root = path.resolve(bundle);
  const packetFile = path.join(root, "native-rollout-evidence.json");
  if (!fs.existsSync(packetFile)) throw new Error("Evidence bundle is missing native-rollout-evidence.json.");
  const packet = JSON.parse(fs.readFileSync(packetFile, "utf8"));
  if (packet.schemaVersion !== NATIVE_ROLLOUT_EVIDENCE_SCHEMA || packet.status !== "blocked") {
    throw new Error("Evidence-only manifest requires a blocked native rollout packet.");
  }
  if (!/^[a-f0-9]{40}$/u.test(sourceSha || "")
    || packet.binding?.repositoryRevision !== sourceSha
    || packet.binding?.packageVersion !== packageVersion
    || packet.decision?.eligible !== false
    || !Array.isArray(packet.decision?.reasons)
    || packet.decision.reasons.length === 0) {
    throw new Error("Blocked evidence is not bound to the requested source identity and negative decision.");
  }
  if (!/^[1-9]\d*$/u.test(String(workflowRunId || ""))) {
    throw new Error("workflowRunId must identify a GitHub Actions run.");
  }
  const binaries = packet.binding.binaries;
  const packages = nativePlatformPackageNames();
  if (!binaries || packages.some((name) => !binaries[name])) {
    throw new Error("Blocked evidence must retain all six native platform bindings.");
  }
  const tgzFiles = files(root).filter((file) => file.endsWith(".tgz"));
  for (const packageName of packages) {
    const prefix = `${packageName.replace("@flopeek/", "flopeek-")}-${packageVersion}.tgz`;
    const filename = tgzFiles.find((file) => path.basename(file) === prefix);
    if (!filename || sha256File(path.join(root, filename)) !== binaries[packageName].tarballSha256) {
      throw new Error(`Platform artifact is missing or not bound for ${packageName}.`);
    }
  }
  const outputRelative = path.relative(root, path.resolve(output)).replaceAll("\\", "/");
  const checksums = Object.fromEntries(files(root)
    .filter((file) => file !== outputRelative)
    .map((file) => [file, sha256File(path.join(root, file))]));
  const manifest = {
    schemaVersion: "flopeek-native-candidate-evidence-manifest/v1",
    status: "blocked",
    sourceSha,
    packageVersion,
    workflowRunId: String(workflowRunId),
    rolloutEvidenceSha256: sha256File(packetFile),
    decision: {
      eligible: false,
      reasons: packet.decision.reasons,
      selectedImplementation: packet.decision.selectedImplementation,
      rollback: packet.decision.rollback,
    },
    files: checksums,
  };
  fs.writeFileSync(path.resolve(output), `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}

if (require.main === module) {
  try {
    const argv = process.argv.slice(2);
    const options = {
      bundle: argument(argv, "--bundle"),
      sourceSha: argument(argv, "--source-sha"),
      packageVersion: argument(argv, "--package-version"),
      workflowRunId: argument(argv, "--workflow-run-id"),
      output: argument(argv, "--output"),
    };
    if (!Object.values(options).every(Boolean)) {
      throw new Error("Usage: build-native-candidate-evidence-manifest --bundle <directory> --source-sha <sha> --package-version <version> --workflow-run-id <id> --output <json>.");
    }
    const manifest = buildEvidenceManifest(options);
    process.stdout.write(`Wrote blocked evidence manifest with ${Object.keys(manifest.files).length} bound files.\n`);
  } catch (error) {
    process.stderr.write(`Blocked evidence manifest failed: ${error.stack || error.message}\n`);
    process.exitCode = 1;
  }
}

module.exports = { buildEvidenceManifest, files, sha256File };
