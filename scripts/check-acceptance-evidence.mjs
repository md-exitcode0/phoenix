import assert from 'node:assert/strict';
import {peerResult} from './lib/acceptance-evidence.mjs';
const execution={turn_id:'turn',task_id:'task',attempt_id:'attempt'};
const handoff={kind:'handoff',handoff_id:'handoff',requester:'frontend',receiver:'researcher',execution,event_sequence:2};
const returned={kind:'return',reply_to:'handoff',requester:'frontend',receiver:'researcher',ok:true,status:'done',body:'Actual source findings',execution,event_sequence:3};
const assess=rows=>peerResult(rows,'turn','frontend','researcher');
assert.equal(assess([handoff,returned]),returned);
for(const mutation of [
  {ok:false},{requester:'foreign'},{receiver:'coder'},{status:'blocked'},
  {execution:{...execution,turn_id:'older'}},{execution:{...execution,task_id:'other'}},
  {execution:{...execution,attempt_id:'older'}},{body:' '},{event_sequence:1}
])assert.equal(assess([handoff,{...returned,...mutation}]),null);
assert.equal(assess([handoff,returned,returned]),null,'duplicate returns cannot count as exactly-once contribution');
assert.equal(assess([{...handoff,handoff_id:''},{...returned,reply_to:''}]),null);
assert.equal(assess([{...handoff,requester:'other'},returned]),null);
console.log('PASS: contributions require exact owner, peer, request, attempt, successful body and ordered unique return');
