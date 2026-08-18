const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { findPlatform, platformRegistry } = require("./agent-integration-registry");
const { codexGlobalMcpCheck } = require("./codex-global-mcp");

const AGENT_INTEGRATION_MANIFEST_SCHEMA = "flopeek-agent-integrations/v1";
const MANAGED_BLOCK_START = "# >>> flopeek managed MCP >>>";
const MANAGED_BLOCK_END = "# <<< flopeek managed MCP <<<";
const CANONICAL_SKILL = path.resolve(__dirname, "..", "integrations", "skills", "flopeek");
const TRANSIENT_RENAME_CODES = new Set(["EACCES", "EBUSY", "EPERM"]);

function normalizeRelative(value) {
  return value.split("/").join(path.sep);
}

function wait(milliseconds) {
  if (!milliseconds) return;
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds);
}

function atomicWrite(file, content, options = {}) {
  const fileSystem = options.fileSystem || fs;
  const configuredAttempts = Number(options.attempts);
  const configuredRetryDelay = Number(options.retryDelayMs);
  const attempts = Math.min(Math.max(Number.isFinite(configuredAttempts) && configuredAttempts > 0 ? configuredAttempts : 8, 1), 8);
  const retryDelayMs = Math.min(Math.max(Number.isFinite(configuredRetryDelay) && configuredRetryDelay >= 0 ? configuredRetryDelay : 25, 0), 250);
  const pause = options.wait || wait;
  fileSystem.mkdirSync(path.dirname(file), { recursive: true });
  const temporary = `${file}.${process.pid}.${Date.now()}.tmp`;
  try {
    fileSystem.writeFileSync(temporary, content, "utf8");
    let lastError = null;
    for (let attempt = 1; attempt <= attempts; attempt += 1) {
      try {
        fileSystem.renameSync(temporary, file);
        return { path: file, attempts: attempt };
      } catch (error) {
        lastError = error;
        if (!TRANSIENT_RENAME_CODES.has(error?.code) || attempt === attempts) break;
        pause(Math.min(retryDelayMs * attempt, 250));
      }
    }
    throw lastError;
  } finally {
    if (fileSystem.existsSync(temporary)) fileSystem.rmSync(temporary, { force: true });
  }
}

function filesIn(directory, base = directory) {
  if (!fs.existsSync(directory)) return [];
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) return filesIn(absolute, base);
    return [{ absolute, relative: path.relative(base, absolute).split(path.sep).join("/") }];
  }).sort((left, right) => left.relative.localeCompare(right.relative));
}

function directoryHash(directory) {
  const hash = crypto.createHash("sha256");
  for (const file of filesIn(directory)) {
    hash.update(file.relative);
    hash.update("\0");
    hash.update(fs.readFileSync(file.absolute));
    hash.update("\0");
  }
  return hash.digest("hex");
}

function mcpEntry() {
  return { command: "flopeek", args: ["mcp", "."] };
}

function managedTomlBlock() {
  return [
    MANAGED_BLOCK_START,
    "[mcp_servers.flopeek]",
    'command = "flopeek"',
    'args = ["mcp", "."]',
    MANAGED_BLOCK_END,
  ].join(os.EOL);
}

function sameJson(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function executableCandidates(name, env = process.env) {
  const directories = String(env.PATH || env.Path || "").split(path.delimiter).filter(Boolean);
  const extensions = process.platform === "win32"
    ? String(env.PATHEXT || ".COM;.EXE;.BAT;.CMD;.PS1").split(";").filter(Boolean)
    : [""];
  const hasExtension = Boolean(path.extname(name));
  return directories.flatMap((directory) => (hasExtension ? [path.join(directory, name)] : extensions.map((extension) => path.join(directory, `${name}${extension.toLowerCase()}`)).concat(extensions.map((extension) => path.join(directory, `${name}${extension.toUpperCase()}`)))));
}

function detectExecutable(names, env = process.env) {
  for (const name of names) {
    const found = executableCandidates(name, env).find((candidate) => {
      try { return fs.statSync(candidate).isFile(); } catch { return false; }
    });
    if (found) return { name, path: found };
  }
  return null;
}

function selectPlatforms(selection = "auto", env = process.env) {
  const registry = platformRegistry().platforms;
  const requested = Array.isArray(selection) ? selection : String(selection).split(",").map((item) => item.trim()).filter(Boolean);
  if (!requested.length || requested.includes("auto")) {
    return registry.filter((platform) => platform.status === "supported" && detectExecutable(platform.executables, env));
  }
  if (requested.includes("all")) return registry.filter((platform) => platform.status === "supported");
  return [...new Set(requested)].map((id) => {
    const platform = findPlatform(id);
    if (!platform) throw new Error(`Unknown agent platform: ${id}`);
    if (platform.status !== "supported") throw new Error(`${platform.label} is ${platform.status}: ${platform.reason}`);
    return platform;
  });
}

function readJsonConfig(file) {
  if (!fs.existsSync(file)) return {};
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("root must be an object");
    return parsed;
  } catch (error) {
    throw new Error(`Refusing to modify invalid JSON config ${file}: ${error.message}`);
  }
}

function planSkill(root, platform, action, canonicalHash) {
  const target = path.join(root, normalizeRelative(platform.skillDirectory));
  const exists = fs.existsSync(target);
  if (exists && !fs.statSync(target).isDirectory()) return { platform: platform.id, kind: "skill", path: target, status: "conflict", reason: "The Flopeek skill target exists but is not a directory." };
  if (action === "install") {
    if (!exists) return { platform: platform.id, kind: "skill", path: target, status: "create" };
    if (directoryHash(target) === canonicalHash) return { platform: platform.id, kind: "skill", path: target, status: "unchanged" };
    return { platform: platform.id, kind: "skill", path: target, status: "conflict", reason: "An unmanaged or modified Flopeek skill already exists." };
  }
  if (!exists) return { platform: platform.id, kind: "skill", path: target, status: "absent" };
  if (directoryHash(target) === canonicalHash) return { platform: platform.id, kind: "skill", path: target, status: "remove" };
  return { platform: platform.id, kind: "skill", path: target, status: "conflict", reason: "The installed skill differs from the canonical managed skill; it will not be removed." };
}

function tomlManagedRange(content) {
  const start = content.indexOf(MANAGED_BLOCK_START);
  const endMarker = content.indexOf(MANAGED_BLOCK_END);
  if (start < 0 && endMarker < 0) return null;
  if (start < 0 || endMarker < start) return { invalid: true };
  if (content.indexOf(MANAGED_BLOCK_START, start + MANAGED_BLOCK_START.length) >= 0 || content.indexOf(MANAGED_BLOCK_END, endMarker + MANAGED_BLOCK_END.length) >= 0) return { invalid: true };
  return { start, end: endMarker + MANAGED_BLOCK_END.length, text: content.slice(start, endMarker + MANAGED_BLOCK_END.length) };
}

function planTomlConfig(root, platform, action) {
  const file = path.join(root, normalizeRelative(platform.mcpConfig));
  const content = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
  const range = tomlManagedRange(content);
  const expected = managedTomlBlock();
  if (range?.invalid) return { platform: platform.id, kind: "mcp", path: file, status: "conflict", reason: "Flopeek managed-block markers are incomplete." };
  if (!range && content.includes("[mcp_servers.flopeek]")) return { platform: platform.id, kind: "mcp", path: file, status: "conflict", reason: "An unmanaged Flopeek MCP entry already exists." };
  if (action === "install") {
    if (!range) {
      const separator = content && !content.endsWith("\n") && !content.endsWith("\r") ? os.EOL + os.EOL : content ? os.EOL : "";
      return { platform: platform.id, kind: "mcp", path: file, status: "create", content: `${content}${separator}${expected}${os.EOL}` };
    }
    if (range.text.split("\r\n").join("\n") === expected.split("\r\n").join("\n")) return { platform: platform.id, kind: "mcp", path: file, status: "unchanged" };
    return { platform: platform.id, kind: "mcp", path: file, status: "conflict", reason: "The managed Flopeek MCP block was modified." };
  }
  if (!range) return { platform: platform.id, kind: "mcp", path: file, status: "absent" };
  if (range.text.split("\r\n").join("\n") !== expected.split("\r\n").join("\n")) return { platform: platform.id, kind: "mcp", path: file, status: "conflict", reason: "The managed Flopeek MCP block was modified; it will not be removed." };
  const next = `${content.slice(0, range.start)}${content.slice(range.end)}`;
  return { platform: platform.id, kind: "mcp", path: file, status: "remove", content: next.trim() ? next : "" };
}

function planJsonConfig(root, platform, action) {
  const file = path.join(root, normalizeRelative(platform.mcpConfig));
  let config;
  try { config = readJsonConfig(file); } catch (error) { return { platform: platform.id, kind: "mcp", path: file, status: "conflict", reason: error.message }; }
  if (config.mcpServers !== undefined && (!config.mcpServers || typeof config.mcpServers !== "object" || Array.isArray(config.mcpServers))) {
    return { platform: platform.id, kind: "mcp", path: file, status: "conflict", reason: "The existing mcpServers value must be an object." };
  }
  const current = config.mcpServers?.flopeek;
  const expected = mcpEntry();
  if (action === "install") {
    if (current && !sameJson(current, expected)) return { platform: platform.id, kind: "mcp", path: file, status: "conflict", reason: "An unmanaged or different Flopeek MCP entry already exists." };
    if (current) return { platform: platform.id, kind: "mcp", path: file, status: "unchanged" };
    const next = { ...config, mcpServers: { ...(config.mcpServers || {}), flopeek: expected } };
    return { platform: platform.id, kind: "mcp", path: file, status: "create", content: `${JSON.stringify(next, null, 2)}\n` };
  }
  if (!current) return { platform: platform.id, kind: "mcp", path: file, status: "absent" };
  if (!sameJson(current, expected)) return { platform: platform.id, kind: "mcp", path: file, status: "conflict", reason: "The Flopeek MCP entry differs from the managed value; it will not be removed." };
  const mcpServers = { ...config.mcpServers };
  delete mcpServers.flopeek;
  const next = { ...config };
  if (Object.keys(mcpServers).length) next.mcpServers = mcpServers;
  else delete next.mcpServers;
  return { platform: platform.id, kind: "mcp", path: file, status: "remove", content: `${JSON.stringify(next, null, 2)}\n` };
}

function planPlatform(root, platform, action, canonicalHash) {
  const skill = planSkill(root, platform, action, canonicalHash);
  const config = platform.configFormat === "toml-managed-block"
    ? planTomlConfig(root, platform, action)
    : planJsonConfig(root, platform, action);
  return [skill, config];
}

function manifestPath(root) {
  return path.join(root, ".flopeek", "agent-integrations.json");
}

function readManifest(root) {
  const file = manifestPath(root);
  if (!fs.existsSync(file)) return null;
  try {
    const manifest = JSON.parse(fs.readFileSync(file, "utf8"));
    return manifest?.schemaVersion === AGENT_INTEGRATION_MANIFEST_SCHEMA ? manifest : null;
  } catch {
    return null;
  }
}

function executePlan(root, platforms, action, plan, canonicalHash, dryRun) {
  const conflicts = plan.filter((item) => item.status === "conflict");
  if (conflicts.length || dryRun) return;
  for (const item of plan) {
    if (item.kind === "skill" && item.status === "create") {
      fs.mkdirSync(path.dirname(item.path), { recursive: true });
      fs.cpSync(CANONICAL_SKILL, item.path, { recursive: true, errorOnExist: true });
    } else if (item.kind === "skill" && item.status === "remove") {
      fs.rmSync(item.path, { recursive: true });
    } else if (item.kind === "mcp" && ["create", "remove"].includes(item.status)) {
      if (!item.content && action === "uninstall") {
        if (fs.existsSync(item.path)) fs.rmSync(item.path);
      } else atomicWrite(item.path, item.content);
    }
  }
  if (action === "install") {
    const existing = readManifest(root);
    const installedPlatforms = new Set([...(existing?.platforms || []), ...platforms.map((platform) => platform.id)]);
    const installedFiles = new Map((existing?.files || []).map((item) => [`${item.platform}:${item.kind}:${item.path}`, item]));
    for (const item of plan.filter((candidate) => ["create", "unchanged"].includes(candidate.status))) {
      const record = { platform: item.platform, kind: item.kind, path: path.relative(root, item.path).split(path.sep).join("/") };
      installedFiles.set(`${record.platform}:${record.kind}:${record.path}`, record);
    }
    const manifest = {
      schemaVersion: AGENT_INTEGRATION_MANIFEST_SCHEMA,
      installedAt: existing?.installedAt || new Date().toISOString(),
      updatedAt: new Date().toISOString(),
      canonicalSkillHash: canonicalHash,
      platforms: [...installedPlatforms].sort(),
      files: [...installedFiles.values()].sort((left, right) => `${left.platform}:${left.kind}`.localeCompare(`${right.platform}:${right.kind}`)),
    };
    atomicWrite(manifestPath(root), `${JSON.stringify(manifest, null, 2)}\n`);
  } else {
    const manifest = readManifest(root);
    if (!manifest) return;
    const removed = new Set(platforms.map((platform) => platform.id));
    const remaining = (manifest.platforms || []).filter((id) => !removed.has(id));
    if (!remaining.length) {
      if (fs.existsSync(manifestPath(root))) fs.rmSync(manifestPath(root));
    } else {
      atomicWrite(manifestPath(root), `${JSON.stringify({ ...manifest, platforms: remaining, files: (manifest.files || []).filter((item) => !removed.has(item.platform)) }, null, 2)}\n`);
    }
  }
}

function integrationAction(root, action, options = {}) {
  const repository = fs.realpathSync(root);
  if (!fs.existsSync(path.join(CANONICAL_SKILL, "SKILL.md"))) throw new Error(`Canonical Flopeek skill is missing: ${CANONICAL_SKILL}`);
  let selection = options.platforms || "auto";
  if (action === "uninstall" && (selection === "auto" || (Array.isArray(selection) && selection.includes("auto")))) {
    const installed = readManifest(repository)?.platforms || [];
    if (installed.length) selection = installed;
  }
  const platforms = selectPlatforms(selection, options.env || process.env);
  if (!platforms.length) throw new Error("No supported local agent host was detected. Pass --platform codex, claude, cursor, gemini, or all explicitly.");
  const canonicalHash = directoryHash(CANONICAL_SKILL);
  const plan = platforms.flatMap((platform) => planPlatform(repository, platform, action, canonicalHash));
  const conflicts = plan.filter((item) => item.status === "conflict");
  const warnings = platforms.some((platform) => platform.id === "codex")
    ? [codexGlobalMcpCheck(repository, options)].filter((check) => check.status === "warning")
    : [];
  executePlan(repository, platforms, action, plan, canonicalHash, Boolean(options.dryRun));
  return {
    schemaVersion: AGENT_INTEGRATION_MANIFEST_SCHEMA,
    action,
    repository,
    dryRun: Boolean(options.dryRun),
    ok: conflicts.length === 0,
    platforms: platforms.map((platform) => platform.id),
    plan: plan.map((item) => ({ ...item, content: undefined })),
    conflicts,
    warnings,
    manifest: action === "install" && !options.dryRun && !conflicts.length ? manifestPath(repository) : null,
  };
}

function installAgentIntegration(root, options = {}) {
  return integrationAction(root, "install", options);
}

function uninstallAgentIntegration(root, options = {}) {
  return integrationAction(root, "uninstall", options);
}

function doctorAgentIntegration(root, options = {}) {
  const repository = fs.realpathSync(root);
  if (!fs.existsSync(path.join(CANONICAL_SKILL, "SKILL.md"))) throw new Error(`Canonical Flopeek skill is missing: ${CANONICAL_SKILL}`);
  const selection = options.platforms || "all";
  const platforms = selectPlatforms(selection, options.env || process.env);
  const canonicalHash = directoryHash(CANONICAL_SKILL);
  const checks = [];
  const nodeMajor = Number(process.versions.node.split(".")[0]);
  checks.push({ id: "node", status: nodeMajor >= 20 ? "pass" : "error", message: `Node.js ${process.versions.node}; Flopeek requires Node.js 20 or newer.` });
  const flopeekCommand = detectExecutable(["flopeek"], options.env || process.env);
  checks.push({ id: "flopeek-command", status: flopeekCommand ? "pass" : "warning", message: flopeekCommand ? `Flopeek detected at ${flopeekCommand.path}.` : "The flopeek command must be available on PATH for MCP hosts." });
  for (const platform of platforms) {
    const executable = detectExecutable(platform.executables, options.env || process.env);
    checks.push({ id: `${platform.id}:host`, platform: platform.id, status: executable ? "pass" : "warning", message: executable ? `${platform.label} detected at ${executable.path}.` : `${platform.label} executable was not detected on PATH.` });
    if (platform.id === "codex") checks.push(codexGlobalMcpCheck(repository, options));
    for (const item of planPlatform(repository, platform, "install", canonicalHash)) {
      checks.push({ id: `${platform.id}:${item.kind}`, platform: platform.id, status: item.status === "unchanged" ? "pass" : item.status === "conflict" ? "error" : "warning", message: item.status === "unchanged" ? `${item.kind} integration is current.` : item.reason || `${item.kind} integration is not installed.` });
    }
  }
  const errors = checks.filter((check) => check.status === "error");
  const warnings = checks.filter((check) => check.status === "warning");
  return {
    schemaVersion: "flopeek-agent-integration-doctor/v1",
    repository,
    ok: errors.length === 0 && (!options.strict || warnings.length === 0),
    strict: Boolean(options.strict),
    summary: { passed: checks.filter((check) => check.status === "pass").length, warnings: warnings.length, errors: errors.length },
    checks,
  };
}

module.exports = {
  atomicWrite,
  AGENT_INTEGRATION_MANIFEST_SCHEMA,
  CANONICAL_SKILL,
  detectExecutable,
  directoryHash,
  doctorAgentIntegration,
  installAgentIntegration,
  selectPlatforms,
  uninstallAgentIntegration,
};
