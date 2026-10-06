// Real compiled Rust browser tools + newly loaded production Electron, no model.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,readFile,writeFile,open} from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {acceptanceBrowser} from './lib/acceptance-browser.mjs';
const [binary,output]=process.argv.slice(2);
if(![binary,output].every(path=>path?.startsWith('/')))throw Error('Absolute compiled test binary and NEW evidence directory required');
const testName='tools::browser_native::browser_native_tests::live_embedded_input_runtime_reaches_cold_reloaded_and_switched_targets';
const listed=execFileSync(binary,[testName,'--exact','--list'],{encoding:'utf8',timeout:10000});
assert.ok(listed.includes(testName+': test'),'the actual compiled fixture must exist before a browser starts');
await mkdir(output,{mode:0o700});
const home=await mkdtemp(join(tmpdir(),'phoenix-ui-acceptance-input-'));
const env={...process.env,PHOENIX_HOME:home,PHOENIX_EMBEDDED_INPUT_FIXTURE:'1',
  PHOENIX_BROWSER_LOGIN_SOURCE:'none',PHOENIX_BROWSER_PROFILE:'phoenix',PHOENIX_NO_LIBRARIAN:'1',
  XDG_DATA_HOME:join(home,'data'),XDG_CONFIG_HOME:join(home,'config'),XDG_CACHE_HOME:join(home,'cache'),TMPDIR:join(home,'tmp')};
for(const key of ['DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','DBUS_STARTER_ADDRESS','DBUS_STARTER_BUS_TYPE',
  'PHOENIX_BROWSER_ATTACH','PHOENIX_CHROMIUM_BRIDGE_URL','PHOENIX_CHROMIUM_BRIDGE_TOKEN'])delete env[key];
for(const key of ['XDG_DATA_HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','TMPDIR'])await mkdir(env[key],{recursive:true,mode:0o700});
const log=await open(join(output,'runtime.log'),'wx',0o600);let cleanup;
try {
  cleanup=await acceptanceBrowser({home,env,output});
  const child=spawn(binary,[testName,
    '--exact','--ignored','--nocapture','--test-threads=1'],{env,cwd:home,stdio:['ignore',log.fd,log.fd]});
  const deadline=setTimeout(()=>child.kill('SIGKILL'),90000);
  let result;
  try{result=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));});}
  finally{clearTimeout(deadline);}
  await writeFile(join(output,'execution.json'),JSON.stringify({home,binary,...result,modelCalls:0},null,2));
  assert.equal(result.code,0,'inspect runtime.log for the actual failed entrypoint');
  const receipt=JSON.parse(await readFile(join(home,'runtime-input-result.json'),'utf8'));
  assert.equal(receipt.passed,true);
  await writeFile(join(output,'result.json'),JSON.stringify(receipt,null,2));
  console.log('PASS: compiled Rust model-tool indexed clicks, reload, tab switch and real typing reached the correct newly loaded hidden Electron views. No provider calls.');
}finally{await cleanup?.();await log.close();}
