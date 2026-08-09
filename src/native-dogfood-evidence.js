"use strict";

const { getAdapterRegistry } = require("./adapter-registry");

const NATIVE_DOGFOOD_EVIDENCE_SCHEMA = "flopeek-native-dogfood-evidence/v1";
const REQUIRED_DOGFOOD_DAYS = 7;
const MINIMUM_DOGFOOD_REPOSITORIES = 5;
const REQUIRED_DOGFOOD_SURFACES = Object.freeze({
  cliCommands: 7,
  mcpTools: 62,
  httpRoutes: 95,
  unclassified: 0,
});
const REQUIRED_NATIVE_ADAPTERS = Object.freeze(getAdapterRegistry().adapters
  .filter((adapter) => adapter.capabilities.structure !== "inventory-only")
  .map((adapter) => adapter.id)
  .sort());

function exactKeys(value, expected, field) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${field} must be an object.`);
  }
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
    throw new Error(`${field} must contain exactly: ${wanted.join(", ")}.`);
  }
}

function validRevision(value) {
  return typeof value === "string" && /^[a-f0-9]{40}$/u.test(value);
}

function validSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/u.test(value);
}

function validTimestamp(value, field) {
  if (typeof value !== "string" || !Number.isFinite(Date.parse(value))) {
    throw new Error(`${field} must be an ISO date-time.`);
  }
  return Date.parse(value);
}

function validDate(value, field) {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}$/u.test(value)) {
    throw new Error(`${field} must be an ISO UTC calendar date.`);
  }
  const parsed = Date.parse(`${value}T00:00:00.000Z`);
  if (!Number.isFinite(parsed) || new Date(parsed).toISOString().slice(0, 10) !== value) {
    throw new Error(`${field} must be a real ISO UTC calendar date.`);
  }
  return parsed;
}

function sameStringSet(left, right) {
  return Array.isArray(left)
    && Array.isArray(right)
    && left.length === new Set(left).size
    && right.length === new Set(right).size
    && left.length === right.length
    && [...left].sort().every((value, index) => value === [...right].sort()[index]);
}

function validateSurfaces(value, field) {
  exactKeys(value, Object.keys(REQUIRED_DOGFOOD_SURFACES), field);
  for (const [key, expected] of Object.entries(REQUIRED_DOGFOOD_SURFACES)) {
    if (!Number.isSafeInteger(value[key]) || value[key] !== expected) {
      throw new Error(`${field}.${key} must equal ${expected}.`);
    }
  }
  return value;
}

function validateDay(day, binding, index) {
  const field = `native dogfood day ${index}`;
  exactKeys(day, [
    "date",
    "startedAt",
    "completedAt",
    "sourceRevision",
    "binarySha256",
    "status",
    "repositories",
    "exactRepositories",
    "adapters",
    "targetRepositoryWrites",
    "surfaces",
    "evidenceSha256",
  ], field);
  const dateMs = validDate(day.date, `${field}.date`);
  const startedMs = validTimestamp(day.startedAt, `${field}.startedAt`);
  const completedMs = validTimestamp(day.completedAt, `${field}.completedAt`);
  if (completedMs <= startedMs || new Date(completedMs).toISOString().slice(0, 10) !== day.date) {
    throw new Error(`${field} must complete after it starts on its declared UTC date.`);
  }
  if (!validRevision(day.sourceRevision) || day.sourceRevision !== binding.sourceRevision) {
    throw new Error(`${field} is not bound to the candidate source revision.`);
  }
  if (!validSha256(day.binarySha256) || day.binarySha256 !== binding.binarySha256) {
    throw new Error(`${field} is not bound to the candidate binary.`);
  }
  if (day.status !== "passed"
    || !Number.isSafeInteger(day.repositories) || day.repositories < MINIMUM_DOGFOOD_REPOSITORIES
    || day.exactRepositories !== day.repositories
    || !sameStringSet(day.adapters, REQUIRED_NATIVE_ADAPTERS)
    || day.targetRepositoryWrites !== false) {
    throw new Error(`${field} does not prove the required read-only repository and adapter matrix.`);
  }
  validateSurfaces(day.surfaces, `${field}.surfaces`);
  if (!validSha256(day.evidenceSha256)) {
    throw new Error(`${field}.evidenceSha256 must be a lowercase SHA-256 digest.`);
  }
  return { date: day.date, dateMs };
}

function validateNativeDogfoodEvidence(value, binding = {}) {
  exactKeys(value, [
    "schemaVersion",
    "status",
    "requiredDays",
    "sourceRevision",
    "binarySha256",
    "generatedAt",
    "days",
    "summary",
  ], "native dogfood evidence");
  if (value.schemaVersion !== NATIVE_DOGFOOD_EVIDENCE_SCHEMA) {
    throw new Error(`Native dogfood evidence must use ${NATIVE_DOGFOOD_EVIDENCE_SCHEMA}.`);
  }
  if (!Number.isSafeInteger(value.requiredDays) || value.requiredDays !== REQUIRED_DOGFOOD_DAYS) {
    throw new Error(`Native dogfood evidence requires exactly ${REQUIRED_DOGFOOD_DAYS} days.`);
  }
  validTimestamp(value.generatedAt, "native dogfood generatedAt");
  if (!validRevision(value.sourceRevision) || (binding.sourceRevision && value.sourceRevision !== binding.sourceRevision)) {
    throw new Error("Native dogfood evidence source revision is not exact or does not match the candidate.");
  }
  if (!validSha256(value.binarySha256) || (binding.binarySha256 && value.binarySha256 !== binding.binarySha256)) {
    throw new Error("Native dogfood evidence binary SHA-256 is not exact or does not match the candidate.");
  }
  if (!Array.isArray(value.days)) throw new Error("Native dogfood evidence days must be an array.");
  exactKeys(value.summary, [
    "distinctDays",
    "repositories",
    "exactRepositories",
    "adapters",
    "targetRepositoryWrites",
    "surfaces",
  ], "native dogfood summary");
  const seenDates = new Set();
  const dates = [];
  for (const [index, day] of value.days.entries()) {
    const validated = validateDay(day, value, index);
    if (seenDates.has(validated.date)) throw new Error(`Native dogfood evidence repeats date ${validated.date}.`);
    seenDates.add(validated.date);
    dates.push(validated.dateMs);
  }
  dates.sort((left, right) => left - right);
  for (let index = 1; index < dates.length; index += 1) {
    if (dates[index] - dates[index - 1] !== 24 * 60 * 60 * 1000) {
      throw new Error("Native dogfood evidence dates must be consecutive UTC calendar days.");
    }
  }
  if (value.status === "pending") {
    if (value.days.length >= value.requiredDays) {
      throw new Error("Native dogfood evidence with the required days must be marked complete.");
    }
    if (value.days.length === 0) {
      if (value.summary.distinctDays !== 0
        || value.summary.repositories !== 0
        || value.summary.exactRepositories !== 0
        || !sameStringSet(value.summary.adapters, [])
        || value.summary.targetRepositoryWrites !== false) {
        throw new Error("Pending native dogfood evidence with no days must contain no completed claims.");
      }
      exactKeys(value.summary.surfaces, Object.keys(REQUIRED_DOGFOOD_SURFACES), "native dogfood summary.surfaces");
      if (Object.values(value.summary.surfaces).some((count) => count !== 0)) {
        throw new Error("Pending native dogfood evidence with no days must not claim surfaces.");
      }
    } else if (value.summary.distinctDays !== value.days.length
      || !Number.isSafeInteger(value.summary.repositories)
      || value.summary.repositories < MINIMUM_DOGFOOD_REPOSITORIES
      || value.summary.exactRepositories !== value.summary.repositories
      || !sameStringSet(value.summary.adapters, REQUIRED_NATIVE_ADAPTERS)
      || value.summary.targetRepositoryWrites !== false) {
      throw new Error("Pending native dogfood summary does not match its completed day claims.");
    } else {
      validateSurfaces(value.summary.surfaces, "native dogfood summary.surfaces");
    }
    return Object.freeze({ status: "pending", distinctDays: value.days.length });
  }
  if (value.status !== "complete") throw new Error("Native dogfood evidence status must be pending or complete.");
  if (value.days.length < value.requiredDays) {
    throw new Error(`Native dogfood evidence requires ${value.requiredDays} completed days.`);
  }
  if (value.summary.distinctDays !== value.days.length
    || !Number.isSafeInteger(value.summary.repositories)
    || value.summary.repositories < MINIMUM_DOGFOOD_REPOSITORIES
    || value.summary.exactRepositories !== value.summary.repositories
    || !sameStringSet(value.summary.adapters, REQUIRED_NATIVE_ADAPTERS)
    || value.summary.targetRepositoryWrites !== false) {
    throw new Error("Native dogfood summary does not prove the complete matrix.");
  }
  validateSurfaces(value.summary.surfaces, "native dogfood summary.surfaces");
  return Object.freeze({ status: "complete", distinctDays: value.days.length });
}

function buildPendingNativeDogfoodEvidence({ sourceRevision, binarySha256, generatedAt = new Date().toISOString() }) {
  const value = {
    schemaVersion: NATIVE_DOGFOOD_EVIDENCE_SCHEMA,
    status: "pending",
    requiredDays: REQUIRED_DOGFOOD_DAYS,
    sourceRevision,
    binarySha256,
    generatedAt,
    days: [],
    summary: {
      distinctDays: 0,
      repositories: 0,
      exactRepositories: 0,
      adapters: [],
      targetRepositoryWrites: false,
      surfaces: Object.fromEntries(Object.keys(REQUIRED_DOGFOOD_SURFACES).map((key) => [key, 0])),
    },
  };
  validateNativeDogfoodEvidence(value, { sourceRevision, binarySha256 });
  return value;
}

module.exports = {
  MINIMUM_DOGFOOD_REPOSITORIES,
  NATIVE_DOGFOOD_EVIDENCE_SCHEMA,
  REQUIRED_DOGFOOD_DAYS,
  REQUIRED_DOGFOOD_SURFACES,
  REQUIRED_NATIVE_ADAPTERS,
  buildPendingNativeDogfoodEvidence,
  validateNativeDogfoodEvidence,
};
