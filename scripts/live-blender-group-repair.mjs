import {bananaQualityBrief} from './lib/banana-quality-brief.mjs';
// Four real Sol-medium coworkers, ordinary group admission, UI-only repair.
import {mkdir,readFile,writeFile,copyFile,open,stat} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {messages,once,withAcceptanceGateway} from './lib/acceptance-gateway.mjs';
const [binary,blender,source,output,baselineScreenshot]=process.argv.slice(2);
if(![binary,blender,source,output].every(p=>p?.startsWith('/')))throw Error('Four absolute paths required');
await mkdir(output,{mode:0o700});const work=join(output,'work'),before=join(output,'before');
await mkdir(work);await mkdir(before);
await copyFile(join(source,'banana.blend'),join(before,'banana.blend'));
await copyFile(join(source,'banana.blend'),join(work,'banana.blend'));
const sourceImage=baselineScreenshot||join(source,'banana.png');
if(!sourceImage.startsWith('/'))throw Error('Baseline screenshot must be absolute');
await copyFile(sourceImage,join(before,'banana.png'));
if(!baselineScreenshot)await copyFile(sourceImage,join(work,'banana.png'));
const hash=async path=>createHash('sha256').update(await readFile(path)).digest('hex');
const initial={blend:await hash(join(source,'banana.blend')),preview:await hash(sourceImage)};
await writeFile(join(output,'binary.json'),JSON.stringify({binary,sha256:await hash(binary),model:'gpt-5.6-sol',reasoning_effort:'medium'},null,2),{flag:'wx'});
const inspector=resolve('scripts/inspect-blender-scene.py');
await withAcceptanceGateway({binary,output,workspace:work,preserveState:true,setup:async({env})=>{
  env.PHOENIX_DESKTOP_BACKEND='gnome';
  const apps=join(env.XDG_DATA_HOME,'applications');await mkdir(apps,{recursive:true});
  for(const value of [blender,join(work,'banana.blend')])if(/[\r\n"`$\\]/.test(value))throw Error('Unsupported desktop-entry path quoting');
  await writeFile(join(apps,'phoenix-blender-repair.desktop'),`[Desktop Entry]\nType=Application\nName=Phoenix Blender Repair\nExec="${blender}" --disable-autoexec "${join(work,'banana.blend')}"\nTerminal=false\n`,{flag:'wx'});
}},async({socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const models=(await once(socketPath,{Settings:{action:'models_snapshot'}})).Settings.snapshot;
  await writeFile(join(output,'models.json'),JSON.stringify(models,null,2),{flag:'wx'});
  for(const lane of models.lanes.filter(lane=>['phoenix','specialist','frontend','researcher','coder','critic'].includes(lane.lane)&&lane.configured!==false)) {
    if(lane.model!=='gpt-5.6-sol'||lane.reasoning_effort!=='medium')throw Error('Acceptance route is not Sol medium: '+JSON.stringify(lane));
  }
  const name='Blender finish review '+Date.now();
  const created=await once(socketPath,{CompanyDirectory:{action:'create_group',name,
    description:'Independent visual and geometry inspection, UI repair, final review',color:'#a66c38',icon_seed:'blender-review',
    members:['frontend','researcher','coder','critic'],settings:{read_full_transcript:false}}});
  const group=created.CompanyDirectory.directory.groups.find(g=>g.name===name);
  await writeFile(join(output,'group.json'),JSON.stringify(group,null,2),{flag:'wx'});
  const prompt=[
    bananaQualityBrief,
    '@Iris and @Theo inspect independently in parallel. @Leo owns the repair. @Remy owns final acceptance.',
    'Dependencies: Iris before Leo; Theo before Leo; Leo before Remy.',
    baselineScreenshot
      ? `The original UI-only banana task stopped unfinished. The saved scene has an unshaded curved body and a detached stem; there is no exported preview. The baseline PNG is a screenshot of its editor, not a render. Complete a polished recognizable banana: curved tapered body, plausible attached short stem, yellow surface, smooth silhouette and clean presentation. Inspect the actual work before deciding what to preserve or reshape. Baseline files: ${before}. Repair files: ${work}.`
      : `The earlier UI-only banana has a smooth yellow body but still lacks convincing ripe-peel detail and organic tips. Inspect the actual current scene; do not assume historical protrusions still exist. Preserve good work while bringing the whole artifact up to the standing quality bar. Baseline files: ${before}. Repair files: ${work}.`,
    'Iris: inspect the baseline PNG with image_analyze. Publish a concise visual repair brief covering silhouette, stem attachment, material consistency and clean framing. Do not edit or create files.',
    `Theo: independently inspect the baseline blend using read-only Blender inspection. Blender executable: ${blender}. Keep bash cwd within the repair workspace and pass the absolute baseline file path. The read-only inventory script ${inspector} is available; you may run it in a fresh --background --disable-autoexec --python-exit-code 7 process. Report which components/modifiers need attention and what your measurements do and do not prove. Do not modify or render the baseline or inspect credentials.`,
    `Leo: read both prerequisite contributions and repair the existing scene only through Blender's real interface. Open Phoenix Blender Repair, which loads ${join(work,'banana.blend')} in your private desktop. Use the reviewers’ actual findings to finish the body, stem, materials and presentation. Keep geometry that meets the brief; repair what does not. Aim for about ten minutes of repair work. Save banana.blend and a clean banana.png in ${work}. The preview must show a plausible attached short stem, no protrusion below the peel, smooth silhouette and the entire object without editor chrome. Use only computer_* tools, image_analyze, read/list_directory for file verification, todo_write, work action workflow for your durable goal, and final_answer. No Python, bpy, console, bash, MCP, browser, scripts or generated assets for creation/repair. Use the requested directory and basename separately in Save As.`,
    `Remy: wait for Leo's saved contribution, then independently inspect the repaired PNG and verify both requested files exist. Read-only geometry inspection is permitted if needed. Judge against the original defects and quality criteria, not the builder's checklist. Publish an explicit accepted/requires-changes decision with concrete evidence. Do not repair files or claim quality solely from tool success.`,
    'Everyone: one substantive contribution for your assignment; no acknowledgement rounds, reuse one durable workflow goal for substantial execution; no extra agents, no user questions. Work only on these named artifacts and the allowed inspector. No network browsing, credentials, installs or external communications. Do not manufacture metrics; the harness records them.',
  ].join('\n');
  await writeFile(join(output,'request.txt'),prompt,{flag:'wx'});
  const preview=(await once(socketPath,{GroupActivationPreview:{group_id:group.group_id,user_request:prompt}})).GroupActivationPreview;
  await writeFile(join(output,'activation.json'),JSON.stringify(preview,null,2),{flag:'wx'});
  const waves=preview.execution_waves;
  if(JSON.stringify(waves?.map(w=>[...w].sort()))!==JSON.stringify([['frontend','researcher'],['coder'],['critic']]))throw Error('Unexpected group dependency plan; no task submitted');
  const {execution_wave_display_names,active_display_names,...activation}=preview;
  activation.tool_constraints={
    frontend:['image_analyze','read','list_directory','todo_write','final_answer'],
    researcher:['read','list_directory','bash','todo_write','final_answer'],
    coder:['computer_*','image_analyze','read','list_directory','todo_write','work','final_answer'],
    critic:['image_analyze','read','list_directory','bash','todo_write','final_answer'],
  };
  activation.inspection_participants=['frontend','researcher','critic'];
  await writeFile(join(output,'constrained-activation.json'),JSON.stringify(activation,null,2),{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx',0o600),started=Date.now(),stories=[];
  const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:group.canonical_session_id}}).catch(()=>{}),20*60*1000);
  let terminal;
  const turnId='group-blender-'+Date.now();
  await writeFile(join(output,'submission.json'),JSON.stringify({sessionId:group.canonical_session_id,groupId:group.group_id,turnId,started},null,2),{flag:'wx'});
  console.log(JSON.stringify({phase:'submitted',sessionId:group.canonical_session_id,groupId:group.group_id,turnId,output}));
  try{
    for await(const value of messages(socketPath,{Turn:{session_id:group.canonical_session_id,turn_id:turnId,
      user_request:prompt,workspace:work,target_group:group.group_id,group_activation:activation,
      interaction_mode:'execute',permission_mode:'full_access',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify({at:Date.now(),value})+'\n');
      if(value.Story){stories.push(value.Story);if(['group_member_status','group_message','tool'].includes(value.Story.kind))console.log(JSON.stringify(value.Story));}
      if(value.Done||value.Error){terminal=value;break;}
    }
  }finally{clearTimeout(timer);await log.close();}
  if(!terminal)throw Error('No terminal receipt; inspect the existing group before retrying');
  const start=id=>stories.findIndex(s=>s.kind==='group_member_status'&&s.agent_id===id&&s.state==='working');
  const done=id=>stories.findIndex(s=>s.kind==='group_message'&&s.agent_id===id&&s.markdown?.trim());
  const tools=stories.filter(s=>s.kind==='tool');
  const checks={allStarted:['frontend','researcher','coder','critic'].every(id=>start(id)>=0),
    everyMemberPerformedWork:['frontend','researcher','coder','critic'].every(id=>
      tools.some(tool=>(tool.agent_id===id||tool.agent?.endsWith(`(${id})`))&&tool.ok===true)),
    parallelInspectors:Math.max(start('frontend'),start('researcher'))<Math.min(done('frontend'),done('researcher')),
    repairAfterBoth:start('coder')>Math.max(done('frontend'),done('researcher'))&&done('frontend')>=0&&done('researcher')>=0,
    reviewAfterRepair:start('critic')>done('coder')&&done('coder')>=0,
    originalUnchanged:await hash(join(source,'banana.blend'))===initial.blend&&await hash(sourceImage)===initial.preview};
  const artifacts={sceneChanged:await hash(join(work,'banana.blend'))!==initial.blend,
    previewBytes:await stat(join(work,'banana.png')).then(value=>value.size).catch(error=>{if(error.code==='ENOENT')return 0;throw error;})};
  await writeFile(join(output,'result.json'),JSON.stringify({checks,artifacts,elapsed_ms:Date.now()-started,tool_calls:tools.length,
    tool_failures:tools.filter(t=>t.ok===false).length,terminal,
    limits:'Scheduling checks do not establish repaired artifact quality. Inspect saved files independently.'},null,2),{flag:'wx'});
  if(terminal.Error||!Object.values(checks).every(Boolean)||!artifacts.sceneChanged)process.exitCode=1;
});
