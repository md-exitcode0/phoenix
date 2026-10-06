#!/usr/bin/env python3
"""Execute current voice sources in an existing offline Rust test fixture.
Not a default-feature or native build. No model/provider requests are run.
All compiler output and generated private homes stay in the requested /tmp dir.
"""
import argparse,hashlib,json,os,resource,shutil,subprocess,time
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--fixture',type=Path,required=True);p.add_argument('--dependency-map',type=Path,required=True);p.add_argument('--output-dir',type=Path,required=True);a=p.parse_args()
repo=Path(__file__).resolve().parents[1];out=a.output_dir.resolve()
if not str(out).startswith('/tmp/'):raise SystemExit('Compiler outputs must be in /tmp')
out.mkdir(parents=True,exist_ok=True);snapshot=out/'fixture';shutil.copytree(a.fixture,snapshot,dirs_exist_ok=True)
files=['src/runtime/prompt.rs','src/runtime/shared_contract.rs','src/sub_agents/framework.rs','src/sub_agents/mod.rs','src/orchestrator/mod.rs','src/vital_memory_document.rs']
hashes={}
for f in files:
 data=subprocess.check_output(['git','show','HEAD:'+f],cwd=repo) if not (repo/f).is_file() else (repo/f).read_bytes()
 (snapshot/f).write_bytes(data);hashes[f]=hashlib.sha256(data).hexdigest()
manifest=json.loads(a.dependency_map.read_text());deps=manifest['externs'];depdir=Path(deps['tokio']).parent
command=['rustc','--crate-name','phoenix_agent','--edition=2021','--test',str(snapshot/'src/lib.rs'),'-C','debuginfo=0','-C','strip=debuginfo','-C','codegen-units=16','-C','link-arg=-fuse-ld=lld','-o',str(out/'prompt-tests'),'-L','dependency='+str(depdir)]
for f in depdir.parent.glob('build/*/out'):command+=['-L','native='+str(f)]
for name,path in deps.items():command+=['--extern',name+'='+path]
env=os.environ.copy();env.update(CARGO_MANIFEST_DIR=str(snapshot),CARGO_PKG_VERSION='0.1.0',CARGO_PKG_NAME='phoenix_agent')
def bounds():
 resource.setrlimit(resource.RLIMIT_CORE,(0,0));resource.setrlimit(resource.RLIMIT_AS,(4*1024**3,4*1024**3));resource.setrlimit(resource.RLIMIT_CPU,(120,120));resource.setrlimit(resource.RLIMIT_FSIZE,(192*1024**2,192*1024**2));os.setsid()
with (out/'compile.log').open('w') as log:
 process=subprocess.Popen(command,env=env,stdout=log,stderr=log,preexec_fn=bounds);start=time.monotonic();reason=None
 while process.poll() is None:
  available=int(next(l for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')).split()[1])*1024
  rss=int(next((l for l in Path(f'/proc/{process.pid}/status').read_text().splitlines() if l.startswith('VmRSS:')),'VmRSS: 0 kB').split()[1])*1024
  if available<900*1024**2 or rss>2300*1024**2 or time.monotonic()-start>120:
   reason={'available_ram':available,'compiler_rss':rss,'elapsed':time.monotonic()-start};os.killpg(process.pid,15);break
  time.sleep(.25)
 code=process.wait()
 if code or reason:
  (out/'results.json').write_text(json.dumps({'compile_exit':code,'bound_reason':reason,'source_hashes':hashes},indent=2));print((out/'compile.log').read_text()[-4500:]);raise SystemExit(code or 1)
env['PHOENIX_PROMPT_REVIEW_DIR']=str(out/'assembled')
r=subprocess.run([str(out/'prompt-tests'),'runtime::prompt::tests','--test-threads=1'],env=env,capture_output=True,text=True,timeout=45)
(out/'tests.log').write_text(r.stdout+r.stderr);(out/'results.json').write_text(json.dumps({'exit_code':r.returncode,'source_hashes':hashes,'scope':'Current production voice modules executed in existing offline fixture; Cognee/default features omitted. No native IPC or model evaluation.'},indent=2));print(r.stdout+r.stderr);raise SystemExit(r.returncode)
