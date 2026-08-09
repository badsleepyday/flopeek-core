#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { adapterContractDigest } = require("../src/adapter-registry");
const { NATIVE_PROTOCOL_VERSION } = require("../src/native-protocol-client");
const { NATIVE_ROLLOUT_EVIDENCE_SCHEMA } = require("../src/native-rollout-evidence");
const { buildPacket } = require("./build-native-rollout-evidence");

function argument(argv, name) {
  const index = argv.indexOf(name);
  return index >= 0 ? argv[index + 1] || null : null;
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function incompletePacket(root) {
  const manifest = readJson(path.join(root, "package.json"));
  return {
    schemaVersion: NATIVE_ROLLOUT_EVIDENCE_SCHEMA,
    status: "incomplete",
    binding: {
      packageName: manifest.name,
      packageVersion: manifest.version,
      adapterContractDigest: adapterContractDigest(),
      protocolVersion: NATIVE_PROTOCOL_VERSION,
      repositoryRevision: null,
      sourceDigest: null,
      binaries: null,
    },
    evidence: null,
  };
}

function preparePacket({ root, inputs, assets, allowIneligible = false }) {
  if (!fs.existsSync(inputs)) return incompletePacket(root);
  if (!fs.statSync(inputs).isDirectory()) throw new Error("Native rollout inputs must be a directory.");
  const candidate = path.join(inputs, "candidate.json");
  const adapterParity = path.join(inputs, "adapter-parity.json");
  const benchmark = path.join(inputs, "benchmark.json");
  const profiles = path.join(inputs, "profiles");
  const databaseOpenEvidence = path.join(inputs, "database-open-evidence.json");
  const soakEvidence = path.join(inputs, "native-soak.json");
  const surfaceEvidence = path.join(inputs, "native-surface-matrix.json");
  const missing = [candidate, adapterParity, benchmark, profiles, databaseOpenEvidence, soakEvidence, surfaceEvidence]
    .filter((entry) => !fs.existsSync(entry));
  if (missing.length) {
    throw new Error(`Native rollout inputs are partial; missing: ${missing.map((entry) => path.relative(inputs, entry)).join(", ")}.`);
  }
  return buildPacket({
    root,
    candidate: readJson(candidate),
    adapterParity: readJson(adapterParity),
    benchmark: readJson(benchmark),
    profiles,
    assets,
    databaseOpenEvidence,
    soakEvidence,
    surfaceEvidence,
    allowIneligible,
  });
}

function main() {
  const argv = process.argv.slice(2);
  const root = path.resolve(__dirname, "..");
  const inputs = argument(argv, "--inputs");
  const assets = argument(argv, "--assets");
  const output = argument(argv, "--output");
  const allowIneligible = argv.includes("--allow-ineligible");
  if (!inputs || !assets || !output) {
    throw new Error("Usage: prepare-native-rollout-evidence --inputs <directory> --assets <directory> --output <json>.");
  }
  const packet = preparePacket({
    root,
    inputs: path.resolve(inputs),
    assets: path.resolve(assets),
    allowIneligible,
  });
  fs.writeFileSync(path.resolve(output), `${JSON.stringify(packet, null, 2)}\n`);
  process.stdout.write(`Prepared ${packet.status} native rollout evidence from the exact release set.\n`);
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.stack || error.message}\n`);
    process.exitCode = 1;
  }
}

module.exports = { incompletePacket, preparePacket };
