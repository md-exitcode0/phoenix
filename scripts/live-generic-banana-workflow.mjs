// End-to-end check for the ordinary visual workflow. The acting prompt stays
// deliberately plain: all research, reference inspection, saving, visual
// review, and iteration must come from Phoenix's shared operating contract.
import {mkdir,open,readFile,readdir,stat,writeFile} from 'node:fs/promises';
import {join,relative} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,messages,once} from './lib/acceptance-gateway.mjs';
import {currentBlenderTurn} from './lib/blender-tool-audit.mjs';
import {collectDesktopEvidence} from './collect-desktop-evidence.mjs';

const [binary,output,minutesArg='15']=process.argv.slice(2);
const minutes=Number(minutesArg);
if(![binary,output].every(value=>value?.startsWith('/')))throw Error('Absolute binary and NEW output paths required');
if(!Number.isInteger(minutes)||minutes<5||minutes>30)throw Error('Deadline must be 5–30 minutes');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
const prompt='Make me a banana in Blender.';
await writeFile(join(output,'prompt.txt'),prompt,{flag:'wx'});
await writeFile(join(output,'setup.json'),JSON.stringify({
  prompt,initialFiles:await readdir(workspace),observerSuppliedReferences:false,
  observerSuppliedBlenderInstructions:false,actingModel:'gpt-5.6-sol',actingEffort:'medium',
  visionModel:'gpt-6-astra',visionEffort:'low',nativeVision:true,deadlineMinutes:minutes,
  binarySha256:createHash('sha256').update(await readFile(binary)).digest('hex')
},null,2),{flag:'wx'});

const inputOf=receipt=>{try{return JSON.parse(receipt.input||'{}')}catch{return {}}};
const isResearch=receipt=>receipt.success&&['web_search','web_fetch','browser_navigate','browser_search'].includes(receipt.tool_name);
const isConstruction=receipt=>{
  const input=inputOf(receipt);
  if(['write','str_replace'].includes(receipt.tool_name))return /\.(py|blend)$/i.test(input.path||'');
  return receipt.tool_name==='bash'&&/\bbpy\b|blender[^\n]*(?:--python|--render)/i.test(input.command||'');
};
const isImageReview=receipt=>receipt.success&&receipt.tool_name==='image_analyze';
async function filesBelow(root){
  const out=[];
  async function walk(dir){
    for(const entry of await readdir(dir,{withFileTypes:true})){
      const path=join(dir,entry.name);
      if(entry.isDirectory())await walk(path);else if(entry.isFile())out.push(path);
    }
  }
  await walk(root);return out;
}

await withAcceptanceGateway({binary,output,workspace,accountPool:true,preserveState:true,
  visionModel:'gpt-6-astra',nativeVision:true},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const {CompanyDirectory:{directory}}=await once(socketPath,{CompanyDirectory:{action:'status'}});
  const actor=directory.agents.find(agent=>agent.internal_role==='coder');
  if(!actor?.canonical_session_id)throw Error('Disposable coder unavailable');
  const sessionId=actor.canonical_session_id,turnId='generic-banana-'+Date.now();
  await writeFile(join(output,'submission.json'),JSON.stringify({actor,sessionId,turnId},null,2),{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx',0o600);
  const started=Date.now();let terminal,canceled=false;const stories=[];
  const timer=setTimeout(async()=>{canceled=true;try{await once(socketPath,{Cancel:{session_id:sessionId,target_agent:actor.agent_id}})}catch(error){console.error(String(error))}},minutes*60_000);
  try{
    for await(const frame of messages(socketPath,{Turn:{session_id:sessionId,turn_id:turnId,
      user_request:prompt,workspace,target_agent:actor.agent_id,permission_mode:'full_access',
      interaction_mode:'execute',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify({elapsed_ms:Date.now()-started,frame})+'\n');
      if(frame.Story){stories.push(frame.Story);if(['tool','status','answer','warning','compaction'].includes(frame.Story.kind))
        console.log(JSON.stringify({elapsed_ms:Date.now()-started,...frame.Story}));}
      if(frame.Done||frame.Error){terminal=frame;break;}
    }
    if(!terminal)throw Error('No terminal receipt; inspect the existing turn and do not resubmit');
    const session=JSON.parse(await readFile(join(home,'sessions',sessionId+'.json'),'utf8'));
    const receipts=currentBlenderTurn(session.messages,prompt).filter(message=>message.type==='ToolResult'&&message.tool_name!=='response_validation');
    await writeFile(join(output,'tool-receipts.json'),JSON.stringify(receipts,null,2),{flag:'wx',mode:0o600});
    const files=await filesBelow(workspace),artifacts=[];
    for(const path of files){
      const metadata=await stat(path),bytes=await readFile(path);
      artifacts.push({path:relative(workspace,path),bytes:bytes.length,modifiedMs:metadata.mtimeMs,
        sha256:createHash('sha256').update(bytes).digest('hex')});
    }
    await writeFile(join(output,'artifacts.json'),JSON.stringify(artifacts,null,2),{flag:'wx'});
    const research=receipts.findIndex(isResearch),construction=receipts.findIndex(isConstruction);
    const reviews=receipts.map((receipt,index)=>isImageReview(receipt)?index:-1).filter(index=>index>=0);
    const ownOutputReviews=reviews.filter(index=>{
      const path=inputOf(receipts[index]).path||'';
      return /\.(png|jpe?g|webp)$/i.test(path)&&!/(reference|source|photo)/i.test(path);
    });
    const lastConstruction=receipts.map((receipt,index)=>isConstruction(receipt)?index:-1).filter(index=>index>=0).at(-1)??-1;
    const checks={
      prompt_is_plain:prompt==='Make me a banana in Blender.',
      empty_start:true,
      research_before_construction:research>=0&&construction>=0&&research<construction,
      reference_pixels_inspected_before_construction:reviews.some(index=>index>research&&index<construction),
      saved_editable_scene:artifacts.some(file=>file.path.endsWith('.blend')&&file.bytes>10_000),
      saved_render:artifacts.some(file=>file.path.endsWith('.png')&&file.bytes>20_000),
      reviewed_own_output:ownOutputReviews.length>0,
      iterated_after_visual_review:ownOutputReviews.some(index=>index>construction&&index<lastConstruction),
      reviewed_after_last_construction:reviews.some(index=>index>lastConstruction),
      completed_without_observer_prompting:Boolean(terminal.Done)&&!canceled,
      tool_receipts_match_live_story:receipts.length===stories.filter(story=>story.kind==='tool').length
    };
    const result={checks,terminal,canceled,elapsedMs:Date.now()-started,calls:receipts.length,
      failedCalls:receipts.filter(receipt=>!receipt.success).length,researchIndex:research,
      firstConstructionIndex:construction,lastConstructionIndex:lastConstruction,reviewIndexes:reviews,
      ownOutputReviewIndexes:ownOutputReviews,
      scope:'Behavior and artifact evidence. Independent visual review still decides whether the banana itself is good.'};
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2),{flag:'wx'});
    await collectDesktopEvidence(home,join(output,'screens'));
    console.log(JSON.stringify(result));
    if(!Object.values(checks).every(Boolean))process.exitCode=1;
  }finally{clearTimeout(timer);await log.close();}
});
