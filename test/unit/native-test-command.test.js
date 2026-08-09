"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const { nativeTestCommand } = require("../helpers/native-test-command");

test("native proof harness binds to the exact candidate binary when provided", () => {
  const root = path.resolve("fixture-root");
  const binary = path.resolve("candidate", "flopeek-native-core");
  assert.deepEqual(nativeTestCommand(root, { FLOPEEK_NATIVE_CORE_BINARY: binary }), {
    command: binary,
    args: [],
    cwd: root,
  });
});

test("native proof harness retains the source-backed Cargo path outside candidate evidence", () => {
  const root = path.resolve("fixture-root");
  assert.deepEqual(nativeTestCommand(root, {}), {
    command: "cargo",
    args: ["run", "--quiet", "--manifest-path", path.join(root, "native", "flopeek-core", "Cargo.toml"), "--"],
    cwd: root,
  });
});

test("native proof harness reuses an existing release binary instead of compiling a debug process", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "flopeek-native-command-release-"));
  try {
    const binaryName = process.platform === "win32" ? "flopeek-native-core.exe" : "flopeek-native-core";
    const binary = path.join(root, "native", "flopeek-core", "target", "release", binaryName);
    fs.mkdirSync(path.dirname(binary), { recursive: true });
    fs.writeFileSync(binary, "candidate");
    assert.deepEqual(nativeTestCommand(root, {}), { command: binary, args: [], cwd: root });
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
