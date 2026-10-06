import { randomInt } from "node:crypto";
import { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { PAGE_LAYOUT_FAMILIES } from "./page.js";
import { array, member, record, string, strings } from "./parse.js";
import { readRasterMetadata } from "./raster-metadata.js";
import { generatedReferenceCandidates } from "./reference-library-index.js";
import { containedWorkspaceFile, readWorkspaceFile } from "./workspace-files.js";
function parseReferenceDeck(value) {
  if (!Array.isArray(value) || value.length > 64) throw new Error("Invalid Design reference deck");
  const ids = /* @__PURE__ */ new Set();
  return value.map((value2) => {
    const entry = record(value2, "Design reference");
    const id = string(entry.id, "reference id");
    if (!/^[a-z0-9][a-z0-9-]{0,95}$/u.test(id) || ids.has(id))
      throw new Error(`Invalid or duplicate Design reference id: ${id}`);
    ids.add(id);
    return {
      id,
      family: member(entry.family, PAGE_LAYOUT_FAMILIES, `${id}.family`),
      cue: string(entry.cue, `${id}.cue`),
      imagePath: string(entry.imagePath, `${id}.imagePath`),
      ...entry.mobileImagePath ? { mobileImagePath: string(entry.mobileImagePath, `${id}.mobileImagePath`) } : {},
      ...entry.source ? { source: string(entry.source, `${id}.source`) } : {},
      ...entry.group ? { group: string(entry.group, `${id}.group`) } : {},
      ...entry.tags ? { tags: strings(entry.tags, `${id}.tags`) } : {}
    };
  });
}
function referenceLibraryRoot() {
  if (process.env.TASTECODE_REFERENCE_LIBRARY)
    return path.resolve(process.env.TASTECODE_REFERENCE_LIBRARY);
  const configPath = path.join(os.homedir(), ".tastecode", "design-references.json");
  if (existsSync(configPath)) {
    const config = record(
      JSON.parse(readWorkspaceFile(configPath, 16384).toString()),
      "Design reference configuration"
    );
    const root = string(config.libraryPath, "libraryPath");
    if (!path.isAbsolute(root)) throw new Error("Design reference libraryPath must be absolute");
    return root;
  }
  const bundled = fileURLToPath(new URL("../references/library/", import.meta.url));
  const unpacked = bundled.replace(/\.asar([\\/])/u, ".asar.unpacked$1");
  return existsSync(unpacked) ? unpacked : bundled;
}
function loadReviewedReferences(root = referenceLibraryRoot()) {
  try {
    const catalog = record(
      JSON.parse(readWorkspaceFile(path.join(root, "catalog.json"), 2e6).toString()),
      "reference catalog"
    );
    if (catalog.version !== 1) throw new Error("catalog version must be 1");
    const listed = array(catalog.references, "catalog.references");
    const listedIds = new Set(listed.map((entry) => record(entry, "reference").id));
    const entries = [
      ...listed,
      ...generatedReferenceCandidates(root).filter((entry) => !listedIds.has(entry.id))
    ];
    if (entries.length > 1e4) throw new Error("catalog exceeds 10000 entries");
    const eligible = entries.filter((value) => {
      const entry = record(value, "reference");
      return (entry.reviewStatus === "reviewed" || entry.reviewStatus === "candidate") && ![entry.imagePath, entry.mobileImagePath].some(
        (file) => typeof file === "string" && /(?:^|[\\/])threshold-/iu.test(file)
      );
    });
    const references = eligible.flatMap((value) => {
      const entry = record(value, "reference");
      string(entry.reviewNotes, "reviewNotes");
      string(entry.group, "group");
      strings(entry.tags, "tags");
      const source = new URL(string(entry.source, "source"));
      if (!["https:", "http:"].includes(source.protocol))
        throw new Error("reference source must be HTTP(S)");
      if (entry.mobileImagePath) string(entry.pairEvidence, "pairEvidence");
      const [reference] = parseReferenceDeck([entry]);
      return [
        {
          ...reference,
          cue: `${reference.cue} Review: ${entry.reviewNotes}${entry.mobileImagePath ? ` Responsive pairing: ${entry.pairEvidence}` : " No verified mobile reference: derive and visually test the responsive layout from this desktop composition."}`
        }
      ];
    });
    if (!references.length)
      throw new Error("catalog has no reviewed or generated reference candidates");
    if (new Set(references.map(({ id }) => id)).size !== references.length)
      throw new Error("duplicate reference IDs");
    return references.map((entry) => ({
      ...entry,
      imagePath: containedWorkspaceFile(root, entry.imagePath, `reference ${entry.id}`),
      ...entry.mobileImagePath ? {
        mobileImagePath: containedWorkspaceFile(
          root,
          entry.mobileImagePath,
          `mobile reference ${entry.id}`
        )
      } : {}
    }));
  } catch (error) {
    throw new Error(
      `Design reference library could not be loaded: ${error instanceof Error ? error.message : String(error)}. Repair catalog.json or its files, then restart Design mode.`
    );
  }
}
function referenceCandidatesForFamily(family, references) {
  const native = references.filter((entry) => entry.family === family);
  if (new Set(native.map((entry) => entry.group ?? entry.id)).size >= 10) return native;
  const compatible = {
    stats: ["social_proof", "about"],
    pricing: ["feature"],
    how_it_works: ["feature", "about"],
    contact: ["cta"],
    cta: ["contact"],
    faq: ["feature"]
  };
  const alternatives = compatible[family] ?? [];
  const pool = references.filter(
    (entry) => entry.family === family || alternatives.some((candidate) => candidate === entry.family)
  );
  return new Set(pool.map((entry) => entry.group ?? entry.id)).size >= 10 ? pool : native;
}
function selectReviewedReferences(brief, references = loadReviewedReferences(), chooseIndex = randomInt) {
  const request = `${brief.originalRequest} ${(brief.explicitAnswers ?? []).map((answer) => answer.answer).join(" ")}`.toLowerCase();
  if (!references.length) throw new Error("No Design references are available");
  const selected = [];
  const groups = /* @__PURE__ */ new Set();
  for (const family of PAGE_LAYOUT_FAMILIES) {
    const familyEntries = referenceCandidatesForFamily(family, references).filter(
      (entry) => !groups.has(entry.group ?? entry.id)
    );
    if (!familyEntries.length) continue;
    const explicit = familyEntries.filter((entry) => request.includes(entry.id.toLowerCase()));
    const explicitSource = familyEntries.filter(
      (entry) => entry.source && request.includes(entry.source.toLowerCase())
    );
    const pool = explicit.length ? explicit : explicitSource.length ? explicitSource : familyEntries;
    const groupNames = [...new Set(pool.map((entry) => entry.group ?? entry.id))];
    const count = Math.min(groupNames.length, family === "feature" ? 3 : family === "about" ? 2 : 1);
    for (let index = 0; index < count; index++) {
      const [chosenGroup] = groupNames.splice(chooseIndex(groupNames.length), 1);
      const revisions = pool.filter((entry2) => (entry2.group ?? entry2.id) === chosenGroup);
      const entry = revisions[chooseIndex(revisions.length)];
      const group = entry.group ?? entry.id;
      if (groups.has(group)) continue;
      groups.add(group);
      selected.push(
        entry.family === family ? entry : {
          ...entry,
          cue: `${entry.cue} Sampled for ${family} content from compatible compositions; retain its actual ${entry.family} layout family and image geometry.`
        }
      );
    }
  }
  if (!selected.length)
    throw new Error(
      "No references match the requested sections. Add matching catalog entries or attach your own reference images."
    );
  if (selected.length > 24)
    throw new Error(
      "Selected Design reference collection exceeds 24 sections; narrow the catalog collection"
    );
  for (const entry of selected) {
    readRasterMetadata(entry.imagePath);
    if (entry.mobileImagePath) readRasterMetadata(entry.mobileImagePath);
  }
  return selected;
}
export {
  loadReviewedReferences,
  parseReferenceDeck,
  referenceCandidatesForFamily,
  referenceLibraryRoot,
  selectReviewedReferences
};
