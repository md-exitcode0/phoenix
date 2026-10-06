#!/usr/bin/env python3
"""Run actual tool + Canvas VITALS file writers without linking the native app.

Requires an existing compatible rustc dependency map: a JSON object whose
`externs` maps dependency names to prebuilt .rlib/.so paths. Installs nothing.
The generated crate extracts Canvas functions verbatim, omitting only Tauri
command annotations and its async list wrapper. Test stores and compiler
outputs are confined to a new temporary directory under --output-dir.
This verifies file mutation logic, not Tauri/Chromium IPC or native rendering.
"""
import argparse
import json
import os
from pathlib import Path
import re
import resource
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--dependency-map", type=Path, required=True)
parser.add_argument("--output-dir", type=Path, required=True)
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
args.output_dir.mkdir(parents=True, exist_ok=True)
work = Path(tempfile.mkdtemp(prefix="vital-writers-", dir=args.output_dir.resolve()))
home = work / "private-home"
home.mkdir(mode=0o700)
source = (repo / "canvas-app/src/main.rs").read_text()
private = source[source.index("pub(crate) fn phoenix_home()"):source.index("fn read_private_text_required(")]
vitals = source[source.index("const VITALS_SECTIONS:"):source.index("/* ── Memory (CLI bridge")]
vitals, count = re.subn(r"#\[tauri::command\]\nasync fn vitals_list\(\).*?(?=#\[tauri::command\]\nfn vitals_write)", "", vitals, flags=re.S)
assert count == 1, "Canvas list wrapper changed; inspect extraction before running"
vitals = vitals.replace("#[tauri::command]\n", "")
quote = lambda path: json.dumps(str(path))
crate = f'''#![allow(dead_code)]
#[path={quote(repo / "src/vital_memory_document.rs")}] mod vital_memory_document;
mod config {{
    pub fn phoenix_home() -> std::path::PathBuf {{
        std::env::var_os("PHOENIX_HOME").expect("isolated test home").into()
    }}
    pub fn phoenix_vitals_path() -> std::path::PathBuf {{ phoenix_home().join("VITALS.md") }}
    #[path={quote(repo / "src/config/private_io.rs")}] pub mod private_io;
}}
mod tools {{
    pub struct ToolOutput {{ pub summary: String, pub content: String }}
    #[path={quote(repo / "src/tools/vital_memory.rs")}] pub mod vital_memory;
}}
mod canvas {{
    use std::path::PathBuf;
    use crate::vital_memory_document;
    const VITALS_MAX_BYTES: usize = vital_memory_document::MAX_FILE_BYTES;
    {private}
    {vitals}
    include!({quote(repo / "scripts/fixtures/vital-memory-writers.rs")});
}}
'''
rust = work / "writers.rs"
rust.write_text(crate)
externs = json.loads(args.dependency_map.read_text())["externs"]
required = ["anyhow", "serde", "serde_json", "libc", "chrono", "tracing", "directories", "tempfile"]
command = ["rustc", "--edition=2021", "--test", str(rust), "-C", "debuginfo=0", "-C", "strip=debuginfo", "-C", "link-arg=-Wl,--threads=2", "-o", str(work / "writers-tests")]
for name in required:
    command += ["--extern", name + "=" + externs[name]]
command += ["-L", "dependency=" + str(Path(externs["serde"]).parent)]

def bounded():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_CPU, (120, 120))
    resource.setrlimit(resource.RLIMIT_AS, (2 * 1024**3, 2 * 1024**3))
    resource.setrlimit(resource.RLIMIT_FSIZE, (128 * 1024**2, 128 * 1024**2))

subprocess.run(command, check=True, timeout=150, preexec_fn=bounded)
env = os.environ.copy()
env["PHOENIX_HOME"] = str(home)
result = subprocess.run([str(work / "writers-tests"), "--test-threads=1", "--nocapture"], env=env, text=True, capture_output=True, timeout=90, preexec_fn=bounded)
(args.output_dir / "vital-writers-tests.log").write_text(result.stdout + result.stderr)
(args.output_dir / "vital-writers-proof.json").write_text(json.dumps({"command": command, "exit_code": result.returncode, "test_home": str(home), "scope": "Actual tool and Canvas file functions; no native app/IPC/model calls."}, indent=2))
print(result.stdout + result.stderr)
raise SystemExit(result.returncode)
