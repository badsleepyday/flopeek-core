# Native Core Completion Tracker

This document is the verification index for the Native Core stabilization
charter. A local gate is marked `passing` only after its command exits
successfully. Candidate and promotion gates remain separate: local success is
not a substitute for six-platform artifacts, protected approval, or time-based
dogfooding.

## Verified audit-remediation source

- Audited baseline: `48ab710f167edd5f19d3b70f6574a05c254c9578`
- Native evidence implementation SHA: `5e404b126ad8b96bbfadd9ae38675a77c58b430e`
- Windows/Node 24 path-identity remediation SHA: `56ca898ba03a8ea6ea8602a0d1ac32d86611aac9`
- Branch: `fix/rust-core-audit-remediation`
- Exact Windows release binary SHA-256:
  `7acf85ddcbad49301b9bab2ccfd66b7b4ef1e8462ec6d7f39d8cb9665b572d59`
- Verification date: `2026-08-02`

The implementation SHAs above are the code/evidence commits. This tracker is an
attestation follow-up and therefore is not part of that self-referenced source
commit.

## Audit remediation status

| Area | Status | Verified result |
| --- | --- | --- |
| Schema/history correctness | `passing` | Schema v12 separates semantic edge/placement identity from presence intervals. Rust tests cover v1 present, v2 absent, v3 present, v4 absent, v5 present, including edge evidence. |
| Store compatibility and path safety | `passing` | Future schema, version disagreement, missing required objects, corruption, Unix symlink escape, and Windows junction escape fail closed. v11-to-v12 data migration is transactional and tested. |
| MSRV and runtime contract | `passing` | `rust-version = 1.88`; `cargo +1.88.0 check --locked` passed. Current toolchain verification passed with Rust 1.97.0, .NET 10.0.302, Go 1.26.4, and Node 22.23.2. Supported Node CI matrix is 22/24. Local Windows regression verification passed on Node 24.18.1 (public-source batches: 290, 105, 5, and 15 tests) and Node 22.23.2 (127 targeted tests). |
| Windows path identity | `passing` | Repository roots use the native canonical realpath across watcher, scanner, cache, MCP, and core-client boundaries. Windows 8.3 short paths and long paths retain one graph identity and adjacent delta; recursive watching no longer feeds a short root into the affected Node 24/libuv path. |
| Identity/API boundary | `passing` | Identity v2 methods are absent by default and require explicit `experimentalIdentityV2: true` capability negotiation in both client and protocol. JavaScript remains the default authority. |
| Source-free persistence boundary | `passing` | Structural facts use typed `deny_unknown_fields` allowlists with bounded depth/size and adversarial source-body tests. Store validation remains defense in depth. |
| External dependency identity | `passing` | The persisted scheme is honestly named `external-import-root-v1`; canonical import roots and exact observed specifiers are distinct. No package-manager coordinate is claimed without resolver evidence. |
| Supply-chain workflow | `passing` | Third-party Actions are full-commit-SHA pinned, unnecessary promotion OIDC permission is removed, Dependabot covers GitHub Actions, and actionlint passed. |
| Package identity | `passing` | Legacy `.flowpeek` examples/cache references were normalized to `.flopeek`; package, audit, and clean-room checks passed. |

## Current verified commands

- `cargo fmt --check --manifest-path native/flopeek-core/Cargo.toml` — passed.
- `cargo clippy --locked --manifest-path native/flopeek-core/Cargo.toml -- -D warnings` — passed.
- `cargo test --locked --manifest-path native/flopeek-core/Cargo.toml` — 128/128 passed.
- `cargo +1.88.0 check --locked --manifest-path native/flopeek-core/Cargo.toml` — passed.
- `npm run verify:toolchains` — passed for Rust, .NET, Go, and Node.
- Native Node matrix — 101/101 supporting tests, 17/17 non-strict core-client tests, 13/13 source-authority tests, and 29/29 strict lifecycle/incremental tests passed.
- `npm run verify:native-js-parser-parity` — 22/22 files exact across 11 fixtures.
- `npm run verify:native-adapter-parity` — 8 adapters and 29/29 exact cases, bound to the implementation SHA and exact binary above.
- `npm run verify:core-baseline` — 11/11 deterministic fixture cases matched.
- `npm run test:public-source` — 103/103 main tests and 5/5 native packaging-facing tests passed.
- `npm run test:package` — 38/38 passed.
- `npm run audit:package` — passed; 203 files, 1,038,643 packed bytes; publishing remains unapproved.
- `npm run verify:clean-room` — passed; installed package scanned 12 files and exposed 62 MCP tools without lifecycle scripts or target execution.
- `npm run verify:native-surfaces` — 7 CLI commands, 62 MCP tools, 95 HTTP routes, zero unclassified.
- Exact real corpus — 8/8 pinned repositories across TypeScript, Python, PHP, Rust, Java, Svelte, C#, and Go; zero target-repository writes.
- Exact soak — 2,000/2,000 refresh events passed in persistent and cache-disabled modes with parity and RSS plateau assertions.
- Exact local benchmark/profile — compatibility digest and graph statistics matched for cold, unchanged, and one-file-change states. Results are repository- and machine-specific, not a universal speed claim.
- Exact database-open evidence — captured from metadata-only observations of the same binary.
- Native dogfood contract — `scripts/run-native-dogfood-day.js` records one read-only
  UTC day against the exact source revision and Linux x64 binary; the aggregate
  validator requires seven consecutive days, the full adapter/surface matrix,
  and zero target-repository writes.
- `npm run check:docs`, `npm run check:support`, `npm run check:document-contracts`, and actionlint 1.7.12 — passed.

## Candidate and rollout gates

| Gate | Status | Remaining external evidence |
| --- | --- | --- |
| Six-platform candidate | `pending` | GitHub Actions must build and install the exact Linux, macOS, and Windows x64/arm64 artifacts from the verified source SHA. Local Windows evidence cannot replace these jobs. |
| Candidate manifest/provenance | `pending` | The candidate workflow must checksum the main tarball, six platform tarballs/binaries, raw parity/corpus/benchmark/profile/database/soak/surface evidence, and test summary. |
| Protected promotion | `blocked` | Repository environment approval and immutable candidate provenance must exist before npm or GitHub Release publication. No publication was attempted locally. |
| Seven-day dogfooding | `pending` | Seven consecutive days across the declared repository/adapter/surface matrix cannot be manufactured in one remediation session. |

The GitHub Actions workflow `native-dogfood.yml` carries the exact blocked
candidate binary between daily runs. Each run appends one UTC day to the
artifact-backed window; `native-dogfood.json` remains `pending` until seven
consecutive days validate. A pending window is an explicit negative gate, not a
release approval or a substitute for elapsed evidence.

## Rollout state

`AUDIT REMEDIATION: PASSING — NATIVE DEFAULT/PUBLICATION: NOT YET APPROVED`

The audit blockers and actionable source defects are fixed and locally
verified on the exact implementation SHA. `FLOPEEK_CORE=js` remains the
default; Rust remains shadow/native-experimental until the external candidate,
promotion, and time-based dogfooding gates above produce genuine evidence.
