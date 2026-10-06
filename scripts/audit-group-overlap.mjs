// Read-only independent audit of a TERMINAL isolated group experiment.
import {readFile,access} from 'node:fs/promises';
import {join,resolve} from 'node:path';
const output=resolve(process.argv[2]||'');
const execution=JSON.parse(await readFile(join(output,'execution.json'),'utf8'));
const events=(await readFile(join(output,'experiment/gateway-events.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
const submitted=events.find(row=>row.phase==='submission')?.value;
if(!submitted)throw Error('No authoritative submission');
const session=JSON.parse(await readFile(join(execution.home,'sessions',submitted.session_id+'.json'),'utf8'));
const contributions=session.messages.filter(row=>row.type==='GroupContribution');
const visible=new Map();
const mismatches=[];
const delivered=new Set(),statusBeforeContribution=[],statusChecked=new Set();
for(const row of events){
  const status=row.value.Event?.GroupMemberStatus;
  if(status&&['done','continued','waiting_user'].includes(status.state)){
    const saved=contributions.find(item=>item.turn_id===status.turn_id&&item.agent_id===status.agent_id);
    if(saved){
      statusChecked.add(saved.message_id);
      if(!delivered.has(saved.message_id))statusBeforeContribution.push(saved.message_id);
    }
  }
  const event=row.value.Event?.GroupMessage||(row.value.Story?.kind==='group_message'?row.value.Story:null);
  if(!event)continue;
  const body=event.markdown;
  if(visible.has(event.message_id)&&visible.get(event.message_id).body!==body)mismatches.push(event.message_id);
  visible.set(event.message_id,{body,agent:event.agent_id});
  delivered.add(event.message_id);
}
for(const [id,event] of visible){
  const saved=contributions.filter(row=>row.message_id===id);
  if(saved.length!==1||saved[0].body!==event.body||saved[0].agent_id!==event.agent)mismatches.push(id);
}
const statuses=events.flatMap(row=>row.value.Event?.GroupMemberStatus?[{at:row.at,...row.value.Event.GroupMemberStatus}]:[]);
const siblingDone=statuses.find(row=>row.agent_id==='coder'&&row.state==='done');
const answeredWorking=statuses.find(row=>row.agent_id==='researcher'&&row.state==='working'&&row.turn_id!==submitted.turn_id);
const answeredDone=statuses.find(row=>row.agent_id==='researcher'&&row.state==='done'&&row.turn_id!==submitted.turn_id);
const reviewerWorking=statuses.find(row=>row.agent_id==='critic'&&row.state==='working');
const reviewerDone=statuses.find(row=>row.agent_id==='critic'&&row.state==='done');
const finalChecks=events.findLast(row=>row.phase==='final-checks')?.value;
const log=await readFile(join(output,'process.log'),'utf8');
const tokenRemoved=await access(join(execution.home,'auth-profiles.json')).then(()=>false,error=>{if(error.code==='ENOENT')return true;throw error;});
const checks={
  terminalSuccess:execution.code===0,
  harnessChecksPassed:!!finalChecks&&Object.values(finalChecks).every(Boolean),
  canonicalIdentity:session.id===submitted.session_id,
  visibleReceiptsMatchSaved:visible.size>=4&&mismatches.length===0,
  canonicalIdsUnique:new Set(contributions.map(row=>row.message_id)).size===contributions.length,
  contributionPrecedesItsStatus:contributions.length>=4&&statusChecked.size===contributions.length&&statusBeforeContribution.length===0,
  actualOverlap:!!answeredWorking&&!!siblingDone&&Date.parse(answeredWorking.at)<Date.parse(siblingDone.at),
  reviewerAfterBoth:!!reviewerWorking&&!!answeredDone&&!!siblingDone&&Date.parse(reviewerWorking.at)>=Math.max(Date.parse(answeredDone.at),Date.parse(siblingDone.at)),
  reviewerFinished:!!reviewerDone,
  noReceiptOrMissingInputWarning:!/could not finalize idempotency receipt|required group contributions are missing|Ready group work remains saved but could not be queued/.test(log+JSON.stringify(events)),
  tokenRemoved,
};
console.log(JSON.stringify({checks,visibleReceiptCount:visible.size,canonicalContributionCount:contributions.length,mismatches,statusCheckedCount:statusChecked.size,statusBeforeContribution,
  timing:{answeredWorking:answeredWorking?.at,siblingDone:siblingDone?.at,reviewerWorking:reviewerWorking?.at,reviewerDone:reviewerDone?.at},
  reviewer:contributions.filter(row=>row.agent_id==='critic').map(row=>row.body)},null,2));
if(!Object.values(checks).every(Boolean))process.exitCode=1;
