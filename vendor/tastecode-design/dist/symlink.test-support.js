import { mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
function canCreateSymlinks() {
  const root = mkdtempSync(path.join(tmpdir(), "harness-symlink-probe-"));
  try {
    const target = path.join(root, "target");
    writeFileSync(target, "");
    symlinkSync(target, path.join(root, "link"));
    return true;
  } catch (error) {
    if (error instanceof Error && "code" in error && ["EPERM", "EACCES", "ENOSYS", "ENOTSUP"].includes(String(error.code)))
      return false;
    throw error;
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}
export {
  canCreateSymlinks
};
