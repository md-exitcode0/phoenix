import {bananaQualityBrief} from './lib/banana-quality-brief.mjs';
// Resume a completed test group's repair using its saved history and artifacts.
import {mkdir,readFile,writeFile,copyFile,cp,open} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {messages,once,withAcceptanceGateway} from './lib/acceptance-gateway.mjs';
const [binary,blender,prior,output]=process.argv.slice(2);
if(![binary,blender,prior,output].every(p=>p?.startsWith('/')))throw Error('Four absolute paths required');
const priorExecution=JSON.parse(await readFile(join(prior,'execution.json'),'utf8'));
if(priorExecution.gateway?.code!==0||!priorExecution.credentialRemoved)throw Error('Prior gateway has no verified clean terminal receipt');
const hash=async path=>createHash('sha256').update(await readFile(path)).digest('hex');
const sourceHash=await hash(join(prior,'work','banana.blend'));
const sourcePreviewHash=await hash(join(prior,'work','banana.png'));
const oldHome=priorExecution.home;
if(!oldHome.startsWith('/tmp/phoenix-ui-acceptance-'))throw Error('Not an owned acceptance home');
const priorEvents=(await readFile(join(prior,'events.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
const review=priorEvents.map(row=>row.value?.Story).filter(s=>s?.kind==='group_message'&&s.agent_id==='critic').at(-1);
if(!review?.markdown)throw Error('No saved reviewer contribution');
await mkdir(output,{mode:0o700});const work=join(output,'work');await mkdir(work);
for(const file of ['banana.blend','banana.png'])await copyFile(join(prior,'work',file),join(work,file));
await withAcceptanceGateway({binary,output,workspace:work,preserveState:true,setup:async({home,env})=>{
  env.PHOENIX_DESKTOP_BACKEND='gnome';
  // Copy only this test company's durable records, never credentials, native
  // processes, sockets, browser profiles, schedules or unrelated user history.
  for(const dir of ['company','sessions','cas'])await cp(join(oldHome,dir),join(home,dir),{recursive:true,errorOnExist:true});
  const apps=join(env.XDG_DATA_HOME,'applications');await mkdir(apps,{recursive:true});
  for(const value of [blender,join(work,'banana.blend')])if(/[\r\n"`$\\]/.test(value))throw Error('Unsupported desktop-entry path quoting');
  await writeFile(join(apps,'phoenix-blender-revision.desktop'),`[Desktop Entry]\nType=Application\nName=Phoenix Blender Revision\nExec="${blender}" --disable-autoexec "${join(work,'banana.blend')}"\nTerminal=false\n`,{flag:'wx'});
}},async({socketPath})=>{
  const models=(await once(socketPath,{Settings:{action:'models_snapshot'}})).Settings.snapshot;
  await writeFile(join(output,'models.json'),JSON.stringify(models,null,2));
  for(const lane of models.lanes.filter(l=>['phoenix','specialist','coder'].includes(l.lane)&&l.configured!==false))
    if(lane.model!=='gpt-5.6-sol'||lane.reasoning_effort!=='medium')throw Error('Not Sol medium');
  const directory=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory;
  const group=directory.groups.find(g=>g.group_id===review.group_id);
  if(!group)throw Error('Prior group did not survive the isolated restore');
  const request=[
    bananaQualityBrief,
    '@Leo repair the saved work, then @Remy independently review it. Iris and Theo already completed their inspections; do not rerun their work.',
    `This is a revision of the prior rejected result, not a new build. The latest copies are ${join(work,'banana.blend')} and ${join(work,'banana.png')}. Do not assume the existing body or stem meets acceptance; the saved review identifies the actual defects.`,
    'Previous reviewer finding:\n'+review.markdown,
    'Resolve the reviewer’s concrete remaining findings. Before exporting, disable viewport overlays and verify that the grid, axes, 3D cursor and orange selection outlines are gone. A viewport render can retain these overlays even without editor panels. Repair the silhouette, tip openings and stem junction where the saved review requires it. You may rebuild defective geometry through the interface if that is more efficient. Ground your completion report in the actual exported image, and explicitly report any remaining defect rather than claiming it is absent.',
    `Leo: make the body read as a tapered banana with closed tips and a short attached stem, then deliver an actual clean preview without editor chrome. Aim for ten minutes. Open Phoenix Blender Revision, which loads the saved scene. For editor-specific keyboard shortcuts, use computer_window_act move {x,y} to position over the correct editor without clicking, then issue the key in the same batch. Use the interface only; no Python, bpy, console, bash, MCP, scripts, new assets, skill installation or outside agents. Save the existing scene at ${join(work,'banana.blend')}. In the 3D viewport use F3 "Viewport Render" (not "Viewport Render Image"), then Image > Save As for ${join(work,'banana.png')}. Inspect the actual saved PNG and verify the exact files. Keep the requested location and basename separate in file dialogs.`,
    'Remy: inspect the final files independently. Accept only a complete, well-framed banana without editor chrome, a clean short stem junction and closed tapered tips, a ripe-yellow surface, and no stray protrusions. State remaining defects honestly. Do not edit files or repeat the initial inspections unnecessarily.',
    'Use the saved group context. Publish one substantive contribution each. Reuse the durable goal if one exists; otherwise create one private goal for the unfinished outcome. No user questions, installs, web browsing or credentials. A prior agent claim that its deadline expired is historical and is not a deadline for this resumed work. Use current runtime timing and explicit cancellation, not an invented time budget. Original prior-run artifacts are evidence; edit only the new work copies.',
  ].join('\n');
  await writeFile(join(output,'request.txt'),request,{flag:'wx'});
  const preview=(await once(socketPath,{GroupActivationPreview:{group_id:group.group_id,user_request:request}})).GroupActivationPreview;
  await writeFile(join(output,'preview.json'),JSON.stringify(preview,null,2),{flag:'wx'});
  if(JSON.stringify(preview.execution_waves)!==JSON.stringify([['coder'],['critic']]))throw Error('Unexpected revision dependency plan');
  const {active_display_names,execution_wave_display_names,...activation}=preview;
  activation.tool_constraints={coder:['computer_*','image_analyze','read','list_directory','todo_write','work','final_answer'],
    critic:['image_analyze','read','list_directory','bash','todo_write','final_answer']};
  activation.inspection_participants=['critic'];
  await writeFile(join(output,'constrained-activation.json'),JSON.stringify(activation,null,2),{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx',0o600),stories=[],started=Date.now();let terminal;
  const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:group.canonical_session_id}}).catch(()=>{}),20*60*1000);
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
  const checks={reviewAfterContribution:completed>=0&&reviewStart>completed,
    sceneChanged:await hash(join(work,'banana.blend'))!==sourceHash,
    previewChanged:await hash(join(work,'banana.png'))!==sourcePreviewHash,
    noCompletedInspectorRerun:!stories.some(s=>s.kind==='group_member_status'&&s.state==='working'&&['frontend','researcher'].includes(s.agent_id))};
  await writeFile(join(output,'result.json'),JSON.stringify({checks,elapsed_ms:Date.now()-started,terminal,tool_calls:stories.filter(s=>s.kind==='tool').length,tool_failures:stories.filter(s=>s.kind==='tool'&&s.ok===false).length,originalUnchanged:await hash(join(prior,'work','banana.blend'))===sourceHash,limits:'Execution completion does not establish artifact quality; independently inspect the saved scene and PNG.'},null,2),{flag:'wx'});
  if(terminal.Error||!Object.values(checks).every(Boolean)||!stories.some(s=>s.kind==='tool'&&s.agent?.endsWith('(coder)')&&s.ok===true))process.exitCode=1;
});
