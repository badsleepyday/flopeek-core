const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");
const { atomicWrite, doctorAgentIntegration, installAgentIntegration, uninstallAgentIntegration } = require("../../src/agent-integration");
const { platformRegistry } = require("../../src/agent-integration-registry");

function repository(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "flopeek-agent-integration-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return root;
}

test("platform registry distinguishes supported local hosts from ChatGPT web", () => {
  const registry = platformRegistry();
  assert.equal(registry.schemaVersion, "flopeek-agent-platform-registry/v1");
  assert.deepEqual(registry.platforms.map((item) => item.id), ["claude", "codex", "cursor", "gemini", "chatgpt-web"]);
  assert.equal(registry.platforms.find((item) => item.id === "chatgpt-web").status, "remote-only");
  assert.equal(registry.platforms.find((item) => item.id === "codex").skillDirectory, ".agents/skills/flopeek");
});

test("Codex install is project-local, idempotent, and preserves unrelated TOML", (t) => {
  const root = repository(t);
  fs.mkdirSync(path.join(root, ".codex"), { recursive: true });
  fs.writeFileSync(path.join(root, ".codex", "config.toml"), "model = \"example\"\n", "utf8");

  const first = installAgentIntegration(root, { platforms: ["codex"] });
  const second = installAgentIntegration(root, { platforms: ["codex"] });

  assert.equal(first.ok, true);
  assert.equal(second.ok, true);
  assert.ok(second.plan.every((item) => item.status === "unchanged"));
  assert.ok(fs.existsSync(path.join(root, ".agents", "skills", "flopeek", "SKILL.md")));
  const config = fs.readFileSync(path.join(root, ".codex", "config.toml"), "utf8");
  assert.match(config, /model = "example"/);
  assert.match(config, /\[mcp_servers\.flopeek\]/);
  assert.ok(fs.existsSync(path.join(root, ".flopeek", "agent-integrations.json")));

  const removed = uninstallAgentIntegration(root, { platforms: ["codex"] });
  assert.equal(removed.ok, true);
  assert.equal(fs.existsSync(path.join(root, ".agents", "skills", "flopeek")), false);
  assert.equal(fs.readFileSync(path.join(root, ".codex", "config.toml"), "utf8").includes("model = \"example\""), true);
  assert.equal(fs.readFileSync(path.join(root, ".codex", "config.toml"), "utf8").includes("mcp_servers.flopeek"), false);
});

test("Codex install and doctor report global Flopeek and legacy Flowpeek MCP entries without modifying them", (t) => {
  for (const name of ["flopeek", "flowpeek"]) {
    const root = repository(t);
    const codexHome = path.join(root, "user-codex");
    const globalConfig = path.join(codexHome, "config.toml");
    fs.mkdirSync(codexHome, { recursive: true });
    const content = `[mcp_servers.${name}]\ncommand = "${name}"\nargs = ["mcp", "C:/wrong-project"]\n`;
    fs.writeFileSync(globalConfig, content, "utf8");

    const installed = installAgentIntegration(root, { platforms: ["codex"], codexHome });
    assert.equal(installed.ok, true);
    assert.equal(installed.warnings.length, 1);
    assert.equal(installed.warnings[0].id, "codex:global-mcp");
    assert.deepEqual(installed.warnings[0].names, [name]);
    assert.equal(fs.readFileSync(globalConfig, "utf8"), content);

    const doctor = doctorAgentIntegration(root, { platforms: ["codex"], codexHome, env: { PATH: "" }, strict: true });
    assert.equal(doctor.ok, false);
    assert.equal(doctor.checks.find((check) => check.id === "codex:global-mcp").status, "warning");
    assert.equal(fs.readFileSync(globalConfig, "utf8"), content);
  }
});

test("JSON host install and uninstall preserve unrelated MCP servers and settings", (t) => {
  const root = repository(t);
  fs.mkdirSync(path.join(root, ".gemini"), { recursive: true });
  fs.writeFileSync(path.join(root, ".gemini", "settings.json"), JSON.stringify({ theme: "dark", mcpServers: { existing: { command: "other" } } }, null, 2));

  assert.equal(installAgentIntegration(root, { platforms: ["gemini"] }).ok, true);
  let config = JSON.parse(fs.readFileSync(path.join(root, ".gemini", "settings.json"), "utf8"));
  assert.deepEqual(config.mcpServers.flopeek, { command: "flopeek", args: ["mcp", "."] });
  assert.equal(config.mcpServers.existing.command, "other");
  assert.equal(config.theme, "dark");

  assert.equal(uninstallAgentIntegration(root, { platforms: ["gemini"] }).ok, true);
  config = JSON.parse(fs.readFileSync(path.join(root, ".gemini", "settings.json"), "utf8"));
  assert.equal(config.mcpServers.flopeek, undefined);
  assert.equal(config.mcpServers.existing.command, "other");
  assert.equal(config.theme, "dark");
});

test("agent integration atomic writes retry a transient Windows-style replacement lock", () => {
  const files = new Map();
  const target = "C:\\workspace\\.gemini\\settings.json";
  const fileSystem = {
    mkdirSync() {},
    writeFileSync(file, content) { files.set(file, content); },
    renameSync(from, to) {
      this.renameAttempts = (this.renameAttempts || 0) + 1;
      if (this.renameAttempts < 3) {
        const error = new Error("temporary lock");
        error.code = "EPERM";
        throw error;
      }
      files.set(to, files.get(from));
      files.delete(from);
    },
    existsSync(file) { return files.has(file); },
    rmSync(file) { files.delete(file); },
    renameAttempts: 0,
  };
  const delays = [];

  const result = atomicWrite(target, "{\"mcpServers\":{}}\n", {
    fileSystem,
    attempts: 3,
    retryDelayMs: 10,
    wait(milliseconds) { delays.push(milliseconds); },
  });

  assert.equal(result.attempts, 3);
  assert.equal(fileSystem.renameAttempts, 3);
  assert.deepEqual(delays, [10, 20]);
  assert.equal(files.get(target), "{\"mcpServers\":{}}\n");
  assert.equal([...files.keys()].some((file) => file.endsWith(".tmp")), false);
});

test("separate installs merge ownership and auto-uninstall removes every managed platform", (t) => {
  const root = repository(t);
  assert.equal(installAgentIntegration(root, { platforms: ["codex"] }).ok, true);
  assert.equal(installAgentIntegration(root, { platforms: ["gemini"] }).ok, true);
  const manifest = JSON.parse(fs.readFileSync(path.join(root, ".flopeek", "agent-integrations.json"), "utf8"));
  assert.deepEqual(manifest.platforms, ["codex", "gemini"]);
  const removed = uninstallAgentIntegration(root, { platforms: "auto", env: { PATH: "" } });
  assert.equal(removed.ok, true);
  assert.deepEqual(removed.platforms, ["codex", "gemini"]);
  assert.equal(fs.existsSync(path.join(root, ".agents", "skills", "flopeek")), false);
  assert.equal(fs.existsSync(path.join(root, ".gemini", "skills", "flopeek")), false);
});

test("install dry-run writes nothing", (t) => {
  const root = repository(t);
  const result = installAgentIntegration(root, { platforms: ["claude"], dryRun: true });
  assert.equal(result.ok, true);
  assert.equal(result.dryRun, true);
  assert.equal(fs.existsSync(path.join(root, ".claude")), false);
  assert.equal(fs.existsSync(path.join(root, ".mcp.json")), false);
  assert.equal(fs.existsSync(path.join(root, ".flopeek")), false);
});

test("install preflight refuses unmanaged entries and does not partially copy the skill", (t) => {
  const root = repository(t);
  fs.writeFileSync(path.join(root, ".mcp.json"), JSON.stringify({ mcpServers: { flopeek: { command: "custom" } } }));
  const result = installAgentIntegration(root, { platforms: ["claude"] });
  assert.equal(result.ok, false);
  assert.equal(result.conflicts.length, 1);
  assert.equal(fs.existsSync(path.join(root, ".claude", "skills", "flopeek")), false);
  assert.equal(JSON.parse(fs.readFileSync(path.join(root, ".mcp.json"), "utf8")).mcpServers.flopeek.command, "custom");
});

test("install refuses a non-directory skill target", (t) => {
  const root = repository(t);
  fs.mkdirSync(path.join(root, ".agents", "skills"), { recursive: true });
  fs.writeFileSync(path.join(root, ".agents", "skills", "flopeek"), "owned by user");
  const result = installAgentIntegration(root, { platforms: ["codex"] });
  assert.equal(result.ok, false);
  assert.match(result.conflicts[0].reason, /not a directory/);
  assert.equal(fs.readFileSync(path.join(root, ".agents", "skills", "flopeek"), "utf8"), "owned by user");
});

test("install refuses malformed JSON without replacing it", (t) => {
  const root = repository(t);
  fs.mkdirSync(path.join(root, ".cursor"), { recursive: true });
  fs.writeFileSync(path.join(root, ".cursor", "mcp.json"), "{broken", { encoding: "utf8", flag: "w" });
  const result = installAgentIntegration(root, { platforms: ["cursor"] });
  assert.equal(result.ok, false);
  assert.match(result.conflicts[0].reason, /invalid JSON/);
  assert.equal(fs.readFileSync(path.join(root, ".cursor", "mcp.json"), "utf8"), "{broken");
});

test("install rejects a non-object mcpServers value", (t) => {
  const root = repository(t);
  fs.writeFileSync(path.join(root, ".mcp.json"), JSON.stringify({ mcpServers: "invalid" }));
  const result = installAgentIntegration(root, { platforms: ["claude"] });
  assert.equal(result.ok, false);
  assert.match(result.conflicts[0].reason, /must be an object/);
});

test("doctor reports integration state without executing an agent host", (t) => {
  const root = repository(t);
  installAgentIntegration(root, { platforms: ["codex"] });
  const result = doctorAgentIntegration(root, { platforms: ["codex"], env: { PATH: "" } });
  assert.equal(result.ok, true);
  assert.equal(result.checks.find((check) => check.id === "codex:skill").status, "pass");
  assert.equal(result.checks.find((check) => check.id === "codex:mcp").status, "pass");
  assert.equal(result.checks.find((check) => check.id === "codex:host").status, "warning");
  assert.equal(doctorAgentIntegration(root, { platforms: ["codex"], env: { PATH: "" }, strict: true }).ok, false);
});

test("local installer explicitly rejects remote-only ChatGPT web", (t) => {
  const root = repository(t);
  assert.throws(() => installAgentIntegration(root, { platforms: ["chatgpt-web"] }), /remote-only/);
});

test("CLI install, doctor, and uninstall expose machine-readable integration results", (t) => {
  const root = repository(t);
  const cli = path.resolve(__dirname, "..", "..", "src", "cli.js");
  const run = (...args) => spawnSync(process.execPath, [cli, ...args, root, "--platform", "codex", "--format", "json"], { encoding: "utf8" });

  const installed = run("install");
  assert.equal(installed.status, 0, installed.stderr);
  assert.equal(JSON.parse(installed.stdout).action, "install");

  const doctor = run("doctor");
  assert.equal(doctor.status, 0, doctor.stderr);
  assert.equal(JSON.parse(doctor.stdout).checks.find((check) => check.id === "codex:mcp").status, "pass");

  const removed = run("uninstall");
  assert.equal(removed.status, 0, removed.stderr);
  assert.equal(JSON.parse(removed.stdout).action, "uninstall");
});
