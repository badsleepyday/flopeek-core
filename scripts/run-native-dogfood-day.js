#!/usr/bin/env node
"use strict";

const childProcess = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const {
  REQUIRED_DOGFOOD_SURFACES,
  REQUIRED_NATIVE_ADAPTERS,
  validateNativeDogfoodEvidence,
} = require("../src/native-dogfood-evidence");

const ROOT = path.resolve(__dirname, "..");

function argument(argv, name) {
  const index = argv.indexOf(name);
  return index >= 0 ? argv[index + 1] || null : null;
}

function sha256File(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function run(command, args) {
  const result = childProcess.spawnSync(command, args, {
    cwd: ROOT,
    stdio: "inherit",
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed: ${result.error?.message || `exit ${result.status}`}.`);
  }
}

function main(argv = process.argv.slice(2)) {
  const binary = argument(argv, "--binary");
  const sourceRevision = argument(argv, "--source-revision");
  const output = argument(argv, "--output");
  const workDirectory = argument(argv, "--work-directory")
    || fs.mkdtempSync(path.join(os.tmpdir(), "flopeek-native-dogfood-day-"));
  const manifest = argument(argv, "--manifest")
    || path.join(ROOT, "benchmarks", "native-adapter-corpus.json");
  if (!binary || !sourceRevision || !output) {
    throw new Error("Usage: run-native-dogfood-day --binary <exact binary> --source-revision <sha> --output <day.json> [--work-directory <directory>] [--manifest <json>].");
  }
  if (!/^[a-f0-9]{40}$/u.test(sourceRevision)) throw new Error("source revision must be an exact lowercase 40-character commit SHA.");
  const binaryPath = path.resolve(binary);
  if (!fs.existsSync(binaryPath) || !fs.statSync(binaryPath).isFile()) throw new Error("dogfood binary does not exist.");
  const outputPath = path.resolve(output);
  if (fs.existsSync(outputPath)) throw new Error(`dogfood day output already exists: ${outputPath}.`);
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.mkdirSync(path.resolve(workDirectory), { recursive: true });
  const startedAt = new Date().toISOString();
  const binarySha256 = sha256File(binaryPath);
  const rawDirectory = path.join(path.dirname(outputPath), `${path.basename(outputPath, ".json")}.raw`);
  fs.mkdirSync(rawDirectory, { recursive: true });
  const realCorpusFile = path.join(rawDirectory, "real-corpus.json");
  const surfaceFile = path.join(rawDirectory, "native-surface-matrix.json");
  run(process.execPath, [
    "scripts/verify-native-real-corpus.js",
    "--binary", binaryPath,
    "--manifest", path.resolve(manifest),
    "--clone-directory", path.join(path.resolve(workDirectory), "corpus"),
    "--source-revision", sourceRevision,
    "--output", realCorpusFile,
  ]);
  run(process.execPath, [
    "scripts/verify-native-surfaces.js",
    "--binary", binaryPath,
    "--output", surfaceFile,
  ]);
  const realCorpus = JSON.parse(fs.readFileSync(realCorpusFile, "utf8"));
  const surfaces = JSON.parse(fs.readFileSync(surfaceFile, "utf8"));
  if (realCorpus.sourceRevision !== sourceRevision
    || realCorpus.binarySha256 !== binarySha256
    || realCorpus.summary?.targetRepositoryWrites !== false
    || realCorpus.summary?.exactRepositories !== realCorpus.summary?.repositories
    || !Array.isArray(realCorpus.summary?.adapters)
    || JSON.stringify([...realCorpus.summary.adapters].sort()) !== JSON.stringify([...REQUIRED_NATIVE_ADAPTERS].sort())) {
    throw new Error("real-corpus evidence is not exact, read-only, or bound to the dogfood binary.");
  }
  if (surfaces.binarySha256 !== binarySha256
    || surfaces.summary?.cliCommands !== REQUIRED_DOGFOOD_SURFACES.cliCommands
    || surfaces.summary?.mcpTools !== REQUIRED_DOGFOOD_SURFACES.mcpTools
    || surfaces.summary?.httpRoutes !== REQUIRED_DOGFOOD_SURFACES.httpRoutes
    || surfaces.summary?.unclassified !== REQUIRED_DOGFOOD_SURFACES.unclassified) {
    throw new Error("native surface evidence is not bound to the dogfood binary or complete.");
  }
  const completedAt = new Date().toISOString();
  const date = completedAt.slice(0, 10);
  const evidenceSha256 = crypto.createHash("sha256")
    .update(fs.readFileSync(realCorpusFile))
    .update(fs.readFileSync(surfaceFile))
    .digest("hex");
  const day = {
    date,
    startedAt,
    completedAt,
    sourceRevision,
    binarySha256,
    status: "passed",
    repositories: realCorpus.summary.repositories,
    exactRepositories: realCorpus.summary.exactRepositories,
    adapters: [...realCorpus.summary.adapters].sort(),
    targetRepositoryWrites: realCorpus.summary.targetRepositoryWrites,
    surfaces: {
      cliCommands: surfaces.summary.cliCommands,
      mcpTools: surfaces.summary.mcpTools,
      httpRoutes: surfaces.summary.httpRoutes,
      unclassified: surfaces.summary.unclassified,
    },
    evidenceSha256,
  };
  const aggregateLike = {
    schemaVersion: "flopeek-native-dogfood-evidence/v1",
    status: "pending",
    requiredDays: 7,
    sourceRevision,
    binarySha256,
    generatedAt: completedAt,
    days: [day],
    summary: {
      distinctDays: 1,
      repositories: day.repositories,
      exactRepositories: day.exactRepositories,
      adapters: day.adapters,
      targetRepositoryWrites: false,
      surfaces: day.surfaces,
    },
  };
  validateNativeDogfoodEvidence(aggregateLike, { sourceRevision, binarySha256 });
  fs.writeFileSync(outputPath, `${JSON.stringify(day, null, 2)}\n`);
  process.stdout.write(`Recorded native dogfood day ${date} for ${sourceRevision}/${binarySha256}.\n`);
  return day;
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`Native dogfood day blocked: ${error.stack || error.message}\n`);
    process.exitCode = 1;
  }
}

module.exports = { main, sha256File };
