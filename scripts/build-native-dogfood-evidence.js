#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const crypto = require("node:crypto");
const path = require("node:path");
const {
  REQUIRED_DOGFOOD_DAYS,
  REQUIRED_DOGFOOD_SURFACES,
  REQUIRED_NATIVE_ADAPTERS,
  buildPendingNativeDogfoodEvidence,
  validateNativeDogfoodEvidence,
} = require("../src/native-dogfood-evidence");

function argument(argv, name) {
  const index = argv.indexOf(name);
  return index >= 0 ? argv[index + 1] || null : null;
}

function readDayFiles(directory) {
  const root = path.resolve(directory);
  if (!fs.existsSync(root) || !fs.statSync(root).isDirectory()) throw new Error("dogfood day directory does not exist.");
  const days = fs.readdirSync(root)
    .filter((name) => /^\d{4}-\d{2}-\d{2}\.json$/u.test(name))
    .sort()
    .map((name) => JSON.parse(fs.readFileSync(path.join(root, name), "utf8")));
  for (const day of days) {
    const rawDirectory = path.join(root, `${day.date}.raw`);
    const rawFiles = ["real-corpus.json", "native-surface-matrix.json"]
      .map((name) => path.join(rawDirectory, name));
    if (!rawFiles.every((file) => fs.existsSync(file) && fs.statSync(file).isFile())) {
      throw new Error(`Native dogfood day ${day.date} is missing its raw evidence files.`);
    }
    const digest = crypto.createHash("sha256")
      .update(fs.readFileSync(rawFiles[0]))
      .update(fs.readFileSync(rawFiles[1]))
      .digest("hex");
    if (digest !== day.evidenceSha256) {
      throw new Error(`Native dogfood day ${day.date} raw evidence checksum does not match.`);
    }
  }
  return days;
}

function summaryFor(days) {
  if (!days.length) {
    return {
      distinctDays: 0,
      repositories: 0,
      exactRepositories: 0,
      adapters: [],
      targetRepositoryWrites: false,
      surfaces: Object.fromEntries(Object.keys(REQUIRED_DOGFOOD_SURFACES).map((key) => [key, 0])),
    };
  }
  return {
    distinctDays: days.length,
    repositories: Math.min(...days.map((day) => day.repositories)),
    exactRepositories: Math.min(...days.map((day) => day.exactRepositories)),
    adapters: [...REQUIRED_NATIVE_ADAPTERS],
    targetRepositoryWrites: days.some((day) => day.targetRepositoryWrites),
    surfaces: { ...REQUIRED_DOGFOOD_SURFACES },
  };
}

function buildNativeDogfoodEvidence({ daysDirectory, sourceRevision, binarySha256, generatedAt = new Date().toISOString() }) {
  if (!daysDirectory || !sourceRevision || !binarySha256) {
    throw new Error("daysDirectory, sourceRevision, and binarySha256 are required.");
  }
  const days = readDayFiles(daysDirectory);
  const value = days.length === 0
    ? buildPendingNativeDogfoodEvidence({ sourceRevision, binarySha256, generatedAt })
    : {
      schemaVersion: "flopeek-native-dogfood-evidence/v1",
      status: days.length >= REQUIRED_DOGFOOD_DAYS ? "complete" : "pending",
      requiredDays: REQUIRED_DOGFOOD_DAYS,
      sourceRevision,
      binarySha256,
      generatedAt,
      days,
      summary: summaryFor(days),
    };
  validateNativeDogfoodEvidence(value, { sourceRevision, binarySha256 });
  return value;
}

function main(argv = process.argv.slice(2)) {
  const daysDirectory = argument(argv, "--days");
  const sourceRevision = argument(argv, "--source-revision");
  const binarySha256 = argument(argv, "--binary-sha256");
  const output = argument(argv, "--output");
  if (!daysDirectory || !sourceRevision || !binarySha256 || !output) {
    throw new Error("Usage: build-native-dogfood-evidence --days <directory> --source-revision <sha> --binary-sha256 <sha> --output <json>.");
  }
  const evidence = buildNativeDogfoodEvidence({ daysDirectory, sourceRevision, binarySha256 });
  const resolved = path.resolve(output);
  fs.mkdirSync(path.dirname(resolved), { recursive: true });
  fs.writeFileSync(resolved, `${JSON.stringify(evidence, null, 2)}\n`);
  process.stdout.write(`Wrote ${evidence.status} native dogfood evidence with ${evidence.summary.distinctDays}/${evidence.requiredDays} days.\n`);
  return evidence;
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`Native dogfood aggregation blocked: ${error.stack || error.message}\n`);
    process.exitCode = 1;
  }
}

module.exports = { buildNativeDogfoodEvidence, main, readDayFiles, summaryFor };
