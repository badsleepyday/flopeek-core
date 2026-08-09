#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const {
  buildPendingNativeDogfoodEvidence,
} = require("../src/native-dogfood-evidence");

function argument(argv, name) {
  const index = argv.indexOf(name);
  return index >= 0 ? argv[index + 1] || null : null;
}

function main(argv = process.argv.slice(2)) {
  const sourceRevision = argument(argv, "--source-revision");
  const binarySha256 = argument(argv, "--binary-sha256");
  const output = argument(argv, "--output");
  if (!sourceRevision || !binarySha256 || !output) {
    throw new Error("Usage: create-native-dogfood-pending --source-revision <sha> --binary-sha256 <sha> --output <json>.");
  }
  const evidence = buildPendingNativeDogfoodEvidence({ sourceRevision, binarySha256 });
  const resolved = path.resolve(output);
  fs.mkdirSync(path.dirname(resolved), { recursive: true });
  fs.writeFileSync(resolved, `${JSON.stringify(evidence, null, 2)}\n`);
  process.stdout.write(`Wrote pending native dogfood evidence to ${resolved}.\n`);
  return evidence;
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`Native dogfood pending evidence blocked: ${error.stack || error.message}\n`);
    process.exitCode = 1;
  }
}

module.exports = { main };
