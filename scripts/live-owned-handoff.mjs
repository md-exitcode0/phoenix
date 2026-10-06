import {isolatedTestHome} from './lib/isolated-test-home.mjs';
// Real provider-backed foreground handoff; never retry a submitted task.
import net from 'node:net';
import {mkdir,open} from 'node:fs/promises';
import {join} from 'node:path';
import {homedir} from 'node:os';
const phoenixHome=isolatedTestHome();
const [output,source,journalProbe,shape]=process.argv.slice(2);
if(![output,source].every(p=>p?.startsWith('/')))throw Error('Expected absolute directories');
await mkdir(output,{recursive:true});
const log=await open(join(output,'gateway-events.jsonl'),'wx',0o600);
const record=async value=>log.write(JSON.stringify({at:new Date().toISOString(),value})+'\n');
async function* request(body){
  const socket=net.createConnection(join(phoenixHome,'gateway.sock'));
  socket.on('connect',()=>socket.write(JSON.stringify(body)+'\n'));
  let pending='';
  try{for await(const chunk of socket){pending+=chunk;let split;while((split=pending.indexOf('\n'))>=0){const line=pending.slice(0,split);pending=pending.slice(split+1);if(line.trim())yield JSON.parse(line);}}}finally{socket.destroy();}
}
async function once(body){for await(const value of request(body)){if(value.Error)throw Error(JSON.stringify(value.Error));return value;}throw Error('Missing response; inspect receipts before retrying');}
const name='Owned handoff acceptance '+Date.now();
const created=await once({CompanyDirectory:{action:'create_group',name,description:'Read-only foreground handoff claim acceptance.',color:'#688c9e',icon_seed:'owned-handoff',members:['coder','researcher'],settings:{read_full_transcript:true}}});
const group=created.CompanyDirectory.directory.groups.find(g=>g.name===name);
const prompt=shape==='nested' ? [
  '@Leo owns this bounded read-only acceptance task. Delegate the source inspection once to internal room member researcher using foreground talk mode=1.',
  'Owner: ask researcher to inspect '+join(source,'validate_blend.py')+' with one read using line_numbers=true. Request the exact height pass expression and whether it measures all product geometry. Do not read the file yourself.',
  'Recipient researcher: before reading, send exactly one foreground talk to coder asking whether the verdict should measure all product geometry or just base-to-pad height. This is the only clarification. When the answer returns, perform the one numbered read and return your substantive findings to the original requester using final_answer; do not send a new talk to report completion.',
  'Owner when answering that clarification: respond using final_answer that the verdict must assess all product geometry. Do not delegate again and do not read. Your original outer task remains to publish one final user-facing verdict after the research result returns, citing the exact file line.',
  'No background jobs, file edits, network, credentials, routines, goals, geometry runs, repairs, extra reviewers or additional questions. This tests a nested follow-up while preserving the outer user task.',
].join('\n') : [
  '@Leo owns this small read-only acceptance task. The only permitted delegation is one foreground talk to the existing room member with internal ID researcher.',
  'Leo: call talk with to="researcher", mode=1, subject="Verify height expression". Ask the recipient to inspect '+join(source,'validate_blend.py')+' with one read using line_numbers=true, and return the exact height pass expression and whether it measures all product geometry. Do not inspect the file yourself. After the recipient returns, give one short final user-facing verdict with the verified expression and exact line citation. No background job, extra reviewer, or duplicate request.',
  'Recipient: your only work is the requested source check. Use one numbered read, then return your substantive result to the requester; do not delegate or ask questions.',
  'Both: no file edits, network, credentials, routines, goals, geometry runs or repairs. This is a deliberately bounded handoff-path acceptance test.',
].join('\n');
const {GroupActivationPreview:preview}=await once({GroupActivationPreview:{group_id:group.group_id,user_request:prompt}});
await record({Preview:preview});
if(JSON.stringify(preview.active_agent_ids)!==JSON.stringify(['coder']))throw Error('Unexpected initial participants '+JSON.stringify(preview.active_agent_ids));
const {execution_wave_display_names,active_display_names,...activation}=preview;
const turn_id='owned-handoff-'+Date.now();
await record({Submission:{groupId:group.group_id,sessionId:group.canonical_session_id,turn_id,prompt,activation}});
console.log(JSON.stringify({groupId:group.group_id,sessionId:group.canonical_session_id,turn_id}));
async function inspectReplay(phase){
  const rows=[];
  for await(const value of request({SubscribeJournal:{session_id:group.canonical_session_id}})){
    if(value.Error)throw Error(JSON.stringify(value.Error));
    if(value.StoryReplay)rows.push(value.StoryReplay);
    if(value==='Pong'||value.Pong!==undefined)break;
  }
  await record({JournalReplay:{phase,rows}});
  return rows;
}
const stories=[];let terminal=false,midReplay=null;
for await(const value of request({Turn:{session_id:group.canonical_session_id,turn_id,user_request:prompt,workspace:source,interaction_mode:'execute',permission_mode:'full_access',journal:true,target_group:group.group_id,group_activation:activation,delivery:'queue'}})){
  await record(value);
  if(value.Story){stories.push(value.Story);if(['group_message','group_member_status','handoff'].includes(value.Story.kind))console.log(JSON.stringify(value.Story));}
  if(journalProbe==='reconnect'&&midReplay===null&&value.Story?.kind==='handoff')midReplay=await inspectReplay('during-handoff');
  if(value.Error)throw Error(JSON.stringify(value.Error));
  if(value.Done){terminal=true;break;}
}
if(!terminal)throw Error('No terminal receipt; do not resubmit');
const finals=stories.filter(s=>s.kind==='group_message'&&s.agent_id==='coder');
const reads=stories.filter(s=>s.kind==='tool'&&s.tool==='read');
const checks={oneOwnerContribution:finals.length===1,
  delegatedReadOnly:reads.length===1&&reads[0].agent==='Theo (researcher)',
  exactCitation:/validate_blend\.py:61(?![0-9])/.test(finals[0]?.markdown??''),
  noFailedTools:!stories.some(s=>s.kind==='tool'&&!s.ok)};
if(shape==='nested'){
  const handoffs=stories.filter(s=>s.kind==='handoff');
  const returns=stories.filter(s=>s.kind==='return');
  checks.twoNestedRequests=handoffs.length===2;
  checks.eachRequestReturnedOnce=handoffs.length===2&&handoffs.every(h=>returns.filter(r=>r.reply_to===h.handoff_id).length===1);
}
if(journalProbe==='reconnect'){
  const endReplay=await inspectReplay('after-completion');
  const operational=new Set(['tool_start','tool','commentary','reasoning','narration','receipt','handoff','return','answer','settled','group_message']);
  const owned=stories.filter(s=>operational.has(s.kind)),replayed=endReplay.filter(s=>operational.has(s.kind));
  checks.liveOwnership=owned.length>0&&owned.every(s=>s.execution?.turn_id===turn_id&&s.execution?.task_id===turn_id&&s.execution?.attempt_id);
  checks.midRunReplay=Boolean(midReplay?.some(s=>s.execution?.turn_id===turn_id));
  checks.replayOwnership=replayed.length>0&&replayed.every(s=>s.execution?.turn_id===turn_id&&s.execution?.attempt_id===owned.find(s=>s.execution)?.execution.attempt_id);
  checks.replaySequence=replayed.every((s,i)=>i===0||s.event_sequence>=replayed[i-1].event_sequence);
  checks.replayOneRead=endReplay.filter(s=>s.kind==='tool'&&s.tool==='read').length===1;
}
await record({Checks:checks});console.log(JSON.stringify({checks}));await log.close();
if(!Object.values(checks).every(Boolean))process.exitCode=1;
