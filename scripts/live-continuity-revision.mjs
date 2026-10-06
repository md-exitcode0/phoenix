// Resume real rejected work with one prepared continuity strategy and an equal archive.
import {mkdir,readFile,writeFile,copyFile,cp,open,stat} from 'node:fs/promises';
import {join} from 'node:path';
import {isDeepStrictEqual} from 'node:util';
import {createHash} from 'node:crypto';
import {messages,once,withAcceptanceGateway} from './lib/acceptance-gateway.mjs';
const [binary,blender,prior,output,prepared,strategy]=process.argv.slice(2);
if(![binary,blender,prior,output,prepared].every(p=>p?.startsWith('/'))||!['summary','notes'].includes(strategy))throw Error('Five absolute paths and summary/notes required');
const priorExecution=JSON.parse(await readFile(join(prior,'execution.json'),'utf8'));
if(priorExecution.gateway?.code!==0||!priorExecution.credentialRemoved)throw Error('Prior gateway has no verified clean terminal receipt');
const oldHome=priorExecution.home;
if(!oldHome.startsWith('/tmp/phoenix-ui-acceptance-'))throw Error('Not an owned acceptance home');
const snapshot=join(prior,'durable-state');
const sourceState=await stat(snapshot).then(s=>{if(!s.isDirectory())throw Error('Invalid durable state');return snapshot;},e=>{if(e.code==='ENOENT')return oldHome;throw e;});
const priorEvents=(await readFile(join(prior,'events.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
const review=priorEvents.map(row=>row.value?.Story).filter(s=>s?.kind==='group_message'&&s.agent_id==='critic').at(-1);
if(!review?.markdown)throw Error('No saved reviewer contribution');
await mkdir(output,{mode:0o700});const work=join(output,'work');await mkdir(work);
const originalHashes={};
for(const file of ['banana.blend','banana.png']){originalHashes[file]=createHash('sha256').update(await readFile(join(prior,'work',file))).digest('hex');await copyFile(join(prior,'work',file),join(work,file));}
await withAcceptanceGateway({binary,output,workspace:work,memory:true,preserveState:true,setup:async({home,env})=>{
  env.PHOENIX_DESKTOP_BACKEND='gnome';
  env.PHOENIX_NO_LIBRARIAN='1';
  // Copy only this test company's durable records, never credentials, native
  // processes, sockets, browser profiles, schedules or unrelated user history.
  for(const dir of ['company','sessions','cas'])await cp(join(sourceState,dir),join(home,dir),{recursive:true,errorOnExist:true});
  const id='group-'+review.group_id+'__coder';
  const sessionPath=join(home,'sessions',id+'.json');
  const sourceSession=JSON.parse(await readFile(sessionPath,'utf8'));
  const archived=(await readFile(join(prepared,'archive','continuity.archive.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  if(!isDeepStrictEqual(sourceSession.messages,archived))throw Error('Prepared history does not match the actual source session');
  const continuation=JSON.parse(await readFile(join(prepared,strategy+'-session.json'),'utf8'));
  const metadata=JSON.parse(await readFile(join(prepared,'comparison.json'),'utf8'));
  if(metadata.model!=='gpt-5.6-sol'||metadata.reasoning_effort!=='medium'||!metadata.source_unchanged)throw Error('Unverified continuity preparation');
  sourceSession.messages=continuation.messages;
  sourceSession.transcript_revision=(sourceSession.transcript_revision||0)+1;
  await writeFile(sessionPath,JSON.stringify(sourceSession));
  await copyFile(join(prepared,'archive','continuity.archive.jsonl'),join(home,'sessions',id+'.archive.jsonl'));
  await writeFile(join(output,'continuity.json'),JSON.stringify({strategy,prepared,session_id:id,retained_messages:continuation.messages.length,archived_messages:archived.length,preparation:metadata},null,2),{flag:'wx'});
  const apps=join(env.XDG_DATA_HOME,'applications');await mkdir(apps,{recursive:true});
  for(const value of [blender,join(work,'banana.blend')])if(/[\r\n"`$\\]/.test(value))throw Error('Unsupported desktop-entry path quoting');
  await writeFile(join(apps,'phoenix-blender-revision.desktop'),`[Desktop Entry]\nType=Application\nName=Phoenix Blender Revision\nExec="${blender}" --disable-autoexec "${join(work,'banana.blend')}"\nTerminal=false\n`,{flag:'wx'});
}},async({socketPath})=>{
  const models=(await once(socketPath,{Settings:{action:'models_snapshot'}})).Settings.snapshot;
  await writeFile(join(output,'models.json'),JSON.stringify(models,null,2),{flag:'wx'});
  for(const lane of models.lanes.filter(l=>['phoenix','specialist','coder'].includes(l.lane)&&l.configured!==false))
    if(lane.model!=='gpt-5.6-sol'||lane.reasoning_effort!=='medium')throw Error('Not Sol medium');
  await writeFile(join(output,'binary.json'),JSON.stringify({binary,sha256:createHash('sha256').update(await readFile(binary)).digest('hex'),desktop_backend:'gnome'},null,2),{flag:'wx'});
  const directory=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory;
  const group=directory.groups.find(g=>g.group_id===review.group_id);
  if(!group)throw Error('Prior group did not survive the isolated restore');
  const request=[
    '@Leo continue the unfinished repair from the latest rejected result, then @Remy independently review it. Iris and Theo have already completed their inspections; do not rerun them.',
    `Continue using the established group requirements and saved history. The latest working copies are ${join(work,'banana.blend')} and ${join(work,'banana.png')}. Use these exact copies for edits and delivery; earlier-run artifacts must remain unchanged. Phoenix Blender Revision opens the working scene.`,
    'Resolve the actual remaining defects, preserving successful work. Use recall if necessary to recover exact prior requirements, failed approaches or findings. Do not rebuild completed work or ask the user to repeat available context.',
    'Leo: make all scene and image changes through the application interface only. No Python, bpy, console, bash, MCP, scripts, installed assets, new workflows, skill installation, outside agents or web browsing. Verify the actual delivered files and describe remaining defects honestly.',
    'Remy: inspect the final files independently against the established requirements and publish an acceptance decision with concrete findings. Do not edit files. Read-only inspection is allowed.',
    'Publish one substantive contribution each and finish the requested work. Do not inspect evaluation logs or calculate testing metrics; the harness handles that independently.',
  ].join('\n');
  await writeFile(join(output,'request.txt'),request,{flag:'wx'});
  const preview=(await once(socketPath,{GroupActivationPreview:{group_id:group.group_id,user_request:request}})).GroupActivationPreview;
  await writeFile(join(output,'preview.json'),JSON.stringify(preview,null,2),{flag:'wx'});
  if(JSON.stringify(preview.execution_waves)!==JSON.stringify([['coder'],['critic']]))throw Error('Unexpected revision dependency plan');
  const {active_display_names,execution_wave_display_names,...activation}=preview;
  activation.tool_constraints={coder:['computer_*','image_analyze','recall','read','list_directory','todo_write','final_answer'],
    critic:['image_analyze','recall','read','list_directory','bash','todo_write','final_answer']};
  await writeFile(join(output,'constrained-activation.json'),JSON.stringify(activation,null,2),{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx',0o600),stories=[],started=Date.now();let terminal;
  const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:group.canonical_session_id}}).catch(()=>{}),15*60*1000);
  try{
    for await(const value of messages(socketPath,{Turn:{session_id:group.canonical_session_id,turn_id:'revision-'+Date.now(),user_request:request,
      workspace:work,target_group:group.group_id,group_activation:activation,interaction_mode:'execute',permission_mode:'full_access',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify({at:Date.now(),value})+'\n');
      if(value.Story){stories.push(value.Story);if(['group_member_status','group_message','tool'].includes(value.Story.kind))console.log(JSON.stringify(value.Story));}
      if(value.Done||value.Error){terminal=value;break;}
    }
  }finally{clearTimeout(timer);await log.close();}
  if(!terminal)throw Error('No terminal result; inspect the existing revision before resubmitting');
  const completed=stories.findIndex(s=>s.kind==='group_message'&&s.agent_id==='coder');
  const reviewStart=stories.findIndex(s=>s.kind==='group_member_status'&&s.agent_id==='critic'&&s.state==='working');
  const sourceUnchanged=(await Promise.all(Object.keys(originalHashes).map(async file=>createHash('sha256').update(await readFile(join(prior,'work',file))).digest('hex')===originalHashes[file]))).every(Boolean);
  const checks={sourceUnchanged,reviewAfterContribution:completed>=0&&reviewStart>completed,
    noCompletedInspectorRerun:!stories.some(s=>s.kind==='group_member_status'&&s.state==='working'&&['frontend','researcher'].includes(s.agent_id))};
  await writeFile(join(output,'result.json'),JSON.stringify({checks,strategy,elapsed_ms:Date.now()-started,tool_calls:stories.filter(s=>s.kind==='tool').length,failed_tools:stories.filter(s=>s.kind==='tool'&&s.ok===false).length,terminal},null,2),{flag:'wx'});
  if(terminal.Error||!Object.values(checks).every(Boolean))process.exitCode=1;
});
