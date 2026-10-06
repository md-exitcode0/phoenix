// Owned real-provider run: lose a channel observer, queue another request,
// and recover both exact finals from disk without resubmitting either task.
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,once,messages,sleep} from './lib/acceptance-gateway.mjs';
const [binary,output]=process.argv.slice(2);
if(![binary,output].every(p=>p?.startsWith('/')))throw Error('Absolute binary and NEW output required');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
const digest=s=>createHash('sha256').update(s).digest('hex');
await writeFile(join(output,'binary.json'),JSON.stringify({binary,sha256:digest(await readFile(binary))},null,2),{flag:'wx'});
await withAcceptanceGateway({binary,output,workspace,accountPool:true,preserveState:true},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const {CompanyDirectory:{directory}}=await once(socketPath,{CompanyDirectory:{action:'status'}});
  const actor=directory.agents.find(a=>a.agent_id==='phoenix');
  if(!actor?.canonical_session_id)throw Error('Fixture owner missing');
  const session=actor.canonical_session_id,owner={kind:'agent',id:actor.agent_id};
  const specs=['CHANNEL_RECOVERY_FIRST','CHANNEL_RECOVERY_SECOND'].map(label=>({
    label,turn:'channel_'+digest(output+'\0'+label),prompt:`Reply with exactly ${label}. No tools or other actions are needed.`
  }));
  const request=spec=>({Turn:{session_id:session,turn_id:spec.turn,user_request:spec.prompt,workspace,target_agent:actor.agent_id,owner,permission_mode:'workspace',journal:false,delivery:'queue'}});
  await writeFile(join(output,'submissions.json'),JSON.stringify({session,actor,specs},null,2),{flag:'wx'});
  const signal=AbortSignal.timeout(120000),started=Date.now();
  let disconnected=false,secondAck;
  try{
    // Closing the observer must leave the accepted run in Phoenix.
    for await(const frame of messages(socketPath,request(specs[0]),{signal})){
      if(frame.Error||frame.Done)throw Error('First run ended before observer-loss boundary');
      if(frame.Event){disconnected=true;break;}
    }
    for await(const frame of messages(socketPath,request(specs[1]),{signal})){
      if(frame.Done||frame.Error){secondAck=frame;break;}
    }
    const receiptPath=join(home,'channels','completions',digest(session)+'.json');
    let stored;
    for(let i=0;i<600;i++){
      stored=await readFile(receiptPath,'utf8').then(JSON.parse).catch(error=>{if(error.code==='ENOENT')return null;throw error;});
      if(specs.every(s=>stored?.receipts?.some(r=>r.key.turn===s.turn)))break;
      if(signal.aborted)throw signal.reason;
      await sleep(200);
    }
    const checks={observer_disconnected:disconnected,second_was_queued:secondAck?.Done?.completion==='queued',session_bound:stored?.session===session};
    for(const [i,spec]of specs.entries()){
      const receipt=stored?.receipts?.find(r=>r.key.turn===spec.turn);
      checks[`final_${i+1}`]=receipt?.markdown?.trim()===spec.label;
      checks[`request_bound_${i+1}`]=receipt?.key?.request===digest(JSON.stringify([session,spec.turn,actor.agent_id,workspace,spec.prompt]));
      checks[`one_receipt_${i+1}`]=stored?.receipts?.filter(r=>r.key.turn===spec.turn).length===1;
    }
    const result={checks,elapsed_ms:Date.now()-started,secondAck,stored,scope:'Real gateway direct and queued completion after observer disconnect; disk receipt reads only, no Telegram/Discord sends or resubmissions.'};
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2),{flag:'wx'});
    console.log(JSON.stringify(result));
    if(Object.values(checks).some(v=>!v))process.exitCode=1;
  }finally{
    await once(socketPath,{Cancel:{session_id:session,target_agent:actor.agent_id,owner}}).catch(()=>{});
  }
});
// The helper has stopped the owned gateway and removed copied credentials.
// Re-open the persisted file after that lifetime boundary.
const execution=JSON.parse(await readFile(join(output,'execution.json'),'utf8'));
const result=JSON.parse(await readFile(join(output,'result.json'),'utf8'));
const session=result.stored?.session;
const afterStop=JSON.parse(await readFile(join(execution.home,'channels/completions',digest(session)+'.json'),'utf8'));
result.checks.survives_gateway_stop=JSON.stringify(afterStop)===JSON.stringify(result.stored);
result.checks.clean_gateway_stop=execution.gateway?.code===0&&execution.credentialRemoved===true;
await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));
console.log(JSON.stringify({phase:'stopped-readback',checks:result.checks}));
if(Object.values(result.checks).some(v=>!v))process.exitCode=1;
