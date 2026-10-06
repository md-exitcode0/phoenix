import { randomUUID } from "node:crypto";
import { lstatSync, mkdirSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { readWorkspaceFile } from "./workspace-files.js";
const MAX_ARTIFACT_BYTES = 2e6;
function artifactPath(workspacePath, name, create) {
  const root = realpathSync(workspacePath);
  const directory = path.join(root, ".taste");
  if (create) mkdirSync(directory, { recursive: true });
  if (!lstatSync(directory).isDirectory() || realpathSync(directory) !== directory) {
    throw new Error("Design artifact directory must be a real directory inside the workspace");
  }
  return path.join(directory, name);
}
function readDesignArtifact(workspacePath, name) {
  return JSON.parse(
    readWorkspaceFile(artifactPath(workspacePath, name, false), MAX_ARTIFACT_BYTES).toString(
      "utf8"
    )
  );
}
function writeDesignArtifact(workspacePath, name, value) {
  const content = `${JSON.stringify(value, null, 2)}
`;
  if (Buffer.byteLength(content) > MAX_ARTIFACT_BYTES)
    throw new Error("Design artifact exceeds 2 MB");
  const target = artifactPath(workspacePath, name, true);
  const temporary = path.join(path.dirname(target), `.${name}.${randomUUID()}.tmp`);
  try {
    writeFileSync(temporary, content, { encoding: "utf8", flag: "wx", mode: 384 });
    renameSync(temporary, target);
  } finally {
    rmSync(temporary, { force: true });
  }
}
export {
  readDesignArtifact,
  writeDesignArtifact
};
