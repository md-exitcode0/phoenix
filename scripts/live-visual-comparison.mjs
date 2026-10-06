// Real acting-model comparison of a prior agent render, its photo reference,
// and its detail render. No geometry changes and no observer-written verdict.
import {mkdir,copyFile,readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,once,messages} from './lib/acceptance-gateway.mjs';
const [binary,source,output]=process.argv.slice(2);
if(![binary,source,output].every(p=>p?.startsWith('/')))throw Error('Absolute binary, source and NEW output required');
await mkdir(output,{mode:0o700});const workspace=join(output,'work');await mkdir(workspace);
const hashes={};
for(const file of ['banana.png','banana-detail.png','real-banana-reference.jpg']){
  await copyFile(join(source,file),join(workspace,file));
  hashes[file]=createHash('sha256').update(await readFile(join(workspace,file))).digest('hex');
}
await copyFile(join(source,'reference-credit.json'),join(workspace,'reference-credit.json'));
const primaryBytes=await readFile(join(workspace,'banana.png'));
const primarySize={width:primaryBytes.readUInt32BE(16),height:primaryBytes.readUInt32BE(20)};
await writeFile(join(output,'provenance.json'),JSON.stringify({source,hashes,binary,binary_sha256:createHash('sha256').update(await readFile(binary)).digest('hex')},null,2),{flag:'wx'});
const prompt='Read-only visual comparison. Use exactly one image_analyze call with path "banana.png" and reference_paths ["real-banana-reference.jpg", "banana-detail.png"]. Image 1 is the complete agent-made render; image 2 is the real banana reference; image 3 is a close view of the same agent-made render. Compare physical plausibility, silhouette, stem continuity, peel and lighting using their actual pixels together. After that call, give the two strongest visible differences that matter for a convincing ripe banana, and say whether the render meets that standard. Separate what you see from any guess about construction. Do not edit files, use other tools, delegate, or assume the reference is itself a render. Keep the final under 180 words. You are inspecting an existing artifact, not being asked to build one.';
await writeFile(join(output,'prompt.txt'),prompt,{flag:'wx'});
await withAcceptanceGateway({binary,output,workspace,accountPool:true,preserveState:true},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const {CompanyDirectory:{directory}}=await once(socketPath,{CompanyDirectory:{action:'status'}});
  const actor=directory.agents.find(a=>a.agent_id==='phoenix');
  const session=actor.canonical_session_id,owner={kind:'agent',id:actor.agent_id},events=[];
  let terminal;const started=Date.now();
  try{
    for await(const frame of messages(socketPath,{Turn:{session_id:session,turn_id:'visual-compare-'+Date.now(),user_request:prompt,workspace,target_agent:actor.agent_id,owner,permission_mode:'workspace',journal:false}},{signal:AbortSignal.timeout(120000)})){
      events.push(frame);if(frame.Done||frame.Error){terminal=frame;break;}
    }
    const saved=JSON.parse(await readFile(join(home,'sessions',session+'.json'),'utf8'));
    const tools=saved.messages.filter(m=>m.type==='ToolResult');
    const inspection=tools.filter(m=>m.tool_name==='image_analyze');
    const text=JSON.stringify(inspection);
    const crop=inspection[0]?.input?JSON.parse(inspection[0].input).crop:null;
    const primaryWhole=!crop||(crop.x===0&&crop.y===0&&crop.width===primarySize.width&&crop.height===primarySize.height);
    const checks={whole_primary:primaryWhole,completed:terminal?.Done?.completion==='completed',one_inspection:inspection.length===1,inspection_success:inspection[0]?.success===true,reference_labels:text.includes('2 = reference')&&text.includes('3 = reference'),native_delivery:text.includes('Image pixels attached to your next model request'),no_other_tools:tools.every(m=>['image_analyze','response_validation'].includes(m.tool_name)),sources_unchanged:(await Promise.all(Object.entries(hashes).map(async([file,hash])=>createHash('sha256').update(await readFile(join(workspace,file))).digest('hex')===hash))).every(Boolean)};
    const result={checks,primarySize,elapsed_ms:Date.now()-started,answer:terminal?.Done?.final_markdown,terminal,inspection,scope:'Real native comparison, not a repair or automatic visual-quality verdict.'};
    await writeFile(join(output,'events.json'),JSON.stringify(events,null,2),{flag:'wx'});
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2),{flag:'wx'});
    console.log(JSON.stringify(result));if(Object.values(checks).some(v=>!v))process.exitCode=1;
  }finally{await once(socketPath,{Cancel:{session_id:session,target_agent:actor.agent_id,owner}}).catch(()=>{});}
});
