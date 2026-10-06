import {isolatedTestHome} from './lib/isolated-test-home.mjs';
// One real group submission. Observation failure is never a reason to resubmit.
import net from 'node:net';
import {mkdir,open,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {homedir} from 'node:os';
import {createHash} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
import {citesLine} from './source-citation.mjs';
const [output,source,shape]=process.argv.slice(2);
const phoenixHome=isolatedTestHome();
if(!phoenixHome.startsWith('/'))throw Error('PHOENIX_HOME must be absolute');
const peerWait=shape==='peer-wait';
const atomicPlan=shape==='atomic-plan';
if(![output,source].every(p=>p?.startsWith('/')))throw Error('Expected absolute output and source paths');
await mkdir(output,{recursive:true});
const log=await open(join(output,'gateway-events.jsonl'),'wx',0o600);
const record=value=>log.write(JSON.stringify({at:new Date().toISOString(),value})+'\n');
async function* request(body){
  const socket=net.createConnection(join(phoenixHome,'gateway.sock'));
  socket.on('connect',()=>socket.write(JSON.stringify(body)+'\n'));
  let pending='';
  try{for await(const chunk of socket){pending+=chunk;let end;while((end=pending.indexOf('\n'))>=0){
    const row=pending.slice(0,end);pending=pending.slice(end+1);if(row.trim())yield JSON.parse(row);
  }}}finally{socket.destroy();}
}
async function once(body){for await(const row of request(body)){if(row.Error)throw Error(JSON.stringify(row.Error));return row;}throw Error('No response; inspect before retrying');}
const stamp=Date.now(),name='Named workflow acceptance '+stamp;
const created=await once({CompanyDirectory:{action:'create_group',name,description:'Synthetic assigned source-review workflow acceptance.',color:'#688c9e',icon_seed:'named-workflow',members:['coder','researcher'],settings:{read_full_transcript:true}}});
const group=created.CompanyDirectory.directory.groups.find(g=>g.name===name);
if(!group)throw Error('Created group missing');
const ids={goal:'goal_named_'+stamp,run:'run_named_'+stamp,brief:'node_brief_'+stamp,review:'node_review_'+stamp};
const planKey='live-plan-'+stamp;
if(atomicPlan){
  const stable=(prefix,seed)=>prefix+'_'+createHash('sha256').update(seed).digest('hex');
  ids.goal=stable('goal','plan:'+planKey);ids.run=stable('run','plan:'+planKey);
  ids.brief=stable('node',ids.run+':brief');ids.review=stable('node',ids.run+':review');
}
const file=join(source,'validate_blend.py');
const digest=async()=>createHash('sha256').update(await readFile(file)).digest('hex');
const before=await digest();
const prompt=[
  '@Leo owns this bounded read-only source-review acceptance. Use the existing work workflow API for two named assignments, plus one foreground talk to room member researcher. Do not create another group or use background workers.',
  atomicPlan ? 'Install the two assignments with exactly one work action workflow, workflow_action install_plan, workflow_payload '+JSON.stringify({idempotency_key:planKey,contract:{title:'Named source review',objective:'Verify height measurement scope',budget:{max_total_tokens:100000,max_wall_seconds:600,max_iterations:40},ownership:{scope:'group',owner_agent_id:'coder',group_id:group.group_id}},assignments:[{key:'brief',owner_agent_id:'researcher',title:'Source brief',outcome:'Exact height expression and scope'},{key:'review',owner_agent_id:'coder',title:'Review brief',outcome:'One accurate user verdict',dependencies:['brief']}]})+'. Use the returned run_id and assignments map for subsequent calls. Do not separately create a goal, open a run, or define nodes.' : 'Create exactly one goal '+ids.goal+' with contract title "Named source review", objective "Verify height measurement scope", budget {max_total_tokens:100000,max_wall_seconds:600,max_iterations:40}, and ownership {scope:"group",owner_agent_id:"coder",group_id:"'+group.group_id+'"}. Open run '+ids.run+' for that goal.',
  atomicPlan ? '' : 'Define '+ids.brief+' in phase execute, owner_agent_id researcher, title "Source brief", outcome "Exact height expression and scope", no dependencies. Define '+ids.review+' in phase execute, owner_agent_id coder, title "Review brief", outcome "One accurate user verdict", dependencies ["'+ids.brief+'"]. Give each mutation its own stable idempotency_key. Omit optional fields you do not need.',
  'Call work action workflow, workflow_action tick, workflow_run_id '+ids.run+' as yourself before delegation. It should claim no tasks: the brief belongs to the other member and your review is not ready. Do not impersonate a worker.',
  'Then call talk once to researcher mode=1. Give the exact run/task IDs and these instructions: use work workflow tick to claim your brief; read '+file+' once with line_numbers=true; identify the exact height-pass expression and whether it measures all product geometry. Persist your result with work workflow transition to review phase/state review, then commit phase/state succeeded, using your returned lease token, restart_state terminal for success, and result containing the expression, scope and numbered citation. Return your substantive answer with final_answer; do not ask or delegate.',
  'After the peer returns, tick your run again: only now should your review task be claimable. Verify the returned brief, persist your review via review then succeeded/commit with your own lease and result. Reconcile the run. Give one concise user-facing verdict with the exact source citation. Do not reread the source yourself or make unsupported geometry claims.',
  peerWait ? 'Controlled peer-wait fixture overrides the immediate-read/immediate-review steps above. In the first handoff, instruct researcher to claim the brief, then call work workflow_action wait_for with its lease and receipt {receipt_id:"wait_scope_'+stamp+'",responder:"coder",question:"Fixture scope acknowledgement: confirm source_only"}. It must return a concise waiting notice to you with final_answer, without reading the source or completing the node. This deliberately forced wait tests the protocol; do not ask the user. After that first return, you must resolve_wait on '+ids.brief+' using receipt_id wait_scope_'+stamp+' and answer "source_only". Repeat that exact resolve_wait once to verify harmless duplicate delivery. Then send exactly one further foreground talk to researcher: reclaim the SAME brief with tick (a fresh lease), read the source once, and complete the original brief as specified above. Only after that second return may you claim and complete your dependent review. Do not make a replacement node, reset a goal, or read the source yourself.' : '',
  'No edits, external services, credentials, installations, unrelated memory/history, or additional tasks. The only writes permitted are this synthetic workflow and its normal conversation. Use final_answer to return; do not invent a talk loop. If a tool rejects a request, correct its arguments in this same task rather than restarting it.',
].join('\n');
const {GroupActivationPreview:preview}=await once({GroupActivationPreview:{group_id:group.group_id,user_request:prompt}});
if(JSON.stringify(preview.active_agent_ids)!==JSON.stringify(['coder']))throw Error('Unexpected initial participants');
const {execution_wave_display_names,active_display_names,...activation}=preview;
const turnId='named-workflow-'+stamp;
await record({Submission:{group,ids,turnId,prompt,before}});
console.log(JSON.stringify({groupId:group.group_id,sessionId:group.canonical_session_id,ids,turnId}));
const stories=[];let terminal=false;
for await(const value of request({Turn:{session_id:group.canonical_session_id,turn_id:turnId,user_request:prompt,workspace:source,interaction_mode:'execute',permission_mode:'full_access',journal:true,target_group:group.group_id,group_activation:activation,delivery:'queue'}})){
  await record(value);
  if(value.Story){stories.push(value.Story);if(['tool','group_message','handoff'].includes(value.Story.kind))console.log(JSON.stringify(value.Story));}
  if(value.Error)throw Error(JSON.stringify(value.Error));
  if(value.Done){terminal=true;break;}
}
if(!terminal)throw Error('No terminal receipt; inspect this same task, do not resubmit');
const db=new DatabaseSync(join(phoenixHome,'company','company.sqlite'),{readOnly:true});
const nodes=db.prepare('SELECT node_id,owner_agent_id,state,result_json FROM workflow_nodes WHERE run_id=? ORDER BY node_id').all(ids.run);
const events=db.prepare('SELECT company_seq,event_type,payload FROM company_events WHERE run_id=? ORDER BY company_seq').all(ids.run);
const rows=events.map(e=>({...e,data:JSON.parse(e.payload)}));
const briefDone=rows.find(e=>e.data.node_id===ids.brief&&e.data.state==='succeeded');
const reviewClaim=rows.find(e=>e.event_type==='workflow_node_leased'&&e.data.node_id===ids.review);
const finals=stories.filter(s=>s.kind==='group_message'&&s.agent_id==='coder');
const reads=stories.filter(s=>s.kind==='tool'&&s.tool==='read');
const checks={twoNamedTasks:nodes.length===2&&nodes.some(n=>n.node_id===ids.brief&&n.owner_agent_id==='researcher')&&nodes.some(n=>n.node_id===ids.review&&n.owner_agent_id==='coder'),bothSucceeded:nodes.length===2&&nodes.every(n=>n.state==='succeeded'&&n.result_json),dependencyOrdering:!!briefDone&&!!reviewClaim&&briefDone.company_seq<reviewClaim.company_seq,correctClaimants:rows.filter(e=>e.event_type==='workflow_node_leased').length===(peerWait?3:2)&&rows.filter(e=>e.event_type==='workflow_node_leased').every(e=>e.data.worker_id===(e.data.node_id===ids.brief?'researcher':'coder')),oneResearchRead:reads.length===1&&reads[0].agent==='Theo (researcher)',oneUserVerdict:finals.length===1,exactCitation:/validate_blend\.py:61(?![0-9])/.test(finals[0]?.markdown??''),sourceUnchanged:before===await digest()};
checks.noFailedTools=!stories.some(s=>s.kind==='tool'&&!s.ok);
// Accept precise range/list citations as well as file:line. The original
// exact regex falsely rejected file:34-40, 44, 60-61 for required line61.
checks.exactCitation=citesLine(finals[0]?.markdown??'','validate_blend.py',61);
const receipts=[];
for(const actor of ['coder','researcher']){
  const session=JSON.parse(await readFile(join(phoenixHome,'sessions',group.canonical_session_id+'__'+actor+'.json'),'utf8'));
  for(const message of session.messages.filter(m=>m.type==='ToolResult'&&m.tool_name==='work'&&m.success)){
    const input=JSON.parse(message.input),result=JSON.parse(message.output.slice(message.output.indexOf('{')));
    receipts.push({actor,action:input.workflow_action,chars:message.output.length,result});
  }
}
const firstTick=receipts.find(r=>r.actor==='coder'&&r.action==='tick');
checks.initialOwnerTickWaited=firstTick?.result.report?.claimed===0&&firstTick.result.leased?.length===0;
checks.scopedMutationReceipts=receipts.filter(r=>['create_goal','define_node','transition','wait_for','resolve_wait'].includes(r.action)).every(r=>
  r.action==='create_goal' ? r.result.goal_id===ids.goal&&!r.result.workflow :
  r.result.workflow?.runs?.length===1&&r.result.workflow.runs[0].run_id===ids.run&&
  r.result.workflow.goals.every(g=>g.goal_id===ids.goal)&&r.result.workflow.nodes.every(n=>n.run_id===ids.run));
checks.runCompleted=db.prepare('SELECT state FROM workflow_runs WHERE run_id=?').get(ids.run)?.state==='completed';
if(atomicPlan){
  const installs=receipts.filter(r=>r.action==='install_plan');
  checks.oneAtomicSetup=installs.length===1&&!receipts.some(r=>['create_goal','open_run','define_node'].includes(r.action));
  checks.compactPlanReceipt=installs.length===1&&installs[0].result.run_id===ids.run&&installs[0].result.goal_id===ids.goal
    &&installs[0].result.assignments.brief===ids.brief&&installs[0].result.assignments.review===ids.review&&!installs[0].result.workflow;
}
if(peerWait){
  const waits=rows.filter(e=>e.data.node_id===ids.brief&&e.data.state==='waiting_peer');
  const answers=rows.filter(e=>e.data.node_id===ids.brief&&e.data.state==='ready'&&e.data.wait_json&&JSON.parse(e.data.wait_json).answer==='source_only');
  const claims=rows.filter(e=>e.event_type==='workflow_node_leased'&&e.data.node_id===ids.brief);
  checks.receiptWaitReleasedLease=waits.length===1&&waits[0].data.clear_lease===true&&JSON.parse(waits[0].data.wait_json).responder==='coder';
  checks.oneAnswerReadinessEvent=answers.length===1&&waits.length===1&&answers[0].company_seq>waits[0].company_seq;
  checks.newLeaseForSameAssignment=claims.length===2&&answers.length===1&&claims[1].company_seq>answers[0].company_seq&&claims[0].data.lease_id!==claims[1].data.lease_id;
  const resolutions=receipts.filter(r=>r.action==='resolve_wait');
  checks.duplicateAnswerDidNotRequeue=resolutions.length===2&&resolutions[0].result.made_ready===true&&resolutions[1].result.made_ready===false;
  checks.noUserQuestions=!stories.some(s=>s.kind==='tool'&&['ask_user','ask_for_login'].includes(s.tool));
}
await record({Checks:checks,Nodes:nodes,WorkflowEvents:events,ReceiptSizes:receipts.map(({result,...meta})=>meta)});db.close();await log.close();
console.log(JSON.stringify({checks}));if(!Object.values(checks).every(Boolean))process.exitCode=1;
