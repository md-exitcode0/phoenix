// One real Phoenix CLI task, without the user's gateway, schedules or history.
import {mkdtemp, mkdir, readFile, writeFile, unlink, open} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {tmpdir, homedir} from 'node:os';
import {spawn} from 'node:child_process';
const [binaryArg, blenderArg, outputArg] = process.argv.slice(2);
if (![binaryArg, blenderArg, outputArg].every(p=>p?.startsWith('/'))) throw Error('absolute binary, Blender and new output directory required');
const binary=resolve(binaryArg),blender=resolve(blenderArg),output=resolve(outputArg);
await mkdir(output); // Refuse to overwrite an existing acceptance workspace.
const evidence=output+'.evidence'; await mkdir(evidence,{mode:0o700});
const isolatedHome=await mkdtemp(join(tmpdir(),'phoenix-native-task-'));
const authPath=join(isolatedHome,'auth-profiles.json');
const store=JSON.parse(await readFile(join(homedir(),'.phoenix','auth-profiles.json'),'utf8'));
const profile=Object.keys(store.profiles||{}).sort().find(id=>store.profiles[id].provider==='openai-codex');
if(!profile)throw Error('No connected Codex account');
const saved=store.profiles[profile],token=saved.access||saved.token;
if(!token||!saved.expires||saved.expires<=Date.now()+10*60*1000)throw Error('A current Codex access token with at least ten minutes remaining is required');
// Copy only the short-lived access token, never the refresh token or other accounts.
await writeFile(authPath,JSON.stringify({version:1,profiles:{probe:{type:'token',provider:'openai-codex',token,expires:saved.expires}}}),{mode:0o600,flag:'wx'});
await writeFile(join(isolatedHome,'config.toml'),'[profile]\nname="isolated-native-acceptance"\n[profile.llm]\nprovider="openai-codex"\nmodel="gpt-6-astra"\nspecialist_model="gpt-6-astra"\nlibrarian_model="gpt-6-astra"\nnative_vision=true\nreasoning_effort="high"\ncontext_window=1050000\n[profile.llm.auth]\nsource="profile"\nprofile="probe"\n',{mode:0o600,flag:'wx'});
await writeFile(join(isolatedHome,'settings.json'),JSON.stringify({version:1,revision:1,updated_at:new Date().toISOString(),migrated_canvas_preferences:true,global:{'memory.enabled':false},agents:{},groups:{}}),{mode:0o600,flag:'wx'});
const prompt=`Real-world offline Blender acceptance task. Work only in ${output}. Do not read credentials, browse websites, contact external services, install software, or change unrelated files. Blender is at ${blender}. Build a polished desktop headphone stand: rounded rectangular base 140 mm wide by 110 mm deep by 12 mm thick; rounded upright reaching 240 mm total height; curved padded saddle 75 mm wide; recessed cable-management channel. Millimeter units, named components, plausible connections and no floating geometry. Graphite powder-coated metal, warm cork padding and restrained brass detail. Deliver headphone-stand.blend, a 1200x1000 studio product render preview.png, and verification.json with measured geometry and framing. Use scripting for precise repeatable modeling if useful. Reopen the saved blend in a fresh Blender process. Measure every product mesh vertex projected through the camera, excluding only the studio/background; require the whole product inside a 5% image margin and in front of the camera. Inspect the rendered image using your image tools and correct defects. Distinguish measured checks from visual judgment. Do not inspect observer logs, count calls, or invent metrics; the harness measures those externally. Execute and verify the task, not a plan.`;
await writeFile(join(evidence,'request.txt'),prompt,{flag:'wx'});
const log=await open(join(evidence,'cli.log'),'wx',0o600),started=Date.now();
console.log(JSON.stringify({phase:'starting',isolatedHome,output,evidence}));
let exitCode;
try{
  const child=spawn(binary,['start','--session','isolated-native-astra','--yolo',prompt],{cwd:output,env:{...process.env,PHOENIX_HOME:isolatedHome},stdio:['ignore','pipe','pipe']});
  const capture=async stream=>{for await(const chunk of stream)await log.write(chunk);};
  const done=new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));});
  const result=await Promise.all([done,capture(child.stdout),capture(child.stderr)]);
  exitCode=result[0].code;
  await writeFile(join(evidence,'execution.json'),JSON.stringify({...result[0],elapsed_ms:Date.now()-started,isolatedHome,output},null,2),{flag:'wx'});
  console.log(JSON.stringify({phase:'terminal',...result[0],elapsed_ms:Date.now()-started,evidence}));
}finally{
  await log.close();
  // The CLI may autostart a detached gateway. Stop only a process whose
  // current environment proves it belongs to this unique test home.
  try{
    const pid=Number((await readFile(join(isolatedHome,'gateway.pid'),'utf8')).trim());
    if(!Number.isInteger(pid)||pid<2)throw Error('invalid isolated gateway pid');
    const env=(await readFile(`/proc/${pid}/environ`,'utf8')).split('\0');
    if(!env.includes(`PHOENIX_HOME=${isolatedHome}`))throw Error('refusing to stop a gateway outside this test');
    process.kill(pid,'SIGTERM');
  }catch(error){if(!['ENOENT','ESRCH'].includes(error.code))throw error;}
  finally {
    // Even an ownership-check failure must not retain the copied token.
    await unlink(authPath).catch(error=>{if(error.code!=='ENOENT')throw error;});
  }
}
if(exitCode!==0)process.exitCode=1;
