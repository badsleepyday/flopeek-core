"use strict";

const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const {
  REQUIRED_DOGFOOD_SURFACES,
  REQUIRED_NATIVE_ADAPTERS,
  buildPendingNativeDogfoodEvidence,
  validateNativeDogfoodEvidence,
} = require("../../src/native-dogfood-evidence");
const { buildNativeDogfoodEvidence } = require("../../scripts/build-native-dogfood-evidence");

function completeEvidence() {
  const days = Array.from({ length: 7 }, (_, index) => {
    const date = `2026-02-${String(index + 1).padStart(2, "0")}`;
    return {
      date,
      startedAt: `${date}T03:00:00.000Z`,
      completedAt: `${date}T04:00:00.000Z`,
      sourceRevision: "a".repeat(40),
      binarySha256: "b".repeat(64),
      status: "passed",
      repositories: 8,
      exactRepositories: 8,
      adapters: [...REQUIRED_NATIVE_ADAPTERS],
      targetRepositoryWrites: false,
      surfaces: { ...REQUIRED_DOGFOOD_SURFACES },
      evidenceSha256: "c".repeat(64),
    };
  });
  return {
    schemaVersion: "flopeek-native-dogfood-evidence/v1",
    status: "complete",
    requiredDays: 7,
    sourceRevision: "a".repeat(40),
    binarySha256: "b".repeat(64),
    generatedAt: "2026-02-08T00:00:00.000Z",
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

test("pending dogfood evidence is explicit and exact-binary bound", () => {
  const pending = buildPendingNativeDogfoodEvidence({
    sourceRevision: "a".repeat(40),
    binarySha256: "b".repeat(64),
    generatedAt: "2026-02-01T00:00:00.000Z",
  });
  assert.equal(validateNativeDogfoodEvidence(pending, {
    sourceRevision: "a".repeat(40),
    binarySha256: "b".repeat(64),
  }).status, "pending");
});

test("complete dogfood evidence requires seven consecutive days and the full matrix", () => {
  const evidence = completeEvidence();
  assert.deepEqual(validateNativeDogfoodEvidence(evidence, {
    sourceRevision: "a".repeat(40),
    binarySha256: "b".repeat(64),
  }), { status: "complete", distinctDays: 7 });
  evidence.days[3].date = "2026-02-10";
  evidence.days[3].startedAt = "2026-02-10T03:00:00.000Z";
  evidence.days[3].completedAt = "2026-02-10T04:00:00.000Z";
  assert.throws(() => validateNativeDogfoodEvidence(evidence), /consecutive UTC calendar days/);
});

test("dogfood evidence rejects source or binary rebinding and target writes", () => {
  const evidence = completeEvidence();
  evidence.days[0].binarySha256 = "d".repeat(64);
  assert.throws(() => validateNativeDogfoodEvidence(evidence), /not bound to the candidate binary/);
  const writes = completeEvidence();
  writes.days[0].targetRepositoryWrites = true;
  assert.throws(() => validateNativeDogfoodEvidence(writes), /read-only repository/);
});

test("dogfood aggregation retains partial days and only becomes complete at seven", (context) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "flopeek-native-dogfood-days-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const evidence = completeEvidence();
  const writeDay = (day) => {
    const raw = path.join(root, `${day.date}.raw`);
    fs.mkdirSync(raw);
    const realCorpus = Buffer.from(`real-${day.date}`);
    const surfaces = Buffer.from(`surface-${day.date}`);
    fs.writeFileSync(path.join(raw, "real-corpus.json"), realCorpus);
    fs.writeFileSync(path.join(raw, "native-surface-matrix.json"), surfaces);
    day.evidenceSha256 = crypto.createHash("sha256").update(realCorpus).update(surfaces).digest("hex");
    fs.writeFileSync(path.join(root, `${day.date}.json`), `${JSON.stringify(day)}\n`);
  };
  for (const day of evidence.days.slice(0, 3)) {
    writeDay(day);
  }
  const partial = buildNativeDogfoodEvidence({
    daysDirectory: root,
    sourceRevision: evidence.sourceRevision,
    binarySha256: evidence.binarySha256,
  });
  assert.equal(partial.status, "pending");
  assert.equal(partial.summary.distinctDays, 3);
  for (const day of evidence.days.slice(3)) {
    writeDay(day);
  }
  const complete = buildNativeDogfoodEvidence({
    daysDirectory: root,
    sourceRevision: evidence.sourceRevision,
    binarySha256: evidence.binarySha256,
  });
  assert.equal(complete.status, "complete");
  assert.equal(complete.summary.distinctDays, 7);
});
