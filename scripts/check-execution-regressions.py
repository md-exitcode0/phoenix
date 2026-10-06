#!/usr/bin/env python3
"""Compile real bash/queue modules with existing dependencies; never write live caches.
Bash settings, desktop and remote services are inert in this focused harness. Path
resolution and cancellation are copied verbatim from tools/mod.rs, with source
hashes recorded. This is narrower than a native/default-feature integration test.
"""
import hashlib,json,os,resource,subprocess,tempfile,time
from pathlib import Path
root=Path(__file__).resolve().parents[1]
manifest=Path(os.environ.get('PHOENIX_RUST_DEPENDENCIES',root.parent/'verification/rust/dependency-reuse.json'))
deps=json.loads(manifest.read_text())['externs']
support=(root/'src/tools/mod.rs').read_text()
def section(start,end):return support[support.index(start):support.index(end,support.index(start))]
path_fn=section('fn resolve_workspace_path(', '\nfn relative_display(')
cancel=section('#[derive(Clone, Debug, Default)]\npub struct ToolCancellation', '\n/// A bounded tool call')
source='''#![allow(dead_code)]
mod settings {pub enum SettingsScope {Global} pub fn effective_string(_: &str,_: &SettingsScope)->Option<String>{Some("off".into())}}
mod config {pub fn test_isolated_from_live_home()->bool{true} pub fn phoenix_home()->std::path::PathBuf{panic!("live home unavailable in test")}
 pub mod private_io {pub fn prepare_phoenix_directory(_: &std::path::Path)->anyhow::Result<()>{panic!("live cache unavailable in test")}}}
mod tools {use std::path::{Path,PathBuf}; use anyhow::{Result,Context,bail};
 #[derive(Debug)] pub struct ToolOutput {pub summary:String,pub content:String}
'''+cancel+path_fn+'''
 pub mod isolated_desktop {pub struct Environment; impl Environment {pub fn apply_to_command(&self,_:&mut std::process::Command){}}
 pub fn current_environment()->anyhow::Result<Option<Environment>>{Ok(None)}}
 pub mod remote_runner {pub struct Runner {pub label:String,pub id:String} pub fn find_enabled(_: &str)->anyhow::Result<Runner>{anyhow::bail!("No remote service in focused test")}
 pub fn ssh_command(_: &Runner,_:&std::path::Path,_:&str)->anyhow::Result<std::process::Command>{anyhow::bail!("No remote service in focused test")}}
 #[path="'''+str(root/'src/tools/bash.rs')+'''"] pub mod bash;
}
#[path="'''+str(root/'src/cli/turn_events.rs')+'''"] mod turn_events;
'''
out=Path(os.environ.get('PHOENIX_EXECUTION_EVIDENCE',root.parent/'verification/execution-regressions-20261005'));out.mkdir(parents=True,exist_ok=True)
with tempfile.TemporaryDirectory(prefix='phoenix-focused-execution-',dir='/tmp') as temp:
 temp=Path(temp);(temp/'harness.rs').write_text(source)
 names=['anyhow','serde','tempfile','libc','tokio'];command=['rustc','--test','--edition=2021',str(temp/'harness.rs'),'-o',str(temp/'checks'),'-C','debuginfo=0','-C','strip=debuginfo','-C','link-arg=-fuse-ld=lld','-L','dependency='+str(Path(deps['tokio']).parent)]
 for name in names:command+=['--extern',name+'='+deps[name]]
 def bounds():
  resource.setrlimit(resource.RLIMIT_CORE,(0,0));resource.setrlimit(resource.RLIMIT_AS,(3*1024**3,3*1024**3));resource.setrlimit(resource.RLIMIT_CPU,(120,120));resource.setrlimit(resource.RLIMIT_FSIZE,(64*1024**2,64*1024**2));os.setsid()
 with (out/'compile.log').open('w') as log:
  p=subprocess.Popen(command,stdout=log,stderr=log,preexec_fn=bounds);begin=time.monotonic()
  while p.poll() is None:
   if time.monotonic()-begin>120 or int(next(l for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')).split()[1])<900000:
    os.killpg(p.pid,15);raise SystemExit('Stopped at compiler time/RAM bound')
   time.sleep(.2)
  if p.returncode:print((out/'compile.log').read_text());raise SystemExit(p.returncode)
 results=[]
 for match in ['full_access','workspace_rejects_outside','turn_events::tests']:
  r=subprocess.run([str(temp/'checks'),match,'--nocapture'],capture_output=True,text=True,timeout=20);results.append({'filter':match,'exit_code':r.returncode,'output':r.stdout+r.stderr});print(r.stdout+r.stderr)
 (out/'results.json').write_text(json.dumps({'scope':'Compiled real production bash and queue-consumer helper, inert settings/desktop/remote adapters; not a full native build','hashes':{str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest() for p in [root/'src/tools/bash.rs',root/'src/tools/mod.rs',root/'src/cli/turn_events.rs']},'results':results},indent=2)+'\n')
 if any(r['exit_code'] for r in results):raise SystemExit(1)
