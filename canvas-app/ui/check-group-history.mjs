import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

// Exercise production reconciliation without launching a browser or a model.
const source = readFileSync(new URL('./conversation.js', import.meta.url), 'utf8');
function extract(name) {
  const start = source.indexOf(`  function ${name}(`);
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  function ', start + 1));
}
let persists = 0;
const ctx = vm.createContext({
  state:{item:{kind:'group'},displayRows:[],activeTurnId:'turn-two',displayDirty:false,painting:false},
  canonicalAgentId:value => ({'Iris (frontend)':'frontend','Theo (researcher)':'researcher','Leo (coder)':'coder'})[value] || value || '',
  historyVisibleInConversation:() => true,
  isIncomingAgentTalk:() => false,
  agentContextMessageMeta:() => null,
  isAuthoredBoundaryEntry:entry => entry.value?.role === 'user' || entry.value?.kind === 'user',
  recoveredInsertionIndex:() => -1,
  trimDisplayRows:rows => rows,
  repairHandoffReturnOrder:rows => ({rows,changed:false}),
  displaySemantic:entry => JSON.stringify(entry.value),
  incomingAgentTalkKey:() => '',
  imageCommentUserMirrors:() => false,
  replaceDisplayRows:(rows,dirty) => {ctx.state.displayRows=rows;ctx.state.displayDirty=dirty;},
  scheduleDisplayPersist:() => {persists++;},
  answerKey:text => text,
});
for (const name of ['displayRole','ownedStoryTurn','ownedStoryEventKey','ownedStoryHasBoundary',
  'displayTurnId','rawHandoffIds','isCompletedAgentReturn','normalizedAgentTalkBody',
  'equivalentDisplayRows','ensureDisplayTurnIds','reconcileHistory','cloneDisplayValue','appendDisplay']) {
  vm.runInContext(extract(name), ctx);
}
const wrap = row => ({source:'history',value:row,turn_id:row.turn_id});
const user = turn => ({role:'user',turn_id:turn,text:'Continue'});
const handoff = (turn,id) => ({role:'handoff',kind:'handoff',historical:true,history_id:`handoff:${id}`,
  group_id:'design',turn_id:turn,message_id:id,handoff_id:id,from:'frontend',to:'coder',
  requester:'frontend',receiver:'coder',subject:'Check the site',body:'',status:'inactive',ok:null});
const returned = (turn,id,root) => ({role:'return',kind:'return',historical:true,history_id:`return:${id}`,
  group_id:'design',turn_id:turn,message_id:id,handoff_id:id,reply_to:root,causation_id:root,
  from:'coder',to:'frontend',requester:'frontend',receiver:'coder',subject:'Review',body:'Recorded result',status:'returned',ok:null});
const status = (turn,state='blocked') => ({role:'group_member_status',kind:'group_member_status',historical:true,
  history_id:`activation:${turn}`,activation_id:`activation:${turn}`,group_id:'design',turn_id:turn,agent_id:'frontend',state});
const rows = [user('turn-one'),handoff('turn-one','first'),returned('turn-one','result-one','first'),status('turn-one'),
  user('turn-two'),handoff('turn-two','second'),returned('turn-two','result-two','second'),status('turn-two')];

assert.equal(ctx.reconcileHistory(rows).added.length, rows.length);
assert.deepEqual(Array.from(ctx.state.displayRows, e => e.turn_id), rows.map(e => e.turn_id));
assert.equal(ctx.reconcileHistory(rows).added.length, 0, 'repeated reads do not multiply deliveries');
assert.equal(ctx.state.displayRows.length, rows.length);
assert.equal(ctx.equivalentDisplayRows(wrap(rows[1]),wrap(rows[2])),false,'a request and its return remain distinct');
assert.equal(ctx.equivalentDisplayRows(wrap(rows[2]),wrap({...rows[6],body:rows[2].body})),false,'same reply prose in later turns is independent');
assert.equal(ctx.equivalentDisplayRows(wrap(rows[1]),wrap({...rows[1],group_id:'private-group'})),false);

const live = (value,kind) => ({source:'story',turn_id:value.turn_id,value:{...value,role:undefined,historical:undefined,
  history_id:undefined,message_id:undefined,kind,execution:{turn_id:value.turn_id,task_id:value.turn_id,attempt_id:'actual-attempt'},event_sequence:19}});
const liveRequest = live({...rows[1],from:'Iris (frontend)',to:'Leo (coder)',status:'queued'},'handoff');
const liveReturn = live({...rows[2],handoff_id:'first',agent:'coder',status:'done'},'return');
const liveStatus = live(rows[3],'group_member_status');
ctx.state.displayRows = [wrap(rows[0]),liveRequest,liveReturn,liveStatus,wrap(rows[4])];
assert.equal(ctx.reconcileHistory(rows).added.length,3,'client-journal deliveries and final status already satisfy recovery');
assert.ok(ctx.state.displayRows.includes(liveRequest) && ctx.state.displayRows.includes(liveReturn));
assert.equal(ctx.state.displayRows.length,rows.length);
assert.equal(liveRequest.value.status,'queued','canonical snapshot does not overwrite live event state or sequence');
assert.equal(ctx.reconcileHistory(rows).added.length,0);

ctx.state.displayRows = rows.map(wrap);
const placement = ctx.appendDisplay('story',liveRequest.value,false,false);
assert.equal(placement,'before_answer');
assert.equal(ctx.state.displayRows.length,rows.length,'late live delivery replaces the matching recovered row');
assert.equal(ctx.state.displayRows[1].source,'story');
assert.equal(ctx.state.displayRows[1].value.execution.attempt_id,'actual-attempt');
assert.equal(ctx.state.activeTurnId,'turn-two','a late old-turn handoff does not reactivate its prompt');
assert.equal(persists,1);
assert.equal(ctx.reconcileHistory(rows).added.length,0);
assert.equal(ctx.state.displayRows.length,rows.length);

ctx.state.displayRows = rows.map(row => wrap({...row}));
ctx.state.displayRows[3].value.state='inactive';
const updated = ctx.reconcileHistory(rows);
assert.equal(updated.added.length,0);
assert.equal(updated.reordered,true,'updated persisted status schedules a repaint');
assert.equal(ctx.state.displayRows[3].value.state,'blocked');
assert.equal(ctx.reconcileHistory(rows).reordered,false);

// Real renderStory's replay guard must allow authoritative live replay to
// replace an inactive server snapshot, instead of dropping it as a duplicate.
Object.assign(ctx,{
  normalizeGroupOperationalAgent:value => value,isEphemeralVolumeHandoff:() => false,
  isCompactionEvent:() => false,storyVisibleInConversation:() => true,
  DURABLE_STORY_KINDS:new Set(['handoff']),isInternalRuntimeText:() => false,
});
ctx.state.displayRows=rows.map(row=>wrap({...row}));
ctx.state.activeTurnId='turn-two';ctx.state.replayNeedsRepaint=false;
vm.runInContext(extract('renderStory'),ctx);
ctx.renderStory(live({...rows[5],status:'queued'},'handoff').value,true);
assert.equal(ctx.state.displayRows[5].source,'story');
assert.equal(ctx.state.displayRows.length,rows.length);
assert.equal(ctx.state.replayNeedsRepaint,true);

// New typed history roles use the existing renderer entry points. No user
// final, model action, tool replay or working indicator is created here.
const calls=[];
Object.assign(ctx,{
  askAnswerPrompt:() => null,isInternalRuntimeText:() => false,
  renderHandoff:r => calls.push(['handoff',r]),renderReturn:r => calls.push(['return',r]),
  renderIncomingAgentTalk:r => calls.push(['disclosure',r]),renderGroupMemberStatus:r => calls.push(['status',r]),
  renderGroupMessage:() => {throw Error('Recovery invented a peer contribution');},
  renderAnswer:() => {throw Error('Recovery invented an owner final');},
  renderTool:() => {throw Error('Recovery invented tool activity');},
});
vm.runInContext(extract('renderHistory'),ctx);
ctx.renderHistory(rows[1]);ctx.renderHistory(rows[2]);ctx.renderHistory(rows[3]);
assert.deepEqual(calls.map(call=>call[0]),['handoff','return','disclosure','status']);
assert.equal(calls[2][1].body,'Recorded result');
assert.equal(calls[0][1].status,'inactive');
assert.equal(calls[2][1].ok,null);
console.log('PASS: exact turn/route recovery, request-vs-return identities, repeated prompts, client/live dedup, status refresh and existing renderer dispatch.');
