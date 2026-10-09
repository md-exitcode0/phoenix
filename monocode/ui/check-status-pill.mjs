// Status pill: tool → plain-words label, and the one indicator state machine.
// Usage: node check-status-pill.mjs
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
const require=createRequire(import.meta.url);
const {toolLabel,indicatorState,STALE_MS}=require('./status-pill.js');
const names={designer:'Leon Lin',researcher:'Theo'},resolve=(raw)=>names[raw]||'';

assert.equal(toolLabel('read',JSON.stringify({path:'/w/site/school-guide.md'})),'Reading school-guide.md');
assert.equal(toolLabel('read','docs/plan.md'),'Reading plan.md');
assert.equal(toolLabel('str_replace',JSON.stringify({path:'/w/site/index.html'})),'Editing index.html');
assert.equal(toolLabel('write',JSON.stringify({path:'/w/site/styles.css'})),'Writing styles.css');
assert.equal(toolLabel('web_search',JSON.stringify({query:'vvs exams'})),'Searching the web');
assert.equal(toolLabel('web_fetch',JSON.stringify({url:'https://www.vvs.ca/courses'})),'Reading vvs.ca');
assert.equal(toolLabel('bash',JSON.stringify({command:'npm test'})),'Running tests');
assert.equal(toolLabel('bash',JSON.stringify({command:'cargo test --lib'})),'Running tests');
assert.equal(toolLabel('bash',JSON.stringify({command:'ls -la'})),'Running a command');
assert.equal(toolLabel('message_agent',JSON.stringify({to:['designer'],subject:'x'}),resolve),'Messaging Leon');
assert.equal(toolLabel('message_agent',JSON.stringify({to:['designer','researcher']}),resolve),'Messaging Leon and others');
assert.equal(toolLabel('talk',JSON.stringify({to:'researcher'}),resolve),'Asking Theo');
assert.equal(toolLabel('browser_navigate',JSON.stringify({url:'https://vvs-moodle.pembinahills.ca/login'})),'Opening vvs-moodle.pembinahills.ca');
assert.equal(toolLabel('browser_click','{}'),'Using the browser');
assert.equal(toolLabel('some_new_tool','{}'),'Working');
assert.equal(toolLabel('',''),'Working');

const now=1_000_000;
// Running: the gateway says so, or this window just started a turn.
assert.deepEqual(indicatorState({ownerLive:true,action:'Reading a.md',now}),{mode:'running',label:'Reading a.md'});
assert.deepEqual(indicatorState({socketOpen:true,lastEventAt:now-1000,now}),{mode:'running',label:'Working'});
// Waiting on a coworker is not "working", even while the working flag is set.
assert.deepEqual(indicatorState({socketOpen:true,waitingFor:'Leon Lin',lastEventAt:now,now}),{mode:'waiting',label:'Waiting for Leon'});
assert.deepEqual(indicatorState({waitingPeer:true,now}),{mode:'waiting',label:'Waiting for a coworker'});
assert.deepEqual(indicatorState({waitingUser:true,now}),{mode:'waiting',label:'Waiting for you'});
// Idle: no indicator at all.
assert.deepEqual(indicatorState({now}),{mode:'idle',label:'',stale:false});
// Stale: a working flag nobody cleared, gateway idle, no activity for > STALE_MS.
const stale=indicatorState({socketOpen:true,lastEventAt:now-STALE_MS-1,now});
assert.equal(stale.mode,'idle');assert.equal(stale.stale,true);
// The gateway's live status is never overridden by staleness.
assert.equal(indicatorState({ownerLive:true,socketOpen:true,lastEventAt:now-10*STALE_MS,now}).mode,'running');
console.log('PASS: tool labels, running/waiting/idle, and stale working flag');
