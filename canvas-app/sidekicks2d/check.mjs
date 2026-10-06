import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const EXPECTED_FAMILIES = Object.freeze([
  'classic_flame',
  'ember_orb',
  'shard_flame',
  'split_flame',
  'halo_core',
  'smoke_wisp',
]);
const EYES = Object.freeze(['round', 'spark', 'slit', 'visor', 'closed']);
const EXPRESSIONS = Object.freeze([
  'bright',
  'joy',
  'calm',
  'curious',
  'mischief',
  'sleepy',
  'focused',
  'determined',
]);

function defsIds(svg) {
  return [...svg.matchAll(/\bid=(["'])([^"']+)\1/g)].map((match) => match[2]);
}

for (const familyId of EXPECTED_FAMILIES) {
  const module = await import(`./families/${familyId}.mjs?check=${Date.now()}-${familyId}`);
  assert.equal(module.id ?? module.metadata?.id ?? module.default?.id, familyId, `${familyId}: exported id`);
  const render = module.render ?? module.default?.render;
  assert.equal(typeof render, 'function', `${familyId}: render function`);

  let count = 0;
  for (const fiery of [true, false]) {
    for (const eyeStyle of EYES) {
      for (const expression of EXPRESSIONS) {
        const uid = `check-${familyId}-${eyeStyle}-${expression}-${fiery}`;
        const svg = String(render({ color:'#e55732', fiery, eyeStyle, expression, uid }));
        assert.match(svg, /^<svg\b/i, `${familyId}: complete SVG`);
        assert.match(svg, /<\/svg>\s*$/i, `${familyId}: closes SVG`);
        assert.ok(svg.length > 350, `${familyId}: nontrivial procedural markup`);
        assert.doesNotMatch(svg, /<image\b|data:image\//i, `${familyId}: no external/generated image dependency`);
        const ids = defsIds(svg);
        assert.equal(new Set(ids).size, ids.length, `${familyId}: unique SVG ids within a render`);
        count += 1;
      }
    }
  }
  assert.equal(count, 80, `${familyId}: full state matrix`);

  const first = String(render({ color:'#e55732', fiery:true, eyeStyle:'visor', expression:'focused', uid:`${familyId}-one` }));
  const second = String(render({ color:'#e55732', fiery:true, eyeStyle:'visor', expression:'focused', uid:`${familyId}-two` }));
  const overlap = defsIds(first).filter((id) => defsIds(second).includes(id));
  assert.deepEqual(overlap, [], `${familyId}: uid scopes SVG defs across instances`);
  console.log(`${familyId}: 80 variants + uid isolation OK`);
}

console.log('sidekicks2d: all family contracts OK');

const [sidebar, index, css, runtime, rust] = await Promise.all([
  readFile(new URL('../ui/sidebar.js', import.meta.url), 'utf8'),
  readFile(new URL('../ui/index.html', import.meta.url), 'utf8'),
  readFile(new URL('../ui/sidekicks.css', import.meta.url), 'utf8'),
  readFile(new URL('./runtime.mjs', import.meta.url), 'utf8'),
  readFile(new URL('../../src/runtime/company_control.rs', import.meta.url), 'utf8'),
]);

for (const familyId of EXPECTED_FAMILIES) {
  assert.ok(sidebar.includes(familyId), `sidebar exposes ${familyId}`);
  assert.ok(runtime.includes(familyId), `runtime registers ${familyId}`);
  assert.ok(rust.includes(`"${familyId}"`), `Rust validates ${familyId}`);
}

for (const expression of EXPRESSIONS) {
  assert.ok(sidebar.includes(expression), `sidebar supports expression ${expression}`);
  assert.ok(rust.includes(`"${expression}"`), `Rust validates expression ${expression}`);
}

for (const eyeStyle of EYES) {
  assert.ok(sidebar.includes(eyeStyle), `sidebar supports eye style ${eyeStyle}`);
  assert.ok(rust.includes(`"${eyeStyle}"`), `Rust validates eye style ${eyeStyle}`);
}

assert.match(sidebar, />Fire forms<\/button>/, 'procedural avatar mode is user-visible');
assert.match(sidebar, /data-sidekick-fiery="true"/, 'fiery toggle is user-visible');
assert.match(sidebar, /data-sidekick-fiery="false"/, 'clean-form toggle is user-visible');
assert.doesNotMatch(sidebar, /3D sidekick|live 3D avatar|Open model studio|sidekick-studio\.html/, 'old 3D editor is not in production sidebar');
assert.match(index, /sidekicks2d-bundle\.js/, '2D sidekick bundle is loaded');
assert.doesNotMatch(index, /sidekicks-bundle\.js|sidekick-studio\.js/, 'old 3D sidekick bundle is not loaded');
assert.match(css, /\.phoenix-sidekick2d\b/, '2D avatar surface is styled');
assert.doesNotMatch(css, /iframe/, '2D editor has no model-studio iframe styling');
assert.match(rust, /pub family_id: Option<String>/, 'procedural family persists');
assert.match(rust, /pub fiery: bool/, 'fiery state persists');
assert.match(rust, /pub eye_style: String/, 'eye style persists');

console.log('sidekicks2d: production integration contract OK');
