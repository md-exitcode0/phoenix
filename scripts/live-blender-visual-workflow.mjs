// Practical artifact workflow, explicitly distinct from live-blender-ui.mjs.
// The acting agent may script Blender, but must inspect actual pixels, repair
// visible defects, and open the editable scene in its native desktop.
import {mkdir,writeFile,readFile,open,stat,copyFile} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {bananaQualityBrief} from './lib/banana-quality-brief.mjs';
import {withAcceptanceGateway,messages,once} from './lib/acceptance-gateway.mjs';
import {currentBlenderTurn} from './lib/blender-tool-audit.mjs';
import {collectDesktopEvidence} from './collect-desktop-evidence.mjs';

const [binary,blender,output,repairFrom,deadlineArg]=process.argv.slice(2);
const deadlineMinutes=deadlineArg===undefined?20:Number(deadlineArg);
if(!Number.isInteger(deadlineMinutes)||deadlineMinutes<1||deadlineMinutes>45)throw Error('Deadline must be1–45minutes');
const workflow=repairFrom?'script-and-native-vision-reference-repair':'script-and-native-vision';
if(![binary,blender,output].every(p=>p?.startsWith('/')))throw Error('Absolute binary, Blender and NEW output required');
if([blender,output].some(p=>/[\r\n"`$\\]/.test(p)))throw Error('Unsupported desktop entry path');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
const scene=join(workspace,'banana.blend'),preview=join(workspace,'banana.png');
let repairBrief='',sourceHashes={};
if(repairFrom){
  if(!repairFrom.startsWith('/'))throw Error('Absolute completed source evidence required');
  const prior=JSON.parse(await readFile(join(repairFrom,'metrics.json'),'utf8'));
  const execution=JSON.parse(await readFile(join(repairFrom,'execution.json'),'utf8'));
  const review=JSON.parse(await readFile(join(repairFrom,'visual-review.json'),'utf8'));
  if(!(prior.terminal?.Done||prior.terminal?.Error)||execution.gateway?.code!==0||!execution.credentialRemoved)
    throw Error('Source test must be terminal with its owned gateway cleanly stopped and copied credentials removed');
  if(review.accepted!==false||!Array.isArray(review.findings))throw Error('A concrete rejected visual review is required');
  for(const name of ['create_banana.py','banana.blend','banana.png','banana-detail.png']){
    const source=join(repairFrom,'work',name),bytes=await readFile(source);
    sourceHashes[name]=createHash('sha256').update(bytes).digest('hex');
    if(name==='create_banana.py'){
      // Relocate output paths only; preserve all model-authored geometry/material code.
      await writeFile(join(workspace,name),bytes.toString('utf8').replaceAll(join(repairFrom,'work'),workspace),{flag:'wx'});
    }else await copyFile(source,join(workspace,name));
  }
  const guide=await readFile(new URL('../prompts/apps/blender.md',import.meta.url),'utf8');
  await writeFile(join(workspace,'blender-workflow.md'),guide,{flag:'wx'});
  const reference=new URL('../artifacts/banana-reference/',import.meta.url);
  const credit=JSON.parse(await readFile(new URL('source.json',reference),'utf8'));
  const photo=await readFile(new URL('banana-reference.jpg',reference));
  if(createHash('sha256').update(photo).digest('hex')!==credit.sha256)throw Error('Reference photograph changed');
  await writeFile(join(workspace,'real-banana-reference.jpg'),photo,{flag:'wx'});
  await writeFile(join(workspace,'reference-credit.json'),JSON.stringify(credit,null,2),{flag:'wx'});
  await writeFile(join(output,'repair-provenance.json'),JSON.stringify({repairFrom,sourceHashes,priorWorkflow:prior.workflow,pathRelocation:{from:join(repairFrom,'work'),to:workspace,scope:'literal workspace paths in the copied script only; no geometry/material edits'},guideSha256:createHash('sha256').update(guide).digest('hex'),reference:credit,review},null,2),{flag:'wx'});
  repairBrief=[
    'Continue your earlier saved banana, copied into this workspace with your own creation script. This is an explicitly reference-guided repair test, not a fresh unaided creation. Preserve a backup before making changes.',
    'Before editing, read blender-workflow.md for application guidance, then inspect the existing copied scene in native Blender. It contains no model or geometry supplied by the observer.',
    'Before editing, use image_analyze with path banana.png and reference_paths [real-banana-reference.jpg, banana-detail.png] so the current full render, real photo and current detail are visible together. The photo is a real Cavendish banana by Evan-Amos, CC BY-SA 3.0; metadata is in reference-credit.json. It is reference material only: do not insert this photo into your render or use it to substitute for editable 3D geometry.',
    'Independent review of the prior render: '+review.findings.join(' '),
    'Use the reference to compare overall volume, silhouette, peel ridges, stem cross-section, blossom end, pigment and lighting. Match physical plausibility, not an exact camera composition. Evaluate all original requirements after each repair; do not trade one defect for another. Save an additional whole-object render as banana-alt.png from a substantially different angle to check that the body has credible volume. Inspect that image too. Preserve the reference credit with the artifact.',
  ].join('\n');
}
await writeFile(join(output,'binary.json'),JSON.stringify({binary,
  sha256:createHash('sha256').update(await readFile(binary)).digest('hex'),
  workflow,pure_gui:false,deadlineMinutes,model:'gpt-5.6-sol',reasoning_effort:'medium'},null,2),{flag:'wx'});
const prompt=[
  bananaQualityBrief,
  repairBrief,
  'Create the actual finished banana. This is a practical Blender artifact workflow: choose effective native tools and scripting, then judge and improve the visible result.',
  `This test has a ${deadlineMinutes}-minute execution window from submission. Spend the first half producing and inspecting a meaningful changed render; reserve the final two minutes for saving, reopening the latest native scene, and an honest assessment. This scheduling constraint does not lower the visual quality bar. If a visible structural defect survives repeated small changes, change the construction approach instead of spending the remaining time on the same cosmetic tweak.`,
  `Blender is installed at ${blender}. You may write your own Python/bpy script in ${workspace} and run that executable in background mode. Put --python-exit-code 1 BEFORE --python: blender --background --python-exit-code 1 --python create_banana.py. Blender processes arguments in order; placing the error flag after the script can conceal Python failures as successful exits. Check command output and confirm each expected render was updated before inspecting it. Use CPU rendering if no GPU is available. Do not install packages, download/import assets, contact services, access credentials, read unrelated files, or delegate.`,
  `Save an editable scene to ${scene}; save a clean, well-lit full presentation render to ${preview} and a useful second-angle/detail render to ${join(workspace,'banana-detail.png')}.`,
  'Make a useful first draft and render early. Call image_analyze on the actual whole render and detail: inspect silhouette and proportions first, then stem/tip attachment, natural peel variation, and lighting. Identify the strongest visible defect and repair the scene, then render and inspect the changed result. Judge what the pixels show, not what the script intended or whether it exited successfully.',
  'The first useful render should happen before spending most of the task on fine detail. Preserve a checkpoint before repairs. Keep the original visual quality requirements through every iteration; do not label a toy-like or distorted banana finished.',
  'After saving a useful scene, open the installed app Phoenix Blender Quality using computer_open. It loads your saved scene in your private desktop. Inspect it there through computer_* tools, including another view when needed to check the form; do not assume a successful file save proves it opens correctly.',
  'After your last script edit and render, reopen the saved banana.blend in native Blender and inspect that final version. Any further edit requires saving and refreshing the native view again; an older open window does not verify a newer file.',
  'Use one image_analyze call per batch. For quality comparisons, include real-banana-reference.jpg in reference_paths when present and a useful current detail or whole alternate view as the second reference. The primary image and up to two references then reach the same model request; a crop applies only to the primary. Inspect that set before another repair decision.',
  'Use the existing todo and durable workflow tools as needed. Work only in this disposable desktop/workspace. Do not inspect Phoenix source, other sessions, or observer files.',
  'Finish with actual artifact paths and a concise honest assessment of anything unfinished. This run is not a UI-only computer-control benchmark; scripting is allowed and recorded. External independent visual review decides acceptance.',
].join('\n');
await writeFile(join(output,'request.txt'),prompt,{flag:'wx'});
await withAcceptanceGateway({binary,output,workspace,accountPool:true,preserveState:true,setup:async({env})=>{
  const apps=join(env.XDG_DATA_HOME,'applications');await mkdir(apps,{recursive:true});
  await writeFile(join(apps,'phoenix-blender-quality.desktop'),[
    '[Desktop Entry]','Type=Application','Name=Phoenix Blender Quality',
    `Exec="${blender}" --disable-autoexec --window-geometry 0 0 1440 960 "${scene}"`,
    'Terminal=false','',
  ].join('\n'),{flag:'wx'});
}},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const {CompanyDirectory:{directory}}=await once(socketPath,{CompanyDirectory:{action:'status'}});
  const actor=directory.agents.find(a=>a.internal_role==='coder');
  if(!actor?.canonical_session_id)throw Error('Disposable coder missing; no task submitted');
  const sessionId=actor.canonical_session_id,turnId='blender-visual-'+Date.now();
  await writeFile(join(output,'submission.json'),JSON.stringify({actor,sessionId,turnId},null,2),{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx',0o600);
  let terminal,canceled=false;const started=Date.now(),stories=[];
  const timer=setTimeout(async()=>{canceled=true;try{await once(socketPath,{Cancel:{session_id:sessionId,target_agent:actor.agent_id}});}catch(error){console.error(String(error));}},deadlineMinutes*60*1000);
  console.log(JSON.stringify({phase:'submitted',home,sessionId,turnId,workflow}));
  try{
    for await(const value of messages(socketPath,{Turn:{session_id:sessionId,turn_id:turnId,user_request:prompt,
      workspace,interaction_mode:'execute',permission_mode:'full_access',target_agent:actor.agent_id,journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify({at:new Date().toISOString(),value})+'\n');
      if(value.Story){stories.push(value.Story);if(['tool','status','answer','warning','compaction'].includes(value.Story.kind))console.log(JSON.stringify({elapsed_ms:Date.now()-started,...value.Story}));}
      if(value.Done||value.Error){terminal=value;break;}
    }
    if(!terminal)throw Error('No terminal receipt; inspect this existing turn, never auto-resubmit');
    const session=JSON.parse(await readFile(join(home,'sessions',sessionId+'.json'),'utf8'));
    const results=currentBlenderTurn(session.messages,prompt).filter(m=>m.type==='ToolResult');
    const receipts=results.filter(m=>m.tool_name!=='response_validation');
    await writeFile(join(output,'tool-receipts.json'),JSON.stringify(receipts,null,2),{flag:'wx',mode:0o600});
    await writeFile(join(output,'validation-receipts.json'),JSON.stringify(results.filter(m=>m.tool_name==='response_validation'),null,2),{flag:'wx',mode:0o600});
    const size=async p=>{try{return(await stat(p)).size;}catch(error){if(error.code==='ENOENT')return 0;throw error;}};
    const artifacts={};
    for(const name of ['create_banana.py','banana.blend','banana.png','banana-detail.png','banana-alt.png']){
      try{
        const path=join(workspace,name),bytes=await readFile(path),metadata=await stat(path);
        const sha256=createHash('sha256').update(bytes).digest('hex');
        artifacts[name]={sha256,bytes:bytes.length,modified_ms:metadata.mtimeMs,
          changed_from_source:sourceHashes[name]?sha256!==sourceHashes[name]:null};
      }catch(error){if(error.code!=='ENOENT')throw error;artifacts[name]=null;}
    }
    const scriptTime=artifacts['create_banana.py']?.modified_ms;
    const staleAfterScript=Object.entries(artifacts).filter(([name,file])=>name!=='create_banana.py'&&file&&scriptTime>file.modified_ms).map(([name])=>name);
    await writeFile(join(output,'artifact-provenance.json'),JSON.stringify({artifacts,outputs_older_than_script:staleAfterScript,
      scope:'File identity and freshness only. Changed bytes do not establish visual improvement or a successful native reopen.'},null,2),{flag:'wx'});
    const sourceUnchanged=repairFrom?(await Promise.all(Object.entries(sourceHashes).map(async([name,hash])=>createHash('sha256').update(await readFile(join(repairFrom,'work',name))).digest('hex')===hash))).every(Boolean):null;
    const metrics={workflow,pure_gui:false,deadlineMinutes,sourceUnchanged,terminal,canceled,elapsed_ms:Date.now()-started,
      outputs_older_than_script:staleAfterScript,
      tool_calls:receipts.length,observed_calls:stories.filter(s=>s.kind==='tool').length,
      failed_calls:receipts.filter(r=>!r.success).length,
      image_reviews:receipts.filter(r=>r.success&&r.tool_name==='image_analyze').length,
      native_calls:receipts.filter(r=>r.success&&r.tool_name.startsWith('computer_')).length,
      alternate_bytes:repairFrom?await size(join(workspace,'banana-alt.png')):null,
      scene_bytes:await size(scene),preview_bytes:await size(preview),detail_bytes:await size(join(workspace,'banana-detail.png')),
      visual_acceptance:'unverified; requires independent full/detail visual and read-only scene inspection'};
    await writeFile(join(output,'metrics.json'),JSON.stringify(metrics,null,2),{flag:'wx'});
    await collectDesktopEvidence(home,join(output,'screens'));
    console.log(JSON.stringify({phase:'terminal',...metrics}));
    if(staleAfterScript.length||(repairFrom&&(!metrics.alternate_bytes||!artifacts['banana.blend']?.changed_from_source||!artifacts['banana.png']?.changed_from_source))||sourceUnchanged===false||!terminal.Done||canceled||!metrics.scene_bytes||!metrics.preview_bytes||!metrics.detail_bytes||!metrics.image_reviews||!metrics.native_calls||metrics.tool_calls!==metrics.observed_calls)process.exitCode=1;
  }finally{clearTimeout(timer);await log.close();}
});
