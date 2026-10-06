// Run exactly one group experiment on an owned foreground gateway. Never
// connect this wrapper to the user's gateway or copy histories/refresh tokens.
import {mkdtemp,mkdir,readFile,writeFile,unlink,open} from 'node:fs/promises';
import {join,resolve,dirname} from 'node:path';
import {homedir,tmpdir} from 'node:os';
import {fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';
import net from 'node:net';
const [binary,source,output]=process.argv.slice(2,5).map(p=>p&&resolve(p));
const mode=process.argv[5]||'overlap';
if(!['overlap','named','peer-wait','atomic-plan'].includes(mode))throw Error('Unknown experiment mode');
if(!binary||!source||!output)throw Error('binary, read-only source and NEW output required');
await mkdir(output,{mode:0o700});
const home=await mkdtemp(join(tmpdir(),'phoenix-group-overlap-'));
const authPath=join(home,'auth-profiles.json');
const auth=JSON.parse(await readFile(join(homedir(),'.phoenix','auth-profiles.json'),'utf8'));
const saved=Object.values(auth.profiles||{}).find(p=>p.provider==='openai-codex'&&(p.access||p.token)&&p.expires>Date.now()+15*60*1000);
if(!saved)throw Error('No current Codex access token with fifteen minutes remaining; no request submitted');
const env={...process.env,PHOENIX_HOME:home,PHOENIX_NO_LIBRARIAN:'1'};
let gateway,child,gatewayDone,childDone;
const log=await open(join(output,'process.log'),'wx',0o600);
const capture=async s=>{for await(const bytes of s)await log.write(bytes);};
const observe=p=>new Promise((resolve,reject)=>{p.once('error',reject);p.once('exit',(code,signal)=>resolve({code,signal}));});
const ping=()=>new Promise(resolve=>{
  const socket=net.createConnection(join(home,'gateway.sock'));
  socket.setTimeout(1000); let text='';
  socket.on('connect',()=>socket.write(JSON.stringify('Ping')+'\n'));
  socket.on('data',chunk=>{text+=chunk;if(text.includes('\n')){socket.destroy();resolve(text.includes('Pong'));}});
  socket.on('error',()=>resolve(false));socket.on('timeout',()=>{socket.destroy();resolve(false);});
});
try{
  await writeFile(authPath,JSON.stringify({version:1,profiles:{probe:{type:'token',provider:'openai-codex',token:saved.access||saved.token,expires:saved.expires}}}),{mode:0o600,flag:'wx'});
  await writeFile(join(home,'config.toml'),'[profile]\nname="isolated-group-overlap"\n[profile.llm]\nprovider="openai-codex"\nmodel="gpt-5.6-sol"\nspecialist_model="gpt-5.6-sol"\nlibrarian_model="gpt-5.6-sol"\nreasoning_effort="medium"\n[profile.llm.auth]\nsource="profile"\nprofile="probe"\n',{mode:0o600,flag:'wx'});
  await writeFile(join(home,'settings.json'),JSON.stringify({version:1,revision:1,updated_at:new Date().toISOString(),migrated_canvas_preferences:true,global:{'memory.enabled':false},agents:{},groups:{}}),{mode:0o600,flag:'wx'});
  console.log(JSON.stringify({phase:'starting',home,output}));
  gateway=spawn(binary,[],{cwd:source,env,stdio:['ignore','pipe','pipe']});gatewayDone=observe(gateway);
  const streams=[capture(gateway.stdout),capture(gateway.stderr)];
  let ready=false;
  for(let attempt=0;attempt<100;attempt++){
    if(gateway.exitCode!==null||gateway.signalCode!==null)throw Error('Isolated gateway exited before readiness');
    if(await ping()){ready=true;break;}
    await new Promise(resolve=>setTimeout(resolve,100));
  }
  if(!ready)throw Error('Isolated gateway did not become ready; no group submitted');
  await new Promise((resolve,reject)=>{
    const socket=net.createConnection(join(home,'gateway.sock'));let buffer='';
    socket.setTimeout(10000);
    socket.on('connect',()=>socket.write(JSON.stringify({Onboarding:{action:'choose_company',choice:'founding_company'}})+'\n'));
    socket.on('data',chunk=>{buffer+=chunk;if(buffer.includes('\n')){
      socket.destroy();try{const value=JSON.parse(buffer.slice(0,buffer.indexOf('\n')));if(value.Error)reject(Error(value.Error.message));else resolve();}catch(error){reject(error);}
    }});
    socket.on('error',reject);socket.on('timeout',()=>{socket.destroy();reject(Error('Fixture roster setup timed out; no group submitted'));});
  });
  const probeArgs=mode==='overlap'
    ? ['live-group-question.mjs',join(output,'experiment'),source,'-','early']
    : ['live-named-workflow.mjs',join(output,'experiment'),source,mode];
  probeArgs[0]=join(dirname(fileURLToPath(import.meta.url)),probeArgs[0]);
  child=spawn(process.execPath,probeArgs,{env,stdio:['ignore','pipe','pipe']});childDone=observe(child);
  const result=await Promise.all([childDone,capture(child.stdout),capture(child.stderr)]);
  await writeFile(join(output,'execution.json'),JSON.stringify({...result[0],home,output},null,2),{flag:'wx',mode:0o600});
  console.log(JSON.stringify({phase:'experiment-terminal',...result[0],home,output}));
  if(result[0].code!==0)process.exitCode=1;
  gateway.kill('SIGTERM');await gatewayDone;await Promise.all(streams);
}finally{
  if(child&&child.exitCode===null&&child.signalCode===null){child.kill('SIGTERM');await childDone;}
  if(gateway&&gateway.exitCode===null&&gateway.signalCode===null){gateway.kill('SIGTERM');await gatewayDone;}
  await unlink(authPath).catch(error=>{if(error.code!=='ENOENT')throw error;});
  await log.close();
}
