// Two real gateway process starts against a completed isolated fixture.
// No credentials copied and no model turn submitted.
import {readFile,writeFile,open,access} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {spawn} from 'node:child_process';
import net from 'node:net';
import {createHash} from 'node:crypto';
const [binary,output]=process.argv.slice(2,4).map(resolvePath=>resolve(resolvePath));
const label=process.argv[4]||'restart';
if(!/^[a-z0-9-]+$/.test(label))throw Error('Invalid evidence label');
const execution=JSON.parse(await readFile(join(output,'execution.json'),'utf8'));
const ownerReturnFixture=!!execution.gateway;
if((ownerReturnFixture?execution.gateway.code:execution.code)!==0||!/^\/tmp\/phoenix-(?:group-overlap|ui-acceptance)-[A-Za-z0-9]+$/.test(execution.home))throw Error('Requires a successful isolated fixture');
if(ownerReturnFixture){
  const result=JSON.parse(await readFile(join(output,'result.json'),'utf8'));
  if(!execution.credentialRemoved||!result.checks||!Object.values(result.checks).every(Boolean))throw Error('Owner-return fixture did not pass or clean up');
}
const home=execution.home;
await access(join(home,'auth-profiles.json')).then(()=>{throw Error('Fixture must have no copied credentials');},e=>{if(e.code!=='ENOENT')throw e;});
const submission=ownerReturnFixture?JSON.parse(await readFile(join(output,'submission.json'),'utf8'))
  :(await readFile(join(output,'experiment/gateway-events.jsonl'),'utf8')).trim().split('\n').map(JSON.parse).find(x=>x.phase==='submission').value;
const session_id=ownerReturnFixture?submission.group.canonical_session_id:submission.session_id;
const sessionPath=join(home,'sessions',session_id+'.json');
const before=await readFile(sessionPath);
const digest=bytes=>createHash('sha256').update(bytes).digest('hex');
const logPath=join(output,label+'-process.log');
const log=await open(logPath,'wx',0o600);
async function request(body,barrier=false){
  return new Promise((resolve,reject)=>{
    const socket=net.createConnection(join(home,'gateway.sock'));let buffer='',rows=[];
    socket.setTimeout(10000);
    socket.on('connect',()=>socket.write(JSON.stringify(body)+'\n'));
    socket.on('error',reject);
    socket.on('timeout',()=>{socket.destroy();reject(Error('Gateway observation timed out'));});
    socket.on('data',chunk=>{buffer+=chunk;let i;
      while((i=buffer.indexOf('\n'))>=0){
        let row;try{row=JSON.parse(buffer.slice(0,i));}catch(e){socket.destroy();reject(e);return;}
        buffer=buffer.slice(i+1);rows.push(row);
        if(row.Error){socket.destroy();reject(Error(row.Error.message));return;}
        if(!barrier||row==='Pong'){socket.destroy();resolve(rows);return;}
      }
    });
  });
}
// An existing live socket belongs to an existing observer. Never start or
// stop another gateway over it, even when it points at a fixture directory.
let live=false;try{await request('Ping');live=true;}catch(e){if(!['ENOENT','ECONNREFUSED'].includes(e.code))throw e;}
if(live)throw Error('Fixture gateway already live; inspect it rather than relaunching');
const results=[];
const env={...process.env,PHOENIX_HOME:home,PHOENIX_NO_LIBRARIAN:'1',PHOENIX_WS_PORT:'0',
  XDG_DATA_HOME:join(home,'data'),XDG_CONFIG_HOME:join(home,'config'),XDG_CACHE_HOME:join(home,'cache')};
for(const key of ['DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','DBUS_STARTER_ADDRESS','DBUS_STARTER_BUS_TYPE','XDG_ACTIVATION_TOKEN'])delete env[key];
try{
  for(let cycle=1;cycle<=2;cycle++){
    const child=spawn(binary,[],{cwd:output,env,stdio:['ignore','pipe','pipe']});
    const done=new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));});
    const capture=async stream=>{for await(const bytes of stream)await log.write(bytes);};
    const captures=[capture(child.stdout),capture(child.stderr)];
    try{
      let ready=false;
      for(let i=0;i<100;i++){
        if(child.exitCode!==null||child.signalCode!==null)throw Error('Gateway exited before readiness');
        try{await request('Ping');ready=true;break;}catch(e){if(!['ENOENT','ECONNREFUSED'].includes(e.code))throw e;}
        await new Promise(resolve=>setTimeout(resolve,100));
      }
      if(!ready)throw Error('No gateway readiness');
      const queue=(await request({QueuedTurns:{session_id}}))[0].QueuedTurns;
      const asks=(await request({ConversationAsks:{session_id}}))[0].ConversationAsks;
      const snapshot=(await request({CompanySnapshot:{session_id}}))[0].CompanySnapshot;
      const replays=[];
      for(let reconnect=0;reconnect<2;reconnect++)replays.push(await request({SubscribeJournal:{session_id}},true));
      const after=await readFile(sessionPath);
      const checks={queueEmpty:Array.isArray(queue)&&queue.length===0,
        authoritativeRuntimeIdle:!!snapshot&&snapshot.active_jobs===0,
        asksSettled:Array.isArray(asks)&&asks.length===(ownerReturnFixture?0:1)&&asks.every(a=>a.status!=='pending'),
        transcriptUnchanged:before.equals(after),
        replayHasNoWorking:replays.every(rows=>!rows.some(r=>r.Story?.kind==='group_member_status'&&r.Story.state==='working')),
        replayIdsUnique:replays.every(rows=>{const ids=rows.flatMap(r=>r.Story?.kind==='group_message'?[r.Story.message_id]:[]);return new Set(ids).size===ids.length;})};
      results.push({cycle,checks,transcriptSha256:digest(after),replays,
        snapshot:{active_jobs:snapshot?.active_jobs,stale_jobs:snapshot?.stale_jobs},
        note:'Completed journal replay may contain only Pong; canonical file equality is the transcript evidence, not the empty event replay.'});
      if(!Object.values(checks).every(Boolean))throw Error('Restart checks failed: '+JSON.stringify(checks));
    }finally{
      if(child.exitCode===null&&child.signalCode===null)child.kill('SIGTERM');
      const exited=await done;await Promise.all(captures);
      if(exited.code!==0)throw Error('Owned restart gateway did not exit cleanly: '+JSON.stringify(exited));
    }
  }
}finally{
  await log.close();
  const logText=await readFile(logPath,'utf8');
  const recoveryErrors=logText.split('\n').filter(line=>/restart recovery failed|enqueue recovery failed/.test(line));
  await writeFile(join(output,label+'-evidence.json'),JSON.stringify({results,recoveryErrors,canonicalSha256:digest(before)},null,2),{flag:'wx',mode:0o600});
  if(recoveryErrors.length)throw Error('Gateway startup recovery failed: '+recoveryErrors.join('\n'));
}
console.log(JSON.stringify({restartCycles:results.length,checks:results.map(r=>r.checks)}));
