import * as classicFlame from './families/classic_flame.mjs';
import * as emberOrb from './families/ember_orb.mjs';
import * as shardFlame from './families/shard_flame.mjs';
import * as splitFlame from './families/split_flame.mjs';
import * as haloCore from './families/halo_core.mjs';
import * as smokeWisp from './families/smoke_wisp.mjs';

const EXPECTED_IDS = Object.freeze([
  'classic_flame',
  'ember_orb',
  'shard_flame',
  'split_flame',
  'halo_core',
  'smoke_wisp',
]);

export const EYE_STYLES = Object.freeze(['round', 'spark', 'slit', 'visor', 'closed']);
export const EXPRESSIONS = Object.freeze([
  'bright',
  'joy',
  'calm',
  'curious',
  'mischief',
  'sleepy',
  'focused',
  'determined',
]);

export const LEGACY_FAMILY = Object.freeze({
  ember: 'ember_orb',
  cinder: 'smoke_wisp',
  kiln: 'shard_flame',
  wisp: 'halo_core',
});

const MODULES = [
  classicFlame,
  emberOrb,
  shardFlame,
  splitFlame,
  haloCore,
  smokeWisp,
];

function descriptor(module, expectedId) {
  const metadata = module.metadata || module.family || module.default?.metadata || {};
  const id = String(metadata.id || module.id || expectedId);
  const label = String(metadata.label || module.label || id.replaceAll('_', ' '));
  const render = module.render || module.renderFamily || module.default?.render ||
    (typeof module.default === 'function' ? module.default : null);
  if (id !== expectedId) throw new Error(`2D sidekick family id mismatch: expected ${expectedId}, received ${id}`);
  if (typeof render !== 'function') throw new Error(`2D sidekick family ${id} does not export a render function`);
  return Object.freeze({ id, label, render });
}

export const FAMILIES = Object.freeze(MODULES.map((module, index) => descriptor(module, EXPECTED_IDS[index])));
export const FAMILY_IDS = Object.freeze(FAMILIES.map(({ id }) => id));
const FAMILY_BY_ID = new Map(FAMILIES.map((family) => [family.id, family]));

function validColor(value) {
  const color = typeof value === 'string' ? value : '';
  return /^#[0-9a-f]{6}$/i.test(color) ? color.toLowerCase() : '#e55732';
}

function safeUid(value) {
  return String(value || 'sidekick').replace(/[^a-zA-Z0-9_-]/g, '').slice(0, 72) || 'sidekick';
}

function withRuntimeClass(svg, familyId, fiery) {
  const end = svg.indexOf('>');
  if (end < 0) throw new Error(`2D sidekick family ${familyId} returned malformed SVG`);
  let opening = svg.slice(0, end + 1);
  const rest = svg.slice(end + 1);
  const runtimeClasses = `phoenix-sidekick2d family-${familyId}${fiery ? ' is-fiery' : ' is-clean'}`;
  const classPattern = /\sclass=(["'])(.*?)\1/i;
  if (classPattern.test(opening)) {
    opening = opening.replace(classPattern, (_match, quote, classes) => ` class=${quote}${runtimeClasses} ${classes}${quote}`);
  } else {
    opening = opening.replace(/^<svg\b/i, `<svg class="${runtimeClasses}"`);
  }
  if (!/\sdata-sidekick-family=/i.test(opening)) {
    opening = opening.replace(/^<svg\b/i, `<svg data-sidekick-family="${familyId}" data-fiery="${fiery}"`);
  }
  if (!/\saria-hidden=/i.test(opening)) opening = opening.replace(/^<svg\b/i, '<svg aria-hidden="true"');
  return opening + rest;
}

export function render(options = {}) {
  const familyId = FAMILY_BY_ID.has(options.familyId) ? options.familyId : FAMILY_IDS[0];
  const family = FAMILY_BY_ID.get(familyId);
  const fiery = options.fiery !== false;
  const eyeStyle = EYE_STYLES.includes(options.eyeStyle) ? options.eyeStyle : 'round';
  const expression = EXPRESSIONS.includes(options.expression) ? options.expression : 'bright';
  const uid = safeUid(options.uid);
  const output = String(family.render({
    color: validColor(options.color),
    fiery,
    eyeStyle,
    expression,
    uid,
  }) || '').trim();
  if (!output) throw new Error(`2D sidekick family ${familyId} returned empty SVG markup`);
  if (/^<svg\b/i.test(output)) return withRuntimeClass(output, familyId, fiery);
  return `<svg class="phoenix-sidekick2d family-${familyId}${fiery ? ' is-fiery' : ' is-clean'}" data-sidekick-family="${familyId}" data-fiery="${fiery}" viewBox="0 0 64 64" aria-hidden="true">${output}</svg>`;
}

export function familyForLegacyModel(modelId) {
  return LEGACY_FAMILY[modelId] || null;
}

const api = Object.freeze({ FAMILIES, FAMILY_IDS, EYE_STYLES, EXPRESSIONS, LEGACY_FAMILY, render, familyForLegacyModel });
if (typeof window !== 'undefined') window.PhoenixSidekicks2D = api;

