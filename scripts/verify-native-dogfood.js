#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { validateNativeDogfoodEvidence } = require("../src/native-dogfood-evidence");

function argument(argv, name) {
  const index = argv.indexOf(name);
  return index >= 0 ? argv[index + 1] || null : null;
}

function main(argv = process.argv.slice(2)) {
  const file = argument(argv, "--file");
  const sourceRevision = argument(argv, "--source-revision");
  const binarySha256 = argument(argv, "--binary-sha256");
  const requireComplete = argv.includes("--require-complete");
  if (!file) throw new Error("Usage: verify-native-dogfood --file <json> [--source-revision <sha>] [--binary-sha256 <sha>] [--require-complete].");
  const evidence = JSON.parse(fs.readFileSync(path.resolve(file), "utf8"));
  const result = validateNativeDogfoodEvidence(evidence, { sourceRevision, binarySha256 });
  if (requireComplete && result.status !== "complete") {
    throw new Error("Native dogfood evidence is still pending its elapsed multi-day window.");
  }
  process.stdout.write(`${JSON.stringify({
    schemaVersion: "flopeek-native-dogfood-verification/v1",
    status: result.status,
    distinctDays: result.distinctDays,
  }, null, 2)}\n`);
  return result;
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`Native dogfood blocked: ${error.stack || error.message}\n`);
    process.exitCode = 1;
  }
}

module.exports = { main };
