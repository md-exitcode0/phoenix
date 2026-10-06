// Replay an answered question's lost acknowledgement on a new isolated gateway.
// Copies only stopped acceptance state; never submits another model turn.
import {mkdir,readFile,writeFile,cp,readdir} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,once,sleep} from './lib/acceptance-gateway.mjs';
const [binary,source,output]=process.argv.slice(2);
if(![binary,source,output].every(p=>p?.startsWith('/')))throw Error('Absolute paths required');
const execution=JSON.parse(await readFile(join(source,'execution.json')));
if(execution.gateway?.code!==0||!execution.credentialRemoved||!execution.durableState||!/^\/tmp\/phoenix-ui-acceptance-[\w]+$/.test(execution.home))throw Error('Only cleanly stopped owned acceptance state is allowed');
const {sessionId,actor}=JSON.parse(await readFile(join(source,'submission.json')));
const receipt=JSON.parse(await readFile(join(source,'answer-receipt.json')));
const {ConversationAsks:records}=JSON.parse(await readFile(join(source,'question-records.json')));
if(records.length!==1||records[0].session_id!==sessionId||records[0].status!=='answered_late'||records[0].answer!=='Blue')throw Error('Expected exact completed question fixture');
const record=records[0],id=record.ask_id;
if(!/^[\w-]+$/.test(id))throw Error('Invalid question fixture');
const history=await readFile(join(execution.home,'approvals','ask-history',id+'.json'));
if(JSON.stringify(JSON.parse(history))!==JSON.stringify(record))throw Error('Archived question differs from captured gateway record');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
await writeFile(join(output,'provenance.json'),JSON.stringify({source,sourceExecution:execution,binary,sha256:createHash('sha256').update(await readFile(binary)).digest('hex'),historySha256:createHash('sha256').update(history).digest('hex'),scope:'Copy of stopped test company/session/queue state and exact archived question; no model turn submitted.'},null,2),{flag:'wx'});
await withAcceptanceGateway({binary,output,workspace,accountPool:true,preserveState:true,setup:async({home})=>{
  for(const name of ['company','sessions','cas'])await cp(join(execution.durableState,name),join(home,name),{recursive:true,errorOnExist:true,force:false}).catch(e=>{if(e.code!=='ENOENT')throw e;});
  const dir=join(home,'approvals','ask-history');await mkdir(dir,{recursive:true});
  await writeFile(join(dir,id+'.json'),history,{flag:'wx',mode:0o600});
}},async({home,socketPath})=>{
  const owner={kind:'agent',id:actor.agent_id};
  const answer={ask_id:id,answer:'Blue',session_id:sessionId,owner};
  const actual=await once(socketPath,{AnswerAsk:answer});
  const rejects=async(value,pattern)=>{try{await once(socketPath,value);return false;}catch(e){return pattern.test(String(e));}};
  const wrongScope=await rejects({AnswerAsk:{...answer,session_id:'agent-coder',owner:undefined}},/belongs to/);
  const changed=await rejects({AnswerAsk:{...answer,answer:'Orange'}},/different saved decision/);
  await once(socketPath,{DismissAsk:{ask_id:id,session_id:sessionId,owner}});
  await sleep(500);
  const asks=await once(socketPath,{ConversationAsks:{session_id:sessionId,owner}});
  const queue=await once(socketPath,{QueuedTurns:{session_id:sessionId,owner}});
  const runs=await readdir(join(home,'runs')).catch(e=>{if(e.code==='ENOENT')return [];throw e;});
  const checks={same_receipt:JSON.stringify(actual)===JSON.stringify(receipt),wrong_session_rejected:wrongScope,changed_answer_rejected:changed,dismiss_preserves_answer:JSON.stringify(asks.ConversationAsks?.find(a=>a.ask_id===id))===JSON.stringify(record),no_queued_work:queue.QueuedTurns?.length===0,no_new_model_run:runs.length===0};
  const result={checks,actual,queue,records:asks,runs};
  await writeFile(join(output,'result.json'),JSON.stringify(result,null,2),{flag:'wx'});console.log(JSON.stringify(result));
  if(Object.values(checks).some(v=>!v))process.exitCode=1;
});
