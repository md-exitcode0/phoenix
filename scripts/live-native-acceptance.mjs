// Real task through an existing gateway. Never restart or resubmit on timeout.
import net from 'node:net';
import { mkdir, open, readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
const [output, blender, resumeSession] = process.argv.slice(2);
// This talks to the user's LIVE gateway, where an orchestrator turn lands in
// Phoenix's real canonical conversation. An abandoned run once left its task
// (and its todo map) there, and a later unrelated request resumed it. Use the
// isolated harness (scripts/hard-trial.mjs) unless the user asked for a live run.
if (process.env.PHOENIX_LIVE_ACCEPTANCE !== '1') throw new Error('Refusing to run against the live gateway; set PHOENIX_LIVE_ACCEPTANCE=1 only for a user-requested live run, otherwise use scripts/hard-trial.mjs');
if (!output?.startsWith('/') || !blender?.startsWith('/')) throw new Error('Expected absolute output and Blender paths');
await mkdir(output, {recursive:true});
const evidence = output + '.evidence';
await mkdir(evidence, {recursive:true});
// Keep observer traces out of the agent's artifact workspace. Otherwise
// counting tool calls reads a log that changes on every subsequent call.
const log = await open(join(evidence, resumeSession ? 'gateway-repair-events.jsonl' : 'gateway-events.jsonl'), 'wx', 0o600);
const sessionId = resumeSession || 'native-acceptance-' + Date.now();
const prompt = resumeSession
  ? 'Correction to your completed Blender acceptance task: independent evaluated-vertex measurements pass the five geometry checks, but camera projection fails. Product projected Y bounds are -0.0913246 to 0.9975195; the base is cropped below the image. Your final statement that there was no clipping was incorrect. Continue the existing task in '+output+'. Preserve the accepted dimensions and connected geometry. Correct the camera/composition, render again at 1200x1000, and reopen the saved blend in a fresh Blender process. Measure projected bounds of every product mesh vertex using world_to_camera_view, excluding only studio/background meshes. Require the whole product to fit inside a 5% image margin on every side and remain in front of the camera. Inspect the resulting image, update verification.json with measured framing evidence, and report only verified results. Blender remains at '+blender+'. No network, credentials, installs, or unrelated file edits. Do not inspect observer logs or count tool calls; the external harness measures those after completion.'
  : 'Real-world offline Blender acceptance task. Work only in ' + output + '. Do not read credentials, browse websites, contact external services, modify existing project files, install anything, or ask questions. Blender is at ' + blender + '. Design a polished desktop headphone stand, not default primitives: rounded rectangular base 140 mm wide by 110 mm deep by 12 mm thick, rounded upright reaching 240 mm total height, curved padded saddle 75 mm wide, and recessed cable-management channel. Use millimeter units and named components. Graphite powder-coated metal, warm cork padding, restrained brass detail. Plausible connections; no floating geometry. Deliver headphone-stand.blend, a 1200x1000 studio product render preview.png, and verification.json containing measured dimensions, component names and actual render success. Use scripting for precise repeatable modeling if useful. Reopen the saved blend in a fresh Blender process to validate it independently. Inspect the render with an image tool and correct obvious defects. Report actual tool counts and elapsed time if available; never invent metrics. Distinguish measured checks from visual judgment. Execute the task, not a plan.';
const request={Turn:{session_id:sessionId,turn_id:sessionId+(resumeSession?'-repair-'+Date.now():'-turn'),user_request:prompt,interaction_mode:'execute',permission_mode:'full_access',workspace:output,journal:true,target_agent:null,target_group:null,group_activation:null,delivery:'queue'}};
const socket=net.createConnection(join(homedir(),'.phoenix','gateway.sock'));
let pending='',terminal=false;
console.log(JSON.stringify({sessionId,output,startedAt:new Date().toISOString()}));
socket.on('connect',()=>socket.write(JSON.stringify(request)+'\n'));
for await (const chunk of socket) {
  pending+=chunk.toString(); let split;
  while((split=pending.indexOf('\n'))>=0) {
    const line=pending.slice(0,split);pending=pending.slice(split+1);
    if(!line.trim())continue;
    const value=JSON.parse(line);
    await log.write(JSON.stringify({at:new Date().toISOString(),value})+'\n');
    if(value.Story)console.log(JSON.stringify({kind:value.Story.kind,tool:value.Story.tool,agent:value.Story.agent}));
    if(value.Error||value.Done){terminal=true;console.log(JSON.stringify(value));socket.end();break;}
  }
  if(terminal)break;
}
await log.close();
if(!terminal)throw new Error('Connection ended without a terminal receipt; inspect '+sessionId+' before any retry.');
// A terminal response (including a partial-work fallback) is not success.
const files=await Promise.all(['headphone-stand.blend','preview.png','verification.json'].map(async name=>{
  try {const bytes=await readFile(join(output,name));return{name,bytes};}
  catch(error){if(error.code==='ENOENT')return{name,bytes:null};throw error;}
}));
const [blend,png,verification]=files.map(file=>file.bytes);
const checks={blendFile:false,renderDimensions:!!png&&png.length>24&&png.subarray(1,4).toString()==='PNG'&&png.readUInt32BE(16)===1200&&png.readUInt32BE(20)===1000,verificationJson:false,independentGeometry:false};
try{checks.verificationJson=!!verification&&typeof JSON.parse(verification.toString())==='object';}catch{}
// Reopen in a separate process and inspect evaluated mesh data. A creator's
// own verification JSON or a descriptive component name is not sufficient.
// Blender also saves compressed files (the observed 5.2.1 output uses Zstd),
// so a literal BLENDER header is not a valid file acceptance requirement.
if(blend?.length) {
  try {
    const stdout=execFileSync(blender,['--background',join(output,'headphone-stand.blend'),'--python',fileURLToPath(new URL('./inspect-headphone-stand.py',import.meta.url))],{encoding:'utf8',maxBuffer:8*1024*1024});
    const line=stdout.split('\n').find(line=>line.startsWith('INDEPENDENT_ACCEPTANCE='));
    const geometry=JSON.parse(line?.slice('INDEPENDENT_ACCEPTANCE='.length)??'null');
    checks.blendFile=!!geometry?.bounds;
    checks.independentGeometry=geometry?.passed===true;
    console.log(JSON.stringify({independentGeometry:geometry}));
  } catch(error) {
    checks.independentGeometry=false;
    console.log(JSON.stringify({independentGeometryError:error.message}));
  }
}
console.log(JSON.stringify({artifactChecks:checks,passed:Object.values(checks).every(Boolean),note:'These checks do not establish visual quality or manufacturability.'}));
if(!Object.values(checks).every(Boolean))process.exitCode=1;
