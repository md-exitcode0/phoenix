import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {creativeScenarios,creativeScenario,creativeArtifactChecks} from './lib/creative-scenarios.mjs';

const recipe=creativeScenario('banana');
assert.equal(recipe.prompt,'Make me a realistic banana in Blender using banana-reference.jpg. Save the editable project and a polished render in this workspace.');
assert.equal(recipe.reference,'artifacts/banana-reference/banana-reference.jpg');
assert.ok(readFileSync(new URL('../'+recipe.reference,import.meta.url)).length>10000);
assert.throws(()=>creativeScenario('__proto__'),/Unknown creative scenario/);
assert.throws(()=>creativeScenario('missing'),/Unknown creative scenario/);
for(const [name,task] of Object.entries(creativeScenarios)) {
  assert.ok(Object.isFrozen(task));
  assert.equal(task.actor,task.kind==='website'?'frontend':'coder');
  assert.ok(Object.values(creativeArtifactChecks(task,[])).every(value=>value===false),`${name}: no absent-artifact pass`);
  if(task.kind==='website') {
    for(const text of ['Theo','Leo','index.html','motion']) assert.ok(task.prompt.includes(text),`${name}: ${text}`);
    assert.equal(creativeArtifactChecks(task,[{path:'nested/index.html',bytes:2000}]).indexExists,false);
    assert.equal(creativeArtifactChecks(task,[{path:'index.html',bytes:2000}]).indexExists,true);
  } else {
    const gates=creativeArtifactChecks(task,[{path:'object.blend',bytes:20000},{path:'render.png',bytes:40000}]);
    assert.deepEqual(gates,{editableScene:true,renderedOutput:true});
    assert.deepEqual(creativeArtifactChecks(task,[{path:'object.blend',bytes:0},{path:'reference.jpg',bytes:40000}]),{editableScene:false,renderedOutput:false});
  }
}
const festival=creativeScenario('festival'), printshop=creativeScenario('printshop');
assert.equal(festival.name,'Switchyard');
assert.equal(printshop.name,'Overprint');
assert.notEqual(festival.prompt,printshop.prompt);
for(const text of ['kinetic festival poster','eight fictional events','two days and three stages','overlapping events','persists on reload'])
  assert.ok(festival.prompt.includes(text),`festival outcome preserved: ${text}`);
for(const text of ['print-making workbench','live two-ink poster preview','six original poster compositions','quantity','itemised demo price'])
  assert.ok(printshop.prompt.includes(text),`printshop outcome preserved: ${text}`);
// Brief differences are input coverage, never proof of visual diversity. Both
// actual outputs must still be inspected and their interactions exercised.
console.log(`PASS: ${Object.keys(creativeScenarios).length} ordinary creative briefs, unchanged plain-reference banana, distinct new task structures, scoped actor selection, required peers, and missing-artifact negative controls. No model task executed or visual quality established.`);
