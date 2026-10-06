// Lightweight seam tests: synthetic player/manifest fixtures, no real atlases,
// browser, Blender, gateway or model calls. This does not verify character art.
import assert from 'node:assert/strict';
import {createConversationCompanion} from '../canvas-app/ui/conversation-companion.mjs';

const flush = async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); };
const host = {children:['existing-avatar']}, instances = [], fallbacks = [], errors = [];
function syntheticPlayer(container, options) {
  let resolve, reject, ready;
  const load = () => { ready = new Promise((yes, no) => { resolve = yes; reject = no; }); };
  load();
  let state = options.state, character = options.character.id, destroyed = false;
  const element = {hidden:false}, calls = [];
  const alive = () => assert.equal(destroyed, false, 'no setters after destruction');
  const api = Object.freeze({element, calls, get ready() { return ready; }, get state() { return state; },
    setState(value) { alive(); calls.push(['state',value]); if (value !== state) { state = value; load(); } return api; },
    setCharacter(value) { alive(); character = value.id; calls.push(['character',character]); load(); return api; },
    setSize(value) { alive(); calls.push(['size',value]); return api; },
    setMotion(value) { alive(); calls.push(['motion',value]); return api; },
    setPaused(value) { alive(); calls.push(['paused',value]); return api; },
    refresh() { alive(); calls.push(['refresh']); return api; },
    destroy() { if (destroyed) return; destroyed = true; calls.push(['destroy']); container.children.splice(container.children.indexOf(element),1); },
    finish() { resolve(); }, fail() { reject(new Error('SYNTHETIC asset failure')); },
    get destroyed() { return destroyed; },
  });
  container.children.push(element); instances.push(api); return api;
}
const seam = createConversationCompanion({host,mountCompanion:syntheticPlayer,catalog:[{id:'fluffy',manifest:{id:'fluffy',synthetic:true}},{id:'mantis',manifest:{id:'mantis',synthetic:true}}],onFallback:value=>fallbacks.push(value),onError:error=>errors.push(error.message)});
const selected = {ownerId:'phoenix',conversationKey:'agent:phoenix',turnId:'turn-1'};
let lease = seam.select(selected);
assert.equal(instances.length,0,'existing avatar is the default');
assert.deepEqual(host.children,['existing-avatar']);
seam.setCharacter('fluffy'); const first = instances.at(-1);first.finish();await flush();assert.equal(first.element.hidden,false);
const event = (kind,more={}) => ({...selected,live:true,kind,...more});
const send = (kind,more={}) => seam.accept(lease,event(kind,more));
assert.equal(send('tool-start',{operationId:'browser-a',activity:'browsing'}),true);
assert.equal(send('tool-start',{operationId:'code-a',activity:'coding'}),true);
assert.equal(seam.snapshot().toolCount,2);assert.equal(seam.snapshot().state,'coding');
send('tool-start',{operationId:'code-a',activity:'coding'});assert.equal(seam.snapshot().toolCount,2,'duplicate starts do not inflate activity');
assert.equal(send('tool-end',{operationId:'unknown'}),false);assert.equal(seam.snapshot().toolCount,2);
assert.equal(seam.accept(lease,event('tool-start',{operationId:'wrong-thread',activity:'coding',conversationKey:'other-thread'})),false);
send('tool-end',{operationId:'code-a'});assert.equal(seam.snapshot().state,'browsing');
send('ask-open',{askId:'approval-a'});send('ask-open',{askId:'question-a'});assert.equal(seam.snapshot().state,'waiting');
send('ask-close',{askId:'question-a'});assert.equal(seam.snapshot().state,'waiting');
assert.equal(seam.accept(lease,event('turn-ended',{ownerId:'peer',status:'completed',success:true})),false);
assert.equal(seam.accept(lease,event('tool-start',{operationId:'replay',activity:'coding',live:false})),false);
assert.equal(seam.accept(lease,event('narration',{text:'I finished coding successfully.'})),false);
send('turn-ended',{status:'completed',success:true});assert.equal(seam.snapshot().state,'waiting','input outranks completion');
send('ask-close',{askId:'approval-a'});assert.equal(seam.snapshot().state,'idle','closing input does not invent a late success');
send('status',{status:'queued'});assert.equal(seam.snapshot().state,'waiting');
send('turn-ended',{status:'stopped'});assert.equal(seam.snapshot().state,'idle');
for (const status of ['stopped','interrupted','failed','completed']) {
  send('turn-ended',{status,success:true});
  const state = seam.snapshot().state;
  assert.equal(send('tool-start',{operationId:`late-${status}`,activity:'browsing'}),false,'late tool start cannot revive an ended turn');
  assert.equal(seam.snapshot().toolCount,0);assert.equal(seam.snapshot().state,state);
}
send('status',{status:'working'});
assert.equal(send('tool-start',{operationId:'resumed-code',activity:'coding'}),true,'explicit accepted continuation permits fresh work');
assert.equal(seam.snapshot().state,'coding');
send('tool-end',{operationId:'resumed-code'});assert.equal(seam.snapshot().state,'idle');
send('turn-ended',{status:'completed'});assert.equal(seam.snapshot().state,'idle','ambiguous completion is not success');
send('turn-ended',{status:'failed'});assert.equal(seam.snapshot().state,'error');
send('turn-ended',{status:'completed',success:true});assert.equal(seam.snapshot().state,'success');
const terminalReady = first.ready;send('turn-ended',{status:'completed',success:true});assert.equal(first.ready,terminalReady,'duplicate completion does not replay an accent');
seam.setCharacter('mantis');assert.equal(seam.snapshot().state,'idle','selecting experimental mantis does not replay success');
seam.setMinimal(true);seam.setPaused(true);seam.setSize(96);
assert.ok(first.calls.some(call=>call[0]==='motion'&&call[1]==='reduce'));
const oldLease = lease; lease = seam.select({ownerId:'researcher',conversationKey:'agent:researcher',turnId:'turn-2'});
assert.equal(first.destroyed,true);assert.equal(instances.length,1,'new owner keeps existing choice unless explicitly selected');
assert.equal(seam.accept(oldLease,event('turn-ended',{status:'completed',success:true})),false);
seam.setCharacter('fluffy');const loading = instances.at(-1);
lease = seam.select({ownerId:'coder',conversationKey:'agent:coder',turnId:'turn-3',characterId:'mantis'});const latest = instances.at(-1);
loading.fail();await flush();assert.equal(errors.length,0,'stale asset failure is not attributed to the new owner');assert.equal(loading.destroyed,true);assert.equal(latest.element.hidden,true,'stale load cannot reveal current slot');
latest.finish();await flush();assert.equal(latest.element.hidden,false);
const beforeNewTurn = lease;lease = seam.select({ownerId:'coder',conversationKey:'agent:coder',turnId:'turn-4',characterId:'mantis'});
assert.equal(seam.accept(beforeNewTurn,{ownerId:'coder',conversationKey:'agent:coder',turnId:'turn-3',live:true,kind:'turn-ended',status:'completed',success:true}),false);
assert.equal(instances.at(-1),latest,'a new turn reuses its selected owner controller');
assert.equal(seam.snapshot().state,'idle');
seam.setCharacter(null);assert.equal(latest.destroyed,true);assert.deepEqual(host.children,['existing-avatar']);
seam.setCharacter('fluffy');instances.at(-1).fail();await flush();assert.equal(seam.snapshot().mounted,false);assert.equal(errors.length,1);assert.deepEqual(host.children,['existing-avatar']);
assert.equal(seam.snapshot().state,'idle','asset failure is not a runtime failure');
seam.destroy();seam.destroy();assert.throws(()=>seam.setCharacter('mantis'),/destroyed/);
assert.ok(instances.every(instance=>instance.destroyed));
assert.equal(fallbacks.at(-1),true);
console.log('PASS synthetic companion seam: owner/turn isolation, activity/input states, character choices, readiness and teardown. No character art or integrated UI claimed.');
