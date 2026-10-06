import {isolatedTestHome} from './lib/isolated-test-home.mjs';
// Real model-backed waiting/independent-work/late-answer acceptance.
// A single intentional submission; never resubmit on observation timeout.
import net from 'node:net';
import {mkdir, open, readFile, unlink} from 'node:fs/promises';
import {join} from 'node:path';
import {homedir} from 'node:os';
const [output, source, existingGroupArg, answerTiming] = process.argv.slice(2);
const existingGroupId=existingGroupArg==='-'?undefined:existingGroupArg;
const recoveryProbe=answerTiming==='early-retry';
const early=answerTiming==='early'||recoveryProbe;
const phoenixHome=isolatedTestHome();
if(!phoenixHome.startsWith('/'))throw Error('PHOENIX_HOME must be absolute');
if (![output, source].every(p => p?.startsWith('/'))) throw Error('Expected absolute output and source paths');
await mkdir(output, {recursive:true});
const log = await open(join(output, 'gateway-events.jsonl'), 'wx', 0o600);
const record = async (phase, value) => log.write(JSON.stringify({at:new Date().toISOString(),phase,value})+'\n');
async function* request(body) {
  const socket = net.createConnection(join(phoenixHome,'gateway.sock'));
  socket.on('connect',()=>socket.write(JSON.stringify(body)+'\n'));
  let buffer='';
  try { for await (const chunk of socket) {
    buffer+=chunk;
    let split;
    while ((split=buffer.indexOf('\n'))>=0) {
      const line=buffer.slice(0,split); buffer=buffer.slice(split+1);
      if(line.trim()) yield JSON.parse(line);
    }
  }} finally {socket.destroy();}
}
async function once(body) {
  for await (const value of request(body)) {
    if(value.Error) throw Error(JSON.stringify(value.Error));
    return value;
  }
  throw Error('Connection closed without response; inspect receipts before retrying');
}
const name='Question branch acceptance '+Date.now();
const created=await once({CompanyDirectory:existingGroupId?{action:'status'}:{action:'create_group',name,
  description:'Acceptance fixture: read-only verifier audit with one synthetic tolerance question.',
  color:'#688c9e',icon_seed:'question-branch',members:['researcher','coder','critic'],settings:{read_full_transcript:true}}});
const group=created.CompanyDirectory?.directory.groups.find(g=>existingGroupId?g.group_id===existingGroupId:g.name===name);
if(!group) throw Error('Created group missing');
const session_id=group.canonical_session_id;
const prompt=[
  '@Theo and @Leo work independently in parallel; finally @Remy reconcile their saved contributions.',
  'This is a read-only group acceptance experiment. No file writes, network, credentials, goals, routines, or peer delegation. Each person does only their assigned work. Do not repeat another person’s work.',
  'Theo: your assigned decision is whether a measured overall height of 243.002 mm passes a nominal 240 mm requirement. The acceptance tolerance has deliberately NOT been selected. Before deciding, call ask_user exactly once with the exact question "Acceptance fixture: choose the overall-height tolerance" and options "0.1 mm" and "5 mm". This is an ordinary question, not an approval. Do not choose for the user. Publish a short waiting contribution and stop this branch until answered. On resumption use the answer to give the numerical pass/fail decision. No tools other than the question are needed for your arithmetic.',
  'Leo: independently read build_headphone_stand.py and validate_blend.py in '+source+'. Audit whether the height check measures the whole product or a narrower datum. Cite the actual calculation and explain what geometry it excludes. Do not measure geometry or ask questions; your code audit does not need Theo’s tolerance. Publish your substantive result when ready.',
  'Remy: wait for the completed tolerance decision and the independent code audit. Then return one concise combined acceptance verdict that distinguishes the measured dimension from the verifier’s proxy. Do not ask a second question, reread files, or perform repairs.',
].join('\n');
const {GroupActivationPreview:preview}=await once({GroupActivationPreview:{group_id:group.group_id,user_request:prompt}});
await record('preview',preview);
if(JSON.stringify(preview?.execution_dependencies?.map(e=>e.prerequisite+'>'+e.dependent).sort())!==JSON.stringify(['coder>critic','researcher>critic'])) throw Error('Unexpected dependencies '+JSON.stringify(preview));
const {execution_wave_display_names,active_display_names,...activation}=preview;
const turn_id='question-branch-'+Date.now();
await record('submission',{groupId:group.group_id,session_id,turn_id,prompt,activation});
console.log(JSON.stringify({groupId:group.group_id,session_id,turn_id}));
const initial=[];
let earlyStream=null,earlyAsk=null,earlyReply=null,originalBreadcrumb=null,earlyObservation=null;
let reviewerDone=false,unexpectedCoderRestart=false;
let answerAt=null,resumedAt=null,coderDoneAt=null;
async function observeContinuation(stream) {
  let continuationSeen=!early;
  for await(const value of stream) {
    const observedAt=Date.now();
    await record('continuation',value);
    if(value.Event?.WakeTurn?.turn_id&&value.Event.WakeTurn.turn_id!==turn_id)continuationSeen=true;
    const status=value.Event?.GroupMemberStatus;
    if(status?.agent_id==='coder'&&status.state==='done')coderDoneAt??=observedAt;
    if(status&&status.turn_id!==turn_id){
      console.log(JSON.stringify(status));
      if(status.agent_id==='researcher'&&status.state==='working')resumedAt??=observedAt;
      if(status.agent_id==='coder'&&status.state==='working')unexpectedCoderRestart=true;
      if(status.agent_id==='critic'&&status.state==='done')reviewerDone=true;
    }
    if(value.Error)throw Error(JSON.stringify(value.Error));
    // A parent's Done cannot close observation of a still-active successor.
    if(value.Event==='Done'&&continuationSeen&&reviewerDone)break;
  }
}
let terminal=false;
for await(const value of request({Turn:{session_id,turn_id,user_request:prompt,
  interaction_mode:'execute',permission_mode:'full_access',workspace:source,journal:true,
  target_group:group.group_id,group_activation:activation,delivery:'queue'}})) {
  await record('initial',value);
  if(early&&value.Event?.AskUser&&!earlyAsk){
    const asks=(await once({ConversationAsks:{session_id}})).ConversationAsks;
    const ask=asks.find(a=>a.ask_id===value.Event.AskUser.id&&a.status==='pending');
    if(!ask||ask.agent_id!=='researcher'||ask.questions.length!==1||ask.questions[0].question!=='Acceptance fixture: choose the overall-height tolerance'||ask.approval)throw Error('Early question identity mismatch');
    earlyStream=request({Subscribe:{session_id}});
    if((await earlyStream.next()).value!=='Pong')throw Error('Missing early subscription barrier');
    // Drain concurrently, not after the initial turn completes: arrival times
    // are evidence for answer latency and actual overlap with the sibling.
    earlyObservation=observeContinuation(earlyStream);
    earlyObservation.catch(()=>{}); // awaited below; avoid unhandled rejection
    earlyAsk=ask;
    if(recoveryProbe){
      if(!/^[A-Za-z0-9_-]{1,128}$/.test(ask.ask_id))throw Error('Invalid fixture ask identity');
      originalBreadcrumb=await readFile(join(phoenixHome,'approvals','asks',ask.ask_id+'.json'),'utf8');
      const saved=JSON.parse(originalBreadcrumb);
      if(saved.ask_id!==ask.ask_id||saved.session_id!==session_id||saved.approval||saved.agent_id!=='researcher')throw Error('Unsafe recovery fixture');
    }
    answerAt=Date.now();
    const reply=await once({AnswerAsk:{ask_id:ask.ask_id,answer:'0.1 mm',session_id}});
    earlyReply=reply;
    await record('early-answer',reply);console.log(JSON.stringify({earlyAnswer:reply}));
  }
  if(value.Story) {initial.push(value.Story);if(['group_member_status','group_message'].includes(value.Story.kind))console.log(JSON.stringify(value.Story));}
  if(value.Error)throw Error(JSON.stringify(value.Error));
  if(value.Done){terminal=true;break;}
}
if(!terminal)throw Error('No terminal receipt; do not resubmit');
const {ConversationAsks:asks}=await once({ConversationAsks:{session_id}});
await record('pending-asks',asks);
const pending=asks.filter(a=>a.status==='pending');
if(!early&&(pending.length!==1||pending[0].agent_id!=='researcher'||pending[0].questions.length!==1||pending[0].questions[0].question!=='Acceptance fixture: choose the overall-height tolerance'||pending[0].approval))throw Error('Question identity mismatch; no answer sent');
if(early&&!earlyAsk)throw Error('No early question received');
const lastStatus=id=>initial.filter(s=>s.kind==='group_member_status'&&s.agent_id===id).at(-1)?.state;
const checks={independentAuditCompleted:lastStatus('coder')==='done',
  researcherYielded:lastStatus('researcher')===(early?'continued':'waiting_user'),
  reviewerNotStarted:early||!initial.some(s=>s.kind==='group_member_status'&&s.agent_id==='critic'&&s.state==='working'),
  oneOwnedQuestion:true};
await record('initial-checks',checks);console.log(JSON.stringify({checks}));
if(!early&&!Object.values(checks).every(Boolean))throw Error('Initial branch isolation failed; question retained for inspection');
// Subscribe before answering so a fast continuation cannot outrun observation.
const stream=early?earlyStream:request({Subscribe:{session_id}});
if(!early){
  const barrier=await stream.next();
  if(barrier.value!=='Pong')throw Error('Missing subscription barrier');
  const answer=await once({AnswerAsk:{ask_id:pending[0].ask_id,answer:'0.1 mm',session_id}});
  await record('answer',answer);console.log(JSON.stringify(answer));
}
await (early?earlyObservation:observeContinuation(stream));
const finalAsks=(await once({ConversationAsks:{session_id}})).ConversationAsks;
await record('final-asks',finalAsks);
const finalChecks={reviewerCompleted:reviewerDone,noCompletedSiblingRerun:!unexpectedCoderRestart,
  noPendingQuestions:finalAsks.every(a=>a.status!=='pending'),singleQuestion:finalAsks.length===1};
if(early){
  finalChecks.answerResumedBeforeSiblingFinished=answerAt!==null&&resumedAt!==null&&coderDoneAt!==null&&answerAt<=resumedAt&&resumedAt<coderDoneAt;
  await record('overlap-evidence',{answerAt,resumedAt,coderDoneAt,answerToWorkingMs:resumedAt===null?null:resumedAt-answerAt,
    resumedBeforeSiblingFinished:finalChecks.answerResumedBeforeSiblingFinished});
}
await record('final-checks',finalChecks);console.log(JSON.stringify({finalChecks}));
if(recoveryProbe&&Object.values(finalChecks).every(Boolean)){
  // Controlled post-execution/pre-archival crash-state injection, not a process
  // restart. Restore ONLY this harness's exact non-approval pending breadcrumb.
  // The real work and its queue receipt have already completed.
  const path=join(phoenixHome,'approvals','asks',earlyAsk.ask_id+'.json');
  const transcriptPath=join(phoenixHome,'sessions',session_id+'.json');
  const transcriptBefore=await readFile(transcriptPath,'utf8');
  const restored=await open(path,'wx',0o600);
  try{await restored.writeFile(originalBreadcrumb);await restored.sync();}finally{await restored.close();}
  try{
    const reply=await once({AnswerAsk:{ask_id:earlyAsk.ask_id,answer:'0.1 mm',session_id}});
    const sameReceipt=reply.AskAnswered?.disposition===earlyReply.AskAnswered?.disposition;
    const pendingAfter=(await once({ConversationAsks:{session_id}})).ConversationAsks.some(a=>a.status==='pending');
    const queueAfter=(await once({QueuedTurns:{session_id}})).QueuedTurns;
    finalChecks.recoverySameReceipt=sameReceipt;
    finalChecks.recoveryNoPendingQuestion=!pendingAfter;
    finalChecks.recoveryQueueEmpty=Array.isArray(queueAfter)&&queueAfter.length===0;
    finalChecks.recoveryTranscriptUnchanged=(await readFile(transcriptPath,'utf8'))===transcriptBefore;
    await record('recovery-checks',{reply,sameReceipt,pendingAfter,queueEmpty:finalChecks.recoveryQueueEmpty,transcriptUnchanged:finalChecks.recoveryTranscriptUnchanged});
  }catch(error){
    finalChecks.recoverySameReceipt=false;
    await record('recovery-error',{message:String(error)});
  }finally{
    const remaining=await readFile(path,'utf8').catch(error=>{if(error.code==='ENOENT')return null;throw error});
    if(remaining!==null){
      if(remaining!==originalBreadcrumb)throw Error('Recovery fixture changed unexpectedly; retained for inspection');
      await unlink(path);
      await record('fixture-cleanup',{removedRestoredPendingBreadcrumb:true,originalAnsweredHistoryRetained:true});
    }
  }
  console.log(JSON.stringify({recoveryChecks:finalChecks}));
}
await log.close();
if(!Object.values(checks).every(Boolean)||!Object.values(finalChecks).every(Boolean))process.exitCode=1;
