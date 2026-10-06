import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {auditBlenderTools, currentBlenderTurn, plainNativeContinuation, nativeCompletionChecks} from './lib/blender-tool-audit.mjs';

const receipt = input => ({type:'ToolResult', tool_name:'computer_window_act', input:JSON.stringify(input), success:true});
const longBatch = receipt({id:1, actions:[
  {type:'type', text:'Long display targets omit later actions. '.repeat(10)},
  {type:'key', combo:'F4 + Shift'},
]});
assert.equal(auditBlenderTools([longBatch],1).audit.prohibited_console_shortcut,true);
assert.equal(auditBlenderTools([receipt({combo:'shift+f4'})],1).audit.prohibited_console_shortcut,true);
const clean = auditBlenderTools([receipt({actions:[{type:'key',combo:'shift+alt+z'}]})],1).audit;
assert.equal(clean.complete,true);
assert.equal(clean.prohibited_console_shortcut,false);
const missing = auditBlenderTools([],1).audit;
assert.equal(missing.complete,false);
assert.equal(missing.prohibited_console_shortcut,null);
assert.equal(missing.all_tools_allowed,null);
assert.equal(auditBlenderTools([{...longBatch,input:'truncated...'}],1).audit.complete,false);
assert.equal(auditBlenderTools([{...longBatch,tool_name:'exec_command'}],1).audit.all_tools_allowed,false);
console.log('PASS: full late-action input, direct keys, incomplete evidence, malformed inputs and forbidden tools');

// Reproduce the real resumed run: one execution and a rejected next batch.
const feedback={type:'ToolResult',tool_name:'response_validation',input:'{}',success:false,output:'BUILD CONTRACT: plan required'};
const gated=auditBlenderTools([receipt({}),feedback],1);
assert.equal(gated.audit.complete,true);
assert.equal(gated.audit.receipt_count,1);
assert.equal(gated.audit.failed_validation_events,1);
assert.deepEqual(gated.validations,[feedback]);
assert.equal(auditBlenderTools([receipt({}),feedback],2).audit.complete,false,'validation must not cover a missing executed receipt');
console.log('PASS: admission feedback retained separately, execution completeness stays strict');

const priorReceipt={type:'ToolResult',tool_name:'bash',input:'{}',success:true,output:'old test'};
const newReceipt={type:'ToolResult',tool_name:'computer_screenshot',input:'{}',success:true,output:'new test'};
const resumed=currentBlenderTurn([{type:'User',content:'old'},priorReceipt,{type:'User',content:'resume'},newReceipt],'resume');
assert.deepEqual(resumed,[newReceipt]);
assert.equal(auditBlenderTools(resumed,1).audit.all_tools_allowed,true);
assert.throws(()=>currentBlenderTurn([priorReceipt],'missing'),/boundary is missing/);
console.log('PASS: resumed-turn audit excludes historical calls and requires its exact boundary');

for(const name of ['design_reference','skill_search','skill_install','skill','read','list_directory'])
  assert.equal(auditBlenderTools([{...newReceipt,tool_name:name,input:'{}'}],1).audit.all_tools_allowed,true,
    `${name}: learning/inspection alone is not scripted scene construction`);
for(const name of ['bash','write','str_replace','browser_evaluate','execute_blender_code'])
  assert.equal(auditBlenderTools([{...newReceipt,tool_name:name,input:'{}'}],1).audit.all_tools_allowed,false);
const harness=readFileSync(new URL('./live-blender-ui.mjs',import.meta.url),'utf8');
const brief=harness.match(/\]\.join\('\\n'\) : '([^']+)'/);
assert.ok(brief,'plain native request must remain separate from legacy guidance');
assert.ok(brief[1].length<230,'short task brief, not a construction recipe');
assert.match(brief[1],/application interface, not scripts, code or modeling APIs/);
assert.doesNotMatch(brief[1],/curvat|taper|yellow|stem|bevel|shader|cylinder|mesh|roughness/i);
assert.match(harness,/plainNativeContinuation\(/,'guided history is checked before entering a plain native continuation');
assert.match(harness,/\.\.\.auth, preserveState:true/,'use the selected Phoenix account source');
console.log('PASS: learning versus code execution, short method-only task brief, guided-history exclusion and selected account routing.');

const stopped={execution:{gateway:{code:0},credentialRemoved:true,durableState:'/fixture/durable-state',cleanupFailures:[]},
  setup:{mode:'plain-reference-native-ui',nativeGuiRequired:true,observerCoaching:false,prompt:brief[1]},
  metrics:{terminal:{Done:{completion:'incomplete',background_work_pending:false}}}};
const continued=plainNativeContinuation(stopped,['_wrongly_named_native_draft.blend','draft.blend1','banana-reference.jpg']);
assert.equal(continued.sceneName,'_wrongly_named_native_draft.blend');
assert.equal(continued.originalPrompt,brief[1]);
assert.equal(continued.prompt,'Continue the existing task to completion using its saved work and original requirements.');
assert.equal(plainNativeContinuation(stopped,['banana.blend','old.blend']).sceneName,'banana.blend');
for(const scenes of [[],['a.blend','b.blend'],['../escape.blend'],['draft.blend1']])
  assert.throws(()=>plainNativeContinuation(stopped,scenes),/unambiguous/);
for(const changed of [
  {...stopped,setup:{...stopped.setup,observerCoaching:true}},
  {...stopped,setup:{...stopped.setup,mode:'legacy-guided-ui'}},
  {...stopped,execution:{...stopped.execution,credentialRemoved:false}},
  {...stopped,execution:{...stopped.execution,cleanupFailures:[{error:'failed'}]}},
  {...stopped,metrics:{terminal:{Done:{background_work_pending:true}}}},
  {...stopped,metrics:{}},
])assert.throws(()=>plainNativeContinuation(changed,['banana.blend']));
console.log('PASS: plain native continuation preserves the original short task, accepts an unambiguous misnamed draft, rejects guided/active/ambiguous recovery, and supplies no repair recipe.');

const completed={terminal:{Done:{completion:'completed'}},cancelSent:false,tool_input_audit_complete:true,
  all_tools_allowed:true,prohibited_console_shortcut:false,blend_bytes:1024,preview_bytes:1024};
assert.ok(Object.values(nativeCompletionChecks(completed)).every(Boolean));
for(const changed of [
  {...completed,terminal:{Done:{completion:'incomplete'}}},
  {...completed,terminal:{Error:{message:'stopped'}}},
  {...completed,cancelSent:true},{...completed,preview_bytes:0},{...completed,blend_bytes:0},
  {...completed,all_tools_allowed:false},{...completed,tool_input_audit_complete:false},
  {...completed,prohibited_console_shortcut:true},
])assert.ok(Object.values(nativeCompletionChecks(changed)).some(value=>value===false));
console.log('PASS: quota-stopped drafts, missing previews, interrupted work and invalid input audits cannot pass native completion.');
