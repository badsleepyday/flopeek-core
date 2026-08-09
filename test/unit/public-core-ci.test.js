"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const ROOT = path.resolve(__dirname, "..", "..");
const readWorkflow = (name) => fs.readFileSync(path.join(ROOT, ".github", "workflows", name), "utf8");
const assertThirdPartyActionsPinned = (workflow) => {
  for (const match of workflow.matchAll(/uses:\s+([^\s]+)@([^\s]+)/gu)) {
    assert.match(match[2], /^[a-f0-9]{40}$/u, `${match[1]} must use a full commit SHA`);
  }
};

test("public Core CI separates quality, compatibility, native platform, and packaging authority", () => {
  const workflow = readWorkflow("ci.yml");
  const publicSourceRunner = fs.readFileSync(path.join(ROOT, "scripts", "run-tests.js"), "utf8");
  assert.match(workflow, /push:\s*\n\s+branches:\s*\[main, development\]/);
  assert.match(workflow, /pull_request:/);
  assert.match(workflow, /schedule:[\s\S]*cron: '17 3 \* \* 1'/);
  assert.match(workflow, /os:\s*\[ubuntu-latest, windows-latest, macos-latest\]/);
  assert.match(workflow, /node:\s*\[22, 24\]/);
  assert.match(workflow, /runs-on:\s*\$\{\{ matrix\.os \}\}/);
  assert.match(workflow, /quality:\s*\n\s+name: Linux quality\s*\n\s+runs-on: ubuntu-latest/);
  assert.match(workflow, /node-compatibility:[\s\S]*runs-on: ubuntu-latest[\s\S]*npm run test:node-compat/);
  assert.match(workflow, /native-platform:[\s\S]*node-version: 22[\s\S]*npm run test:native-platform/);
  assert.match(workflow, /packaging:[\s\S]*npm run test:package[\s\S]*npm run audit:package[\s\S]*npm run verify:clean-room/);
  assert.match(workflow, /workflow_call:/);
  assert.match(workflow, /ref:\s*\$\{\{ inputs\.source_sha \|\| github\.sha \}\}/);
  assert.match(workflow, /uses: actions\/checkout@[a-f0-9]{40}/);
  assert.match(workflow, /uses: actions\/setup-node@[a-f0-9]{40}/);
  assert.match(workflow, /uses: dtolnay\/rust-toolchain@[a-f0-9]{40}[^\n]*[\s\S]*?toolchain:\s*\$\{\{ steps\.toolchain-contract\.outputs\.rust \}\}/);
  assert.match(workflow, /uses: actions\/setup-dotnet@[a-f0-9]{40}/);
  assert.match(workflow, /global-json-file:\s*global\.json/);
  assert.match(workflow, /uses: actions\/setup-go@[a-f0-9]{40}/);
  assert.match(workflow, /go-version:\s*'1\.26\.4'/);
  assert.match(workflow, /setup-go@[a-f0-9]{40}[^\n]*[\s\S]*?cache:\s*false/);
  assert.match(workflow, /npm run verify:native-adapter-parity -- --output native-adapter-parity\.json/);
  assert.match(workflow, /name: adapter-parity-\$\{\{ matrix\.os \}\}-node-22/);
  assert.match(workflow, /uses: EmbarkStudios\/cargo-deny-action@[a-f0-9]{40}[\s\S]*manifest-path: native\/flopeek-core\/Cargo\.toml[\s\S]*command-arguments: advisories bans licenses sources/);
  assert.match(workflow, /uses: google\/osv-scanner-action\/osv-scanner-action@[a-f0-9]{40}[\s\S]*--lockfile=package-lock\.json[\s\S]*--lockfile=native\/flopeek-core\/Cargo\.lock/);
  assert.match(publicSourceRunner, /lanes\["public-source"\]\.unshift\("test\/unit\/native-inventory-parity\.test\.js"\)/);
  for (const command of ["npm run verify:toolchains", "cargo fmt --check --manifest-path native/flopeek-core/Cargo.toml", "cargo clippy --locked --manifest-path native/flopeek-core/Cargo.toml -- -D warnings", "cargo test --locked --manifest-path native/flopeek-core/Cargo.toml", "npm run verify:native-js-parser-parity", "npm run test:unit", "npm run test:contracts", "node scripts/verify-branch-name.js", "npm run verify:core-baseline", "npm run test:native-platform", "npm run test:package", "npm run audit:package", "npm run verify:clean-room"]) {
    assert.match(workflow, new RegExp(`- run: ${command.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`));
  }
  assert.equal((workflow.match(/cargo fmt --check --manifest-path/g) || []).length, 1);
  assert.equal((workflow.match(/cargo clippy --locked --manifest-path/g) || []).length, 1);
  assert.doesNotMatch(workflow, /npm run test:native-core/);
  assert.doesNotMatch(workflow, /cargo run --quiet/);
  const packageJson = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8"));
  assert.match(packageJson.scripts["test:native-core"], /npm run test:native-runtime/);
  assert.doesNotMatch(packageJson.scripts["test:native-runtime"], /cargo (?:fmt|clippy|test)/);
  assert.match(packageJson.scripts["test:native-platform"], /node scripts\/smoke-native-release\.js/);
  assertThirdPartyActionsPinned(workflow);
});

test("dependency automation covers npm and Cargo without auto-merge", () => {
  const dependabot = fs.readFileSync(path.join(ROOT, ".github", "dependabot.yml"), "utf8");
  assert.match(dependabot, /package-ecosystem: npm[\s\S]*directory: \//);
  assert.match(dependabot, /package-ecosystem: cargo[\s\S]*directory: \/native\/flopeek-core/);
  assert.doesNotMatch(dependabot, /auto-merge|automerge/iu);

  const deny = fs.readFileSync(path.join(ROOT, "deny.toml"), "utf8");
  assert.match(deny, /\[advisories\][\s\S]*ignore = \[\]/);
  assert.match(deny, /\[sources\][\s\S]*unknown-registry = "deny"[\s\S]*unknown-git = "deny"/);
});

test("candidate workflow builds each platform once and produces one immutable complete bundle", () => {
  const workflow = readWorkflow("native-candidate.yml");
  assert.match(workflow, /workflow_dispatch:/);
  assert.match(workflow, /name: Full required correctness CI/);
  assert.doesNotMatch(workflow, /Full six-cell correctness CI/);
  for (const input of ["source_sha", "package_version", "release_channel"]) {
    assert.match(workflow, new RegExp(`\\n\\s{6}${input}:`));
  }
  assert.match(workflow, /uses: \.\/\.github\/workflows\/ci\.yml[\s\S]*source_sha: \$\{\{ inputs\.source_sha \}\}/);
  assert.equal((workflow.match(/cargo build --release --locked/g) || []).length, 1);
  assert.match(workflow, /matrix:[\s\S]*@flopeek\/native-win32-x64[\s\S]*@flopeek\/native-win32-arm64[\s\S]*@flopeek\/native-linux-x64-gnu[\s\S]*@flopeek\/native-linux-arm64-gnu[\s\S]*@flopeek\/native-darwin-x64[\s\S]*@flopeek\/native-darwin-arm64/);
  assert.match(workflow, /tar -xOf "\$tarball" package\/bin\/flopeek-native-core/);
  assert.match(workflow, /run-native-candidate-evidence\.js/);
  assert.match(workflow, /create-native-dogfood-pending\.js/);
  assert.match(workflow, /build-native-rollout-evidence\.js/);
  assert.match(workflow, /--dogfood "\$CANDIDATE_EVIDENCE\/native-dogfood\.json"/);
  assert.match(workflow, /CANDIDATE_BUNDLE=\$\{\{ runner\.temp \}\}\/flopeek-native-bundle/);
  assert.match(workflow, /npm pack --json --pack-destination \$env:CANDIDATE_BUNDLE/);
  assert.match(workflow, /--output "\$CANDIDATE_EVIDENCE\/native-rollout-evidence\.json"/);
  assert.match(workflow, /build-native-release-manifest\.js/);
  assert.match(workflow, /name: Clean install \$\{\{ matrix\.package \}\}/);
  assert.match(workflow, /--require-platform-install-evidence/);
  assert.match(workflow, /pattern: native-candidate-install-\*/);
  assert.match(workflow, /name: native-candidate-bundle/);
  assert.doesNotMatch(workflow, /npm publish|gh release create|git push/);
  assert.doesNotMatch(workflow, /continue-on-error|\|\|\s*true/);
  assertThirdPartyActionsPinned(workflow);
});

test("native dogfood workflow accumulates only exact revision-bound UTC days", () => {
  const workflow = readWorkflow("native-dogfood.yml");
  assert.match(workflow, /workflow_dispatch:/);
  for (const input of ["source_sha", "candidate_run_id", "binary_sha256", "previous_dogfood_run_id"]) {
    assert.match(workflow, new RegExp(`\\n\\s{6}${input}:`));
  }
  assert.match(workflow, /actions\/download-artifact@[a-f0-9]{40}[\s\S]*run-id: \$\{\{ inputs\.candidate_run_id \}\}/);
  assert.match(workflow, /run-native-dogfood-day\.js/);
  assert.match(workflow, /build-native-dogfood-evidence\.js/);
  assert.match(workflow, /verify-native-dogfood\.js/);
  assert.doesNotMatch(workflow, /continue-on-error|\|\|\s*true/);
  assertThirdPartyActionsPinned(workflow);
});

test("promotion workflow consumes a cross-run candidate behind protected environments without rebuilding", () => {
  const workflow = readWorkflow("native-promotion.yml");
  assert.match(workflow, /workflow_dispatch:/);
  for (const input of ["candidate_run_id", "release_manifest_sha256", "release_channel", "dry_run"]) {
    assert.match(workflow, new RegExp(`\\n\\s{6}${input}:`));
  }
  assert.match(workflow, /environment: \$\{\{ inputs\.dry_run && 'native-release-promotion-dry-run' \|\| 'native-release-promotion' \}\}/);
  assert.match(workflow, /uses: actions\/download-artifact@[a-f0-9]{40}[^\n]*[\s\S]*github-token: \$\{\{ github\.token \}\}[\s\S]*run-id: \$\{\{ inputs\.candidate_run_id \}\}/);
  assert.match(workflow, /--expected-manifest-sha256 "\$\{\{ inputs\.release_manifest_sha256 \}\}"/);
  assert.match(workflow, /actions\/runs\/\$\{\{ inputs\.candidate_run_id \}\}/);
  assert.match(workflow, /run\.path -ne '\.github\/workflows\/native-candidate\.yml'/);
  assert.match(workflow, /run\.conclusion -ne 'success'/);
  assert.match(workflow, /verify-native-candidate-install\.js/);
  assert.match(workflow, /publish-npm-release-set\.js --assets candidate-bundle/);
  assert.match(workflow, /verify-native-registry-release\.js/);
  assert.match(workflow, /generate-native-promotion-attestation\.js/);
  assert.match(workflow, /gh release create/);
  assert.doesNotMatch(workflow, /cargo (?:build|run)|npm pack/);
  assert.doesNotMatch(workflow, /packaging\/github-release-approval\.json|continue-on-error|\|\|\s*true/);
  assert.doesNotMatch(workflow, /id-token:\s*write/);
  assertThirdPartyActionsPinned(workflow);
});
