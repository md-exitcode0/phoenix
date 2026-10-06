// Deterministic native-fixture teardown regressions. No browser, credentials,
// gateway, provider request, or user process is involved.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {stopOwnedProcessGroup} from './lib/acceptance-browser.mjs';

const [output]=process.argv.slice(2);
if(!output?.startsWith('/'))throw Error('NEW absolute evidence directory required');
await mkdir(output,{mode:0o700});
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms));
const wrapper=join(output,'wrapper.cjs'),worker=join(output,'worker.cjs');
await writeFile(wrapper,`
const {spawn}=require('node:child_process');
process.on('SIGTERM',()=>process.exit(0));
spawn(process.execPath,[process.argv[2],process.argv[3],process.argv[4]],{stdio:'ignore'}).unref();
setInterval(()=>{},1000);
`,{flag:'wx'});
await writeFile(worker,`
const fs=require('node:fs'),path=require('node:path');
const directory=process.argv[2],mode=process.argv[3];let count=0,stopping=false;
process.on('SIGTERM',()=>{
  if(mode==='ignore'||stopping)return;stopping=true;
  setTimeout(()=>{fs.writeFileSync(path.join(directory,'finished.json'),JSON.stringify({at:Date.now(),count}));process.exit(0);},240);
});
fs.writeFileSync(path.join(directory,'ready.json'),JSON.stringify({pid:process.pid}));
setInterval(()=>fs.writeFileSync(path.join(directory,'heartbeat.txt'),String(++count)),15);
`,{flag:'wx'});

const groups=[];
async function launch(name,mode){
  const directory=join(output,name);await mkdir(directory);
  const child=spawn(process.execPath,[wrapper,worker,directory,mode],{detached:true,stdio:'ignore'});
  const group={directory,child,exitAt:null};groups.push(group);
  group.exited=new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>{group.exitAt=Date.now();resolve({code,signal});});});
  group.exited.catch(()=>{});
  for(let attempt=0;attempt<150;attempt++){
    try{group.worker=JSON.parse(await readFile(join(directory,'ready.json'),'utf8')).pid;break;}catch(error){if(error.code!=='ENOENT'&&!(error instanceof SyntaxError))throw error;}
    await pause(20);
  }
  assert.ok(group.worker,'private descendant became ready');
  const stat=await readFile(`/proc/${group.worker}/stat`,'utf8');
  assert.equal(Number(stat.slice(stat.lastIndexOf(')')+2).trim().split(/\s+/)[2]),child.pid);
  await pause(40);return group;
}
async function quiet(group){
  const before=await readFile(join(group.directory,'heartbeat.txt'),'utf8');await pause(100);
  assert.equal(await readFile(join(group.directory,'heartbeat.txt'),'utf8'),before,'no descendant may keep writing after stop returns');
}
const tests=[];
try{
  const unaffected=await launch('independent-control','ignore');
  const graceful=await launch('delayed-descendant','graceful'),start=Date.now();
  await stopOwnedProcessGroup(graceful.child,graceful.exited,{graceMs:1200});
  const finished=JSON.parse(await readFile(join(graceful.directory,'finished.json'),'utf8'));
  assert.ok(finished.at>graceful.exitAt,'the wrapper really exited before its descendant');
  assert.ok(Date.now()-start>=200,'stop awaited the descendant, not merely the wrapper');
  await quiet(graceful);
  tests.push('wrapper exits first; delayed descendant is allowed to finish and cannot write after teardown');

  const stubborn=await launch('ignored-termination','ignore'),forcedStart=Date.now();
  await stopOwnedProcessGroup(stubborn.child,stubborn.exited,{graceMs:100});
  assert.ok(Date.now()-forcedStart<4000,'ignored graceful shutdown is bounded');
  await quiet(stubborn);
  tests.push('descendant ignoring SIGTERM is killed within the bounded teardown window');

  const before=Number(await readFile(join(unaffected.directory,'heartbeat.txt'),'utf8'));await pause(70);
  assert.ok(Number(await readFile(join(unaffected.directory,'heartbeat.txt'),'utf8'))>before);
  assert.equal(unaffected.child.exitCode,null);
  tests.push('another independently spawned process group remains alive and untouched');

  await stopOwnedProcessGroup(graceful.child,graceful.exited,{graceMs:100});
  await assert.rejects(stopOwnedProcessGroup({pid:process.pid},Promise.resolve()),/Invalid owned process group/);
  tests.push('already-exited group is harmless and the caller process cannot be selected');
}finally{
  for(const group of groups)await stopOwnedProcessGroup(group.child,group.exited,{graceMs:100});
}
await writeFile(join(output,'result.json'),JSON.stringify({passed:true,tests,providerCalls:0,browserStarts:0,scope:'Real isolated Node process groups, including delayed and SIGTERM-resistant descendants. Not a model task.'},null,2),{flag:'wx'});
console.log(JSON.stringify({passed:true,tests:tests.length}));
