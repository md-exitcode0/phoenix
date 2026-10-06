// Does the ordinary build contract trigger useful visual research without a
// reference photo, a research instruction, or a task-specific quality brief?
// This observes preparation only; it does not certify a finished artifact.
import {mkdir,readFile,writeFile,open,readdir} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,once,messages} from './lib/acceptance-gateway.mjs';
import {currentBlenderTurn} from './lib/blender-tool-audit.mjs';
import {collectDesktopEvidence} from './collect-desktop-evidence.mjs';

const [binary,output]=process.argv.slice(2);
if(![binary,output].every(path=>path?.startsWith('/')))throw Error('Absolute binary and NEW output required');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
const prompt='Create a realistic ripe banana in Blender. Save an editable banana.blend and a polished render banana.png in this workspace.';
await writeFile(join(output,'prompt.txt'),prompt,{flag:'wx'});
await writeFile(join(output,'setup.json'),JSON.stringify({binary,binary_sha256:createHash('sha256').update(await readFile(binary)).digest('hex'),initial_files:await readdir(workspace),model:'gpt-5.6-sol',reasoning_effort:'medium',memory:false,observer_supplied_references:false,deadline_ms:240000},null,2),{flag:'wx'});

function isConstruction(tool,input){
  if(['write','str_replace'].includes(tool))return /\.(py|blend)$/i.test(input.path||'');
  return tool==='bash'&&/\bbpy\b|blender[^\n]*(?:--python|--render)/i.test(input.command||'');
}

await withAcceptanceGateway({binary,output,workspace,accountPool:true,preserveState:true},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const {CompanyDirectory:{directory}}=await once(socketPath,{CompanyDirectory:{action:'status'}});
  const actor=directory.agents.find(a=>a.internal_role==='coder');
  if(!actor?.canonical_session_id)throw Error('Disposable coder unavailable');
  const sessionId=actor.canonical_session_id,turnId='reference-discovery-'+Date.now();
  await writeFile(join(output,'submission.json'),JSON.stringify({actor,sessionId,turnId},null,2),{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx',0o600);
  let terminal,stopReason=null;const started=Date.now();
  const stop=async reason=>{if(stopReason)return;stopReason=reason;await once(socketPath,{Cancel:{session_id:sessionId,target_agent:actor.agent_id}});};
  const timer=setTimeout(()=>stop('preparation observation deadline').catch(error=>console.error(String(error))),240000);
  try{
    for await(const frame of messages(socketPath,{Turn:{session_id:sessionId,turn_id:turnId,user_request:prompt,workspace,target_agent:actor.agent_id,permission_mode:'full_access',interaction_mode:'execute',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify({elapsed_ms:Date.now()-started,frame})+'\n');
      if(frame.Story?.kind==='tool'){
        console.log(JSON.stringify({elapsed_ms:Date.now()-started,tool:frame.Story.tool,ok:frame.Story.ok,detail:frame.Story.detail}));
        if(['write','str_replace','bash'].includes(frame.Story.tool)&&!stopReason){
          const saved=JSON.parse(await readFile(join(home,'sessions',sessionId+'.json'),'utf8'));
          const receipts=currentBlenderTurn(saved.messages,prompt).filter(m=>m.type==='ToolResult');
          if(receipts.some(r=>r.success&&isConstruction(r.tool_name,JSON.parse(r.input))))await stop('first construction observed');
        }
      }
      if(frame.Done||frame.Error){terminal=frame;break;}
    }
    if(!terminal)throw Error('Missing terminal receipt; do not resubmit');
    const saved=JSON.parse(await readFile(join(home,'sessions',sessionId+'.json'),'utf8'));
    const receipts=currentBlenderTurn(saved.messages,prompt).filter(m=>m.type==='ToolResult'&&m.tool_name!=='response_validation');
    const construction=receipts.findIndex(r=>r.success&&isConstruction(r.tool_name,JSON.parse(r.input)));
    const discovery=receipts.findIndex(r=>r.success&&['web_search','web_fetch','browser_navigate'].includes(r.tool_name)&&!r.output.includes('0 completed with 0 unique result'));
    const inspection=receipts.findIndex(r=>r.success&&['image_analyze','browser_screenshot'].includes(r.tool_name));
    const checks={plain_request:!/(research|reference|search|photo)/i.test(prompt),discovery_before_construction:discovery>=0&&(construction<0||discovery<construction),image_inspection_after_discovery:inspection>discovery&&discovery>=0,image_inspection_before_construction:inspection>=0&&(construction<0||inspection<construction),native_pixels:inspection>=0&&/Image pixels attached to your next model request|screenshot attached as an image with your next message/.test(receipts[inspection].output)};
    const result={checks,elapsed_ms:Date.now()-started,terminal,stopReason,discovery,inspection,construction,calls:receipts.length,failed_calls:receipts.filter(r=>!r.success).length,scope:'Preparation behavior only. Independently inspect source URLs, downloaded pixels and ordering; tool names alone do not establish reference relevance, native pixel delivery or finished quality.'};
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2),{flag:'wx'});
    await writeFile(join(output,'tool-receipts.json'),JSON.stringify(receipts,null,2),{flag:'wx',mode:0o600});
    await collectDesktopEvidence(home,join(output,'screens'));
    console.log(JSON.stringify(result));if(!Object.values(checks).every(Boolean))process.exitCode=1;
  }finally{clearTimeout(timer);await log.close();}
});
