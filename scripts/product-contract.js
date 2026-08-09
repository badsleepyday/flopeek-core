"use strict";

const fs = require("node:fs");
const path = require("node:path");

const PRODUCT_CONTRACT_SCHEMA = "flopeek-product-contract/v1";
const START = "<!-- GENERATED:PRODUCT-CONTRACT:START -->";
const END = "<!-- GENERATED:PRODUCT-CONTRACT:END -->";
const DOCUMENTS = Object.freeze([
  "README.md",
  "ROADMAP.md",
  "ARCHITECTURE.md",
  "native/flopeek-core/README.md",
]);

function canonicalText(source) {
  return String(source).replace(/\r\n?/g, "\n");
}

function readJson(root, relativePath) {
  return JSON.parse(fs.readFileSync(path.join(root, relativePath), "utf8"));
}

function minimumNodeMajor(range) {
  const match = /^>=(\d+)$/.exec(String(range || "").trim());
  if (!match) throw new Error(`package.json engines.node must be an exact minimum range such as >=22; received ${JSON.stringify(range)}.`);
  return Number(match[1]);
}

function compactAdapter(adapter) {
  return {
    id: adapter.id,
    languages: [...adapter.languages],
    parser: adapter.parser,
    availability: adapter.availability,
    requiredToolchain: adapter.requiredToolchain,
    capabilities: adapter.capabilities,
  };
}

function buildProductContractFromInputs(inputs) {
  const {
    packageJson,
    packagePolicy,
    rolloutEvidence,
    npmApproval,
    githubApproval,
    cleanRoomEvidence,
    coreModes,
    javascriptAdapters,
    nativeAdapters,
  } = inputs;
  const nodeMinimum = minimumNodeMajor(packageJson.engines?.node);
  if (packagePolicy.package?.name !== packageJson.name) throw new Error("Package policy name differs from package.json.");
  if (packagePolicy.package?.minimumNodeMajor !== nodeMinimum) throw new Error("Package policy Node minimum differs from package.json engines.node.");
  if (npmApproval.packageName !== packageJson.name || npmApproval.version !== packageJson.version) {
    throw new Error("npm publication approval identity differs from the source package identity.");
  }
  if (npmApproval.distTag !== packagePolicy.package?.publication?.distTag) {
    throw new Error("npm publication approval dist-tag differs from package policy.");
  }
  if (rolloutEvidence.binding?.packageVersion !== packageJson.version) {
    throw new Error("Native rollout evidence package version differs from package.json.");
  }
  if (!Array.isArray(coreModes) || !["js", "shadow", "native", "native-experimental"].every((mode) => coreModes.includes(mode))) {
    throw new Error("Core mode contract is incomplete.");
  }
  const preview = cleanRoomEvidence.packageAudit?.package;
  if (!preview?.version || cleanRoomEvidence.status !== "passed") throw new Error("Last verified preview evidence is missing or incomplete.");
  const nativeRolloutComplete = rolloutEvidence.status === "complete"
    && rolloutEvidence.decision?.eligible !== false;
  return {
    schemaVersion: PRODUCT_CONTRACT_SCHEMA,
    package: {
      name: packageJson.name,
      sourceVersion: packageJson.version,
      enginesNode: packageJson.engines.node,
      minimumNodeMajor: nodeMinimum,
      distTag: npmApproval.distTag,
      lastVerifiedPreview: {
        version: preview.version,
        status: cleanRoomEvidence.status,
        evidenceClass: cleanRoomEvidence.evidenceClass,
      },
    },
    core: {
      modes: [...coreModes],
      publicDefaultMode: "js",
      publicDefaultImplementation: "javascript",
      experimentalMode: "native-experimental",
      experimentalImplementation: "native",
      nativeRolloutStatus: rolloutEvidence.status,
      nativeDefaultEligible: nativeRolloutComplete,
    },
    release: {
      npm: { status: npmApproval.status, version: npmApproval.version, distTag: npmApproval.distTag },
      github: { status: githubApproval.status },
    },
    adapters: {
      javascript: javascriptAdapters.map(compactAdapter),
      native: nativeAdapters.map(compactAdapter),
    },
  };
}

function loadProductContractInputs(root) {
  const { CORE_MODES } = require(path.join(root, "src", "core-mode.js"));
  const { getAdapterRegistry } = require(path.join(root, "src", "adapter-registry.js"));
  return {
    packageJson: readJson(root, "package.json"),
    packagePolicy: readJson(root, "packaging/package-policy.json"),
    rolloutEvidence: readJson(root, "packaging/native-rollout-evidence.json"),
    npmApproval: readJson(root, "packaging/npm-publication-approval.json"),
    githubApproval: readJson(root, "packaging/github-release-approval.json"),
    cleanRoomEvidence: readJson(root, "packaging/evidence/clean-room-current.json"),
    coreModes: CORE_MODES,
    javascriptAdapters: getAdapterRegistry({ implementation: "javascript" }).adapters,
    nativeAdapters: getAdapterRegistry({ implementation: "native" }).adapters,
  };
}

function adapterSummary(adapters) {
  return adapters.map((adapter) => `${adapter.id} (${adapter.languages.join("/")}; ${adapter.availability}${adapter.requiredToolchain ? `; ${adapter.requiredToolchain}` : ""})`).join(", ");
}

function renderProductContractBlock(contract, document) {
  const heading = document === "ROADMAP.md" ? "###"
    : document === "ARCHITECTURE.md" ? "####"
    : "##";
  const common = [
    START,
    "",
    `${heading} Generated product contract`,
    "",
    `- Source candidate: \`${contract.package.name}@${contract.package.sourceVersion}\` on npm channel \`${contract.package.distTag}\`.`,
    `- Last verified preview artifact: \`${contract.package.name}@${contract.package.lastVerifiedPreview.version}\` (\`${contract.package.lastVerifiedPreview.status}\`).`,
    `- Runtime: Node.js ${contract.package.minimumNodeMajor} or later (\`${contract.package.enginesNode}\`).`,
    `- Public default core: \`${contract.core.publicDefaultMode}\` / ${contract.core.publicDefaultImplementation}.`,
    `- Experimental native core: \`${contract.core.experimentalMode}\`; rollout is \`${contract.core.nativeRolloutStatus}\` and native-default eligibility is \`${contract.core.nativeDefaultEligible}\`.`,
    `- Release approvals: npm \`${contract.release.npm.status}\`; GitHub Release \`${contract.release.github.status}\`.`,
  ];
  if (document === "native/flopeek-core/README.md") {
    common.push(
      `- JavaScript/default adapters: ${adapterSummary(contract.adapters.javascript)}.`,
      `- Native-experimental adapters: ${adapterSummary(contract.adapters.native)}.`,
    );
  }
  common.push("", "This block is generated from repository contracts; edit the source contracts and run `npm run generate:product-contract`.", "", END);
  return common.join("\n");
}

function replaceGeneratedBlock(source, block) {
  if (!source.includes(START) || !source.includes(END)) throw new Error("Document is missing generated product-contract markers.");
  return source.replace(new RegExp(`${START}[\\s\\S]*?${END}`), block);
}

function expectedDocuments(contract, documents) {
  return Object.fromEntries(Object.entries(documents).map(([name, source]) => [name, replaceGeneratedBlock(source, renderProductContractBlock(contract, name))]));
}

function assertGeneratedDocuments(contract, documents) {
  const expected = expectedDocuments(contract, documents);
  for (const [name, source] of Object.entries(documents)) {
    if (canonicalText(source) !== canonicalText(expected[name])) throw new Error(`${name} generated product contract is stale.`);
  }
  return true;
}

module.exports = {
  DOCUMENTS,
  END,
  PRODUCT_CONTRACT_SCHEMA,
  START,
  assertGeneratedDocuments,
  buildProductContractFromInputs,
  canonicalText,
  expectedDocuments,
  loadProductContractInputs,
  minimumNodeMajor,
  renderProductContractBlock,
  replaceGeneratedBlock,
};
