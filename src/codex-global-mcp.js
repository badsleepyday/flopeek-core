const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const GLOBAL_MCP_SECTION = /^\s*\[\s*mcp_servers\s*\.\s*["']?(flopeek|flowpeek)["']?\s*\]\s*(?:#.*)?$/gimu;

function comparablePath(value) {
  const resolved = path.resolve(value);
  return process.platform === "win32" ? resolved.toLowerCase() : resolved;
}

function codexHome(options = {}) {
  const env = options.env || process.env;
  if (options.codexHome) return path.resolve(options.codexHome);
  if (env.CODEX_HOME) return path.resolve(env.CODEX_HOME);
  return path.join(path.resolve(options.homeDirectory || os.homedir()), ".codex");
}

function inspectGlobalCodexMcp(repository, options = {}) {
  const file = path.join(codexHome(options), "config.toml");
  const projectFile = path.join(path.resolve(repository), ".codex", "config.toml");
  if (comparablePath(file) === comparablePath(projectFile) || !fs.existsSync(file)) return null;

  let content;
  try {
    content = fs.readFileSync(file, "utf8");
  } catch (error) {
    return {
      id: "codex:global-mcp",
      platform: "codex",
      status: "warning",
      path: file,
      names: [],
      message: `Codex global config could not be inspected (${error.message}). Verify that it does not define a repository-bound Flopeek or Flowpeek MCP entry.`,
    };
  }

  const names = [...new Set([...content.matchAll(GLOBAL_MCP_SECTION)].map((match) => match[1].toLowerCase()))].sort();
  if (!names.length) return null;
  return {
    id: "codex:global-mcp",
    platform: "codex",
    status: "warning",
    path: file,
    names,
    message: `Codex global config defines ${names.map((name) => `mcp_servers.${name}`).join(", ")}. A global stdio entry can bind the MCP process to the wrong repository; remove that global entry and keep the project-scoped .codex/config.toml integration.`,
  };
}

function codexGlobalMcpCheck(repository, options = {}) {
  return inspectGlobalCodexMcp(repository, options) || {
    id: "codex:global-mcp",
    platform: "codex",
    status: "pass",
    message: "No global Codex Flopeek or Flowpeek MCP entry was detected.",
  };
}

module.exports = { codexGlobalMcpCheck, codexHome, inspectGlobalCodexMcp };
