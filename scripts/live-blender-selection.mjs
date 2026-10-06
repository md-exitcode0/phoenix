// Diagnose real model-directed selection and field editing on the failed scene.
import {mkdir,writeFile,readFile,copyFile,open} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,once,messages} from './lib/acceptance-gateway.mjs';
import {collectDesktopEvidence} from './collect-desktop-evidence.mjs';
const [binary,blender,source,output]=process.argv.slice(2);
if(![binary,blender,source,output].every(p=>p?.startsWith('/')))throw Error('Absolute paths required');
await mkdir(output,{mode:0o700});const workspace=join(output,'work');await mkdir(workspace);
const scene=join(workspace,'banana.blend');await copyFile(source,scene);
const hash=async path=>createHash('sha256').update(await readFile(path)).digest('hex');
const original=await hash(source);
await writeFile(join(output,'binary.json'),JSON.stringify({binary,sha256:await hash(binary),model:'gpt-5.6-sol',effort:'medium'},null,2));
await withAcceptanceGateway({binary,output,workspace,setup:async({env})=>{
  env.PHOENIX_DESKTOP_BACKEND='gnome';
  const apps=join(env.XDG_DATA_HOME,'applications');await mkdir(apps,{recursive:true});
  if([blender,scene].some(p=>/[\r\n"`$\\]/.test(p)))throw Error('Unsupported desktop command path');
  await writeFile(join(apps,'phoenix-blender-selection.desktop'),`[Desktop Entry]\nType=Application\nName=Phoenix Blender Selection\nExec="${blender}" --disable-autoexec "${scene}"\nTerminal=false\n`);
}},async({socketPath,home})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const models=(await once(socketPath,{Settings:{action:'models_snapshot'}})).Settings.snapshot;
  await writeFile(join(output,'models.json'),JSON.stringify(models,null,2));
  for(const lane of models.lanes.filter(l=>['phoenix','specialist','coder'].includes(l.lane)&&l.configured!==false))
    if(lane.model!=='gpt-5.6-sol'||lane.reasoning_effort!=='medium')throw Error('Not Sol medium');
  const directory=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory;
  const actor=directory.agents.find(a=>a.internal_role==='coder');
  const prompt=`Open Phoenix Blender Selection. This loads an existing unfinished banana scene in your private desktop. Your entire diagnostic task: select the object named Banana Stem, set its Location X to exactly 0.75 using a visible numeric field, verify the displayed value and selected object name, and save the existing ${scene}. Do not change the body or any other property. Use computer_read_text or computer_locate to inspect selection and field targets; these now deliver images directly to you without a separate vision model. Read each result before acting. If a click does not select the stem, inspect again and use a different visible selection path. Do not repeat ineffective coordinates. Aim for three minutes. Use only computer_* tools, final_answer, and todo_write. No terminal, scripts, Python, APIs, MCP, filesystem inspection, browser, delegation, or user questions. Report the verified object and X value or the precise unresolved state. This is a small diagnostic, not a request to finish the banana.`;
  const sessionId=actor.canonical_session_id,turnId='blender-selection-'+Date.now(),started=Date.now();
  await writeFile(join(output,'submission.json'),JSON.stringify({sessionId,turnId,prompt},null,2));
  const log=await open(join(output,'events.jsonl'),'wx');let terminal;const stories=[];
  const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:sessionId,target_agent:actor.agent_id}}).catch(()=>{}),5*60*1000);
  try {
    for await(const value of messages(socketPath,{Turn:{session_id:sessionId,turn_id:turnId,user_request:prompt,workspace,target_agent:actor.agent_id,interaction_mode:'execute',permission_mode:'full_access',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify(value)+'\n');
      if(value.Story){stories.push(value.Story);if(['tool','answer'].includes(value.Story.kind))console.log(JSON.stringify(value.Story));}
      if(value.Done||value.Error){terminal=value;break;}
    }
  } finally {clearTimeout(timer);await log.close();}
  if(!terminal)throw Error('No terminal receipt; inspect before retrying');
  await copyFile(join(home,'sessions',sessionId+'.json'),join(output,'session.json'));
  await collectDesktopEvidence(home,join(output,'screens'));
  const tools=stories.filter(s=>s.kind==='tool');
  await writeFile(join(output,'result.json'),JSON.stringify({terminal,elapsed_ms:Date.now()-started,tools:tools.length,toolFailures:tools.filter(t=>t.ok===false).length,sourceUnchanged:await hash(source)===original,sceneChanged:await hash(scene)!==original,limits:'Actual object property and unchanged body require independent scene inspection.'},null,2));
  if(terminal.Error)process.exitCode=1;
});
