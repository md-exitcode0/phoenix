"""Inspect an existing saved scene without rendering, executing embedded code or saving."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

if len(sys.argv) != 3:
    raise SystemExit("Usage: check-saved-blender.py SCENE.blend NEW_OUTPUT_DIRECTORY")
scene = Path(sys.argv[1]).resolve(strict=True)
output = Path(sys.argv[2]).resolve()
output.mkdir(mode=0o700)
binary = shutil.which("blender")
if not binary:
    raise SystemExit("Blender executable was not found")
before = hashlib.file_digest(scene.open("rb"), "sha256").hexdigest()
command = [binary, "--background", "--factory-startup", "--disable-autoexec", str(scene),
           "--python-exit-code", "1", "--python", str(Path(__file__).with_name("inspect-blender-scene.py").resolve())]
with (output / "inspection.log").open("w") as log:
    run = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=120, check=False)
after = hashlib.file_digest(scene.open("rb"), "sha256").hexdigest()
evidence = None
for line in (output / "inspection.log").read_text().splitlines():
    if line.startswith("SCENE_EVIDENCE="):
        evidence = json.loads(line.removeprefix("SCENE_EVIDENCE="))
if evidence:
    (output / "scene.json").write_text(json.dumps(evidence, indent=2))
result = {"scene": str(scene), "exitCode": run.returncode, "sha256Before": before,
          "sha256After": after, "sourceUnchanged": before == after, "inspectionReceived": evidence is not None,
          "scope": "Saved-scene structural inspection only; no render, scene repair or visual-quality verdict."}
(output / "result.json").write_text(json.dumps(result, indent=2))
print(json.dumps(result))
if run.returncode or not evidence or before != after:
    raise SystemExit(1)
