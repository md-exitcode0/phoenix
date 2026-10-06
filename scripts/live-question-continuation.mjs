// Exact question-answer continuation identity, on an owned disposable gateway.
// This exercises the backend needed by channel replies, not a real bot send.
import {mkdir,writeFile,readFile,open} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,once,messages,sleep} from './lib/acceptance-gateway.mjs';
const [binary,output]=process.argv.slice(2);
if(![binary,output].every(p=>p?.startsWith('/')))throw Error('Absolute binary and NEW output required');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
await writeFile(join(output,'binary.json'),JSON.stringify({binary,sha256:createHash('sha256').update(await readFile(binary)).digest('hex')},null,2),{flag:'wx'});
await withAcceptanceGateway({binary,output,workspace,accountPool:true,preserveState:true},async({socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const {CompanyDirectory:{directory}}=await once(socketPath,{CompanyDirectory:{action:'status'}});
  const actor=directory.agents.find(a=>a.agent_id==='phoenix');
  if(!actor?.canonical_session_id)throw Error('Fixture owner missing; no turn submitted');
  const sessionId=actor.canonical_session_id,owner={kind:'agent',id:actor.agent_id};
  const turnId='question-receipt-'+Date.now();
  const prompt='Local clarification test: use ask_user to ask exactly “Which label should I use?” with options “Blue” and “Orange”. Do not choose for me. Finish with a short waiting reply. After I answer that saved question, reply with only the chosen label. No other tools, research, files, messages or actions are needed.';
  await writeFile(join(output,'submission.json'),JSON.stringify({sessionId,turnId,actor,prompt},null,2),{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx',0o600);
  const started=Date.now(),signal=AbortSignal.timeout(120000);
  let ask,initial,reply,resumed,ended,retired=false;
  console.log(JSON.stringify({phase:'submitted',sessionId,turnId}));
  try{
    for await(const frame of messages(socketPath,{Turn:{session_id:sessionId,turn_id:turnId,user_request:prompt,workspace,target_agent:actor.agent_id,owner,permission_mode:'workspace',journal:true}},{signal})){
      await log.write(JSON.stringify({phase:'initial',frame})+'\n');
      if(frame.Event?.AskUser){if(ask)throw Error('Duplicate questions');ask=frame.Event.AskUser;}
      if(frame.Done||frame.Error){initial=frame;break;}
    }
    if(!initial?.Done||!ask?.id)throw Error('Initial question turn did not finish with one durable question');
    const journal=messages(socketPath,{SubscribeJournal:{session_id:sessionId,owner}},{signal});
    try{
      let ready=false;
      for await(const frame of journal){
        await log.write(JSON.stringify({phase:'continuation',frame})+'\n');
        if(frame==='Pong'&&!ready){
          ready=true;
          reply=await once(socketPath,{AnswerAsk:{ask_id:ask.id,answer:'Blue',session_id:sessionId,owner}});
          await writeFile(join(output,'answer-receipt.json'),JSON.stringify(reply,null,2),{flag:'wx'});
          if(!reply.AskAnswered?.continuation_turn_id)throw Error('Answer did not identify its exact successor');
        }
        const story=frame.Story;
        if(ready&&story?.execution?.turn_id===reply.AskAnswered.continuation_turn_id&&story.kind==='answer'){
          resumed=story;
        }
        if(ready&&story?.execution?.turn_id===reply.AskAnswered.continuation_turn_id&&story.kind==='execution_ended'){
          ended=story;break;
        }
      }
    }finally{await journal.return();}
    const queueId=reply.AskAnswered.disposition.replace(/^late_answer_queued:/,'');
    for(let i=0;i<20;i++){
      const snapshot=await once(socketPath,{QueuedTurns:{session_id:sessionId,owner}});
      const entry=snapshot.QueuedTurns?.find(q=>q.queue_id===queueId);
      if(!entry){retired=true;break;}
      if(entry.state==='failed')throw Error('Answer continuation failed');
      await sleep(250);
    }
    const asks=await once(socketPath,{ConversationAsks:{session_id:sessionId,owner}});
    await writeFile(join(output,'question-records.json'),JSON.stringify(asks,null,2),{flag:'wx'});
    const record=asks.ConversationAsks?.find(a=>a.ask_id===ask.id);
    const repeated=await once(socketPath,{AnswerAsk:{ask_id:ask.id,answer:'Blue',session_id:sessionId,owner}});
    const afterRepeat=await once(socketPath,{QueuedTurns:{session_id:sessionId,owner}});
    const checks={initial_question:!!ask,explicit_completion:initial?.Done?.completion==='completed',terminal_boundary:!!ended&&resumed?.execution?.attempt_id===ended.execution.attempt_id,exact_successor:resumed?.execution?.turn_id===reply?.AskAnswered?.continuation_turn_id,chosen_label:resumed?.markdown?.trim()==='Blue',question_recorded:record?.status==='answered_late'&&record?.answer==='Blue',queue_retired:retired,repeated_receipt:JSON.stringify(repeated)===JSON.stringify(reply),repeat_not_requeued:Array.isArray(afterRepeat.QueuedTurns)&&!afterRepeat.QueuedTurns.some(q=>q.queue_id===queueId)};
    const result={checks,elapsed_ms:Date.now()-started,initial,reply,resumed,ended,repeated,afterRepeat,scope:'Backend question-to-final correlation; no messaging platform involved.'};
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2),{flag:'wx'});
    console.log(JSON.stringify(result));
    if(Object.values(checks).some(v=>!v))process.exitCode=1;
  }catch(error){
    await once(socketPath,{Cancel:{session_id:sessionId,target_agent:actor.agent_id,owner}}).catch(()=>{});
    throw error;
  }finally{await log.close();}
});
