import { createHash } from "node:crypto";
import { containedWorkspaceFile, readWorkspaceFile } from "./workspace-files.js";
function snapshotDesignFiles(files) {
  if (files.length > 256) throw new Error("Design file snapshot exceeds 256 files");
  let totalBytes = 0;
  return [...new Set(files)].map((file) => {
    const bytes = readWorkspaceFile(file, 32e6);
    totalBytes += bytes.length;
    if (totalBytes > 128e6) throw new Error("Design file snapshot exceeds 128 MB");
    if (!bytes.length) throw new Error("Design reference and asset files must not be empty");
    return { path: file, sha256: createHash("sha256").update(bytes).digest("hex") };
  });
}
function validateDesignFileSnapshot(snapshot) {
  const current = snapshotDesignFiles(snapshot.map(({ path }) => path));
  if (current.length !== snapshot.length || current.some((file, index) => file.sha256 !== snapshot[index].sha256)) {
    throw new Error(
      "A Design reference or approved asset changed after approval; restore the original file or restart Design mode"
    );
  }
}
function snapshotDesignAssets(workspacePath, manifest) {
  const files = manifest.assets.flatMap((asset) => {
    if (asset.status === "needed" || asset.kind === "component") return [];
    const relative = asset.destination ?? (asset.source?.kind === "project" ? asset.source.reference : void 0);
    return relative ? [containedWorkspaceFile(workspacePath, relative, `asset ${asset.id}`)] : [];
  });
  return snapshotDesignFiles(files);
}
export {
  snapshotDesignAssets,
  snapshotDesignFiles,
  validateDesignFileSnapshot
};
