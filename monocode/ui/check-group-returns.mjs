import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const source=readFileSync(new URL('./conversation.js',import.meta.url),'utf8');
function extract(name){
  const start=source.indexOf(`  function ${name}(`);
  assert.ok(start>=0,name);
  const end=source.indexOf('\n  function ',start+1);
  return source.slice(start,end);
}
const ctx={answerKey:value=>`answer:${value}`};
vm.createContext(ctx);
vm.runInContext(['runtimeFailureSummary','returnSucceeded','returnExplicitId','handoffExplicitId','handoffState'].map(extract).join('\n'),ctx);
vm.runInContext(extract('terminalPreservesUnfinished'),ctx);
assert.equal(ctx.terminalPreservesUnfinished({completion:'incomplete',final_markdown:'Saved draft.'}),true);
assert.equal(ctx.terminalPreservesUnfinished({completion:'unknown'}),true);
assert.equal(ctx.terminalPreservesUnfinished({completion:'canceled'}),true);
assert.equal(ctx.terminalPreservesUnfinished({completion:'completed',final_markdown:'Verified result.'}),false);
assert.equal(ctx.terminalPreservesUnfinished({final_markdown:'The provider became unavailable after this agent had already performed work.'}),true);
const failed={ok:true,status:'done',subject:'scribe turn failed',body:'The `scribe` agent could not complete its turn: usage limit reached'};
assert.equal(ctx.returnSucceeded(failed),false,'legacy success flag must not hide a failure');
assert.equal(ctx.runtimeFailureSummary(failed.body),'The provider reported a usage limit. Your progress is saved.');
assert.equal(ctx.returnSucceeded({ok:true,status:'done',body:'The report is ready.'}),true);
assert.equal(ctx.returnSucceeded({ok:true,status:'canceled',body:'Stopped.'}),false);
assert.equal(ctx.returnSucceeded({ok:true,body:'The provider became unavailable after this agent had already performed work.'}),false);
assert.equal(ctx.returnExplicitId({handoff_id:'assignment',causation_id:'parent-turn'}),ctx.handoffExplicitId({handoff_id:'assignment'}));
assert.equal(ctx.returnExplicitId({reply_to:'original',handoff_id:'delivery',causation_id:'parent'}),ctx.handoffExplicitId({handoff_id:'original'}));

// Exercise the actual return renderer with a small DOM surface. Full error
// content belongs only in the closed disclosure, never the summary or title.
const result={hidden:true,textContent:'',removeAttribute(){}};
let details;
const pre={textContent:''};
const copy={append(node){details=node;}};
const row={dataset:{turnId:'turn'},querySelector(selector){return ({'.handoff-result':result,'.handoff-error-details':details,'.handoff-copy':copy})[selector]||null;}};
Object.assign(ctx,{
  document:{createElement(){return {querySelector(){return pre;},remove(){details=null;}};}},
  internalReturnFailure:ctx.runtimeFailureSummary,
  setHandoffPresentation(_row,status,summary){row.dataset.handoffState=status;result.hidden=false;result.textContent=summary;},
  syncOwnerHandoffUpdate(){},
});
vm.runInContext(extract('applyReturnToHandoff'),ctx);
ctx.applyReturnToHandoff(row,failed);
assert.equal(row.dataset.handoffState,'blocked');
assert.equal(result.textContent,ctx.runtimeFailureSummary(failed.body));
assert.equal(pre.textContent,failed.body);
assert.ok(details.innerHTML.includes('Error details'));
assert.ok(!details.innerHTML.includes(' open'));
ctx.applyReturnToHandoff(row,{ok:true,status:'done',body:'A very long private coworker result'});
assert.equal(row.dataset.handoffState,'done');
assert.equal(result.hidden,true);
assert.equal(result.textContent,'');
assert.equal(details,null);
ctx.applyReturnToHandoff(row,{ok:false,status:'cancelled',body:'Stopped by user'});
assert.equal(row.dataset.handoffState,'canceled');
ctx.applyReturnToHandoff(row,{historical:true,ok:null,status:'returned',body:'Saved result, with verification still pending.'});
assert.equal(row.dataset.handoffState,'returned','a durable return is not promoted to successful completion');
assert.equal(result.hidden,true,'returned body remains behind the existing coworker disclosure');
ctx.applyReturnToHandoff(row,{historical:true,ok:null,status:'returned',subject:'Update',body:failed.body});
assert.equal(row.dataset.handoffState,'blocked','runtime failure disclosure wins over a generic recovered return');

Object.assign(ctx,{
  state:{attachments:[],mentions:[],groupEveryone:false,item:{kind:'group'},displayRows:[{role:'user',turn_id:'first'}]},
  knownAgentProfile:id=>({agent_id:id}),activatableCoworker:()=>true,
  composerImageCommentContext:()=>'',displayRole:entry=>entry.role,displayTurnId:entry=>entry.turn_id,
  initiatingAgentForTurn:()=> 'phoenix',composerGroupProfiles:()=>[{agent_id:'phoenix'}],
});
vm.runInContext(extract('composerRequest'),ctx);
assert.equal(ctx.composerRequest('So?').requestText,'@phoenix So?');
assert.equal(ctx.composerRequest('So?').displayText,'So?');
assert.equal(ctx.composerRequest('@nico review this').requestText,'@nico review this');
ctx.state.item={kind:'agent'};
assert.equal(ctx.composerRequest('So?').requestText,'So?');
console.log('PASS: failure status, correlation, compact disclosure, and hidden coworker results.');

// Actual failed live Blender run: the mesh wraps its error in an agent label.
const capturedFailure='**[coder]** The `coder` agent could not complete its turn: OpenAI Codex API error (429 Too Many Requests): {"error":{"type":"usage_limit_reached","message":"The usage limit has been reached"}}';
assert.match(ctx.runtimeFailureSummary(capturedFailure),/usage limit/);
assert.equal(ctx.returnSucceeded({ok:true,status:'done',body:capturedFailure}),false);
assert.match(ctx.runtimeFailureSummary('## Result\nAvery could not complete its provider-backed turn.\nError: 401'),/sign-in/);
for(const normal of ['Here is how to fix usage_limit_reached','The report documents a 401 error','```\nThe `coder` agent could not complete its turn: 401\n```'])assert.equal(ctx.runtimeFailureSummary(normal),null);
console.log('PASS: real wrapped provider failure and legacy fallback stay compact; ordinary error discussions remain answers.');

assert.match(ctx.runtimeFailureSummary('The provider became unavailable after work: OAuth token refresh returned HTTP 400: the provider rejected the saved login; sign in again'), /sign-in needs attention/);
assert.match(ctx.runtimeFailureSummary('The provider became unavailable after work: saved login rejected; not a usage limit; usage_limit_reached'), /available provider accounts/);
assert.match(ctx.runtimeFailureSummary('The provider became unavailable after work: sign in again (saved login rejected; not a usage limit)'), /Provider sign-in needs attention/);
