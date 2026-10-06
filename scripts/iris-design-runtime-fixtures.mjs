// Synthetic deterministic fixtures for engine tests. These are not agent output
// or visual acceptance evidence for a generated website.
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { deflateSync } from 'node:zlib';
import { dispatch } from './iris-design-runtime.mjs';

export function png(width = 32, height = 32, value = 80) {
  const chunk = (type, data) => {
    const bytes = Buffer.concat([Buffer.from(type), data]);
    let crc = 0xffffffff;
    for (const byte of bytes) {
      crc ^= byte;
      for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
    }
    const result = Buffer.alloc(data.length + 12);
    result.writeUInt32BE(data.length);
    bytes.copy(result, 4);
    result.writeUInt32BE((crc ^ 0xffffffff) >>> 0, result.length - 4);
    return result;
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width); header.writeUInt32BE(height, 4); header[8] = 8; header[9] = 2;
  const rows = Buffer.alloc((1 + width * 3) * height, value);
  for (let y = 0; y < height; y++) rows[y * (1 + width * 3)] = 0;
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]), chunk('IHDR', header), chunk('IDAT', deflateSync(rows)), chunk('IEND', Buffer.alloc(0))]);
}

export const brief = {
  originalRequest: 'Build an engine fixture website.', subject: 'Engine fixture', pageType: 'Website',
  scope: 'One responsive page', primaryGoal: 'Explain this fixture', audience: 'Test maintainers',
  offer: 'Deterministic testing', primaryAction: 'Read the fixture', creativeControl: 'Choose details autonomously',
  requiredContent: ['Introduction'], constraints: [], brandInputs: [], explicitAnswers: [], assumptions: [], unresolved: [],
};
export const briefing = { status: 'complete', message: 'Brief complete.', questions: [], brief };

export function brandFor(state) {
  return {
    version: 1,
    foundation: { strategy: 'create', existingAssets: [], assetActions: [], lockedDecisions: [], assumptions: [] },
    creativeDirection: {
      summary: 'A quiet type composition.', traits: [{ quality: 'precise', boundary: 'not ornate' }],
      productiveTension: 'Large type with compact explanation.',
      signatureDevice: { description: 'Offset title.', status: 'candidate', invariants: ['Left aligned title'] },
      restraint: 'Use one title.', avoid: ['Decorative filler'],
    },
    paletteRecipe: { themes: { light: { accentSeed: '#C1492E', neutralSeed: '#665A50', surfaceContrast: 'quiet' } } },
    typefaces: [{ family: state.typographyCandidates.sans[0], source: 'Fixture family declaration', roles: ['UI'], weights: [500] }],
    interfaceDirection: 'Open type composition with direct controls.',
    imageDirection: { summary: 'No illustration in this fixture.', subjects: [], treatment: 'None.', avoid: [] },
    motionDirection: { summary: 'Visible entrance.', principles: ['Explain title; load; title and body; 200ms; ease-out'], avoid: ['Repeated motion'] },
    voice: { summary: 'Direct.', avoid: ['Unproven claims'] },
  };
}
export function pageFor(state, assetNeeds = []) {
  return {
    version: 1, page: { title: 'Fixture', route: '/', description: 'An engine fixture.' },
    architecture: { contract: 'Explain the fixture.', mode: 'read_understand', novelty: 'low', grid: 'One reading column.', signatureRule: 'Offset title.', rhythm: 'Title then explanation.' },
    navigation: [],
    navigationDesign: { layoutCase: 'navigation-1', layout: 'Name at left.', behavior: [], transformation: { compact: 'Name.', medium: 'Name.', expanded: 'Name.' } },
    sections: [{
      id: 'hero', layoutFamily: 'hero', layoutCases: ['hero-text-5'],
      referenceDirectionId: state.referenceAttachments.length ? 'user-reference-1' : state.referenceDeck.find((entry) => entry.family === 'hero').id,
      purpose: 'Introduce the fixture.', userQuestion: 'What is this?', stage: 'orient', dependencies: [], evidence: [],
      copy: { heading: 'A test of structure.', body: ['Read the recorded result.'], callsToAction: [] },
      layout: 'Open column.',
      motion: { purpose: 'explanation', trigger: 'scroll_enter', behavior: 'Title arrives.', durationMs: 220, easing: 'ease-out', reducedMotion: 'Show final position.' },
      componentNeeds: [], assetNeeds,
      transformation: { compact: 'Narrow reading column.', medium: 'Reading column.', expanded: 'Open reading column.' },
    }],
    responsive: [], interactions: [], acceptanceCriteria: ['One heading.'],
  };
}
export const emptyAssets = { version: 1, assets: [] };
export const buildOutput = { status: 'complete', summary: 'Fixture implementation.', files: ['index.html'], checks: ['Synthetic fixture check'] };
export const previewPlan = { version: 1, kind: 'static', cwd: '.', entry: 'index.html', url: 'http://127.0.0.1:4173/', viewports: [
  { name: 'desktop', width: 1280, height: 800 }, { name: 'mobile', width: 390, height: 844 },
] };
export const passReview = { version: 1, verdict: 'pass', summary: 'Synthetic fixture review.', findings: [] };
export const repairReview = { version: 1, verdict: 'repair', summary: 'Synthetic spacing finding.', findings: [
  { id: 'spacing', severity: 'major', area: 'Hero', evidenceType: 'visual_inspection', confidence: 'high', evidence: 'Synthetic fixture discrepancy.', repair: 'Correct fixture spacing.' },
] };
export const html = '<!doctype html><html><head><title>Fixture</title></head><body><h1>Fixture</h1><p>Deterministic test content.</p></body></html>';

export async function createFixture(t, { automaticReferences = false, request = brief.originalRequest, beforeStart } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'iris-design-test-'));
  const workspace = path.join(root, 'workspace');
  mkdirSync(workspace);
  const reference = path.join(root, 'reference.png');
  writeFileSync(reference, png());
  if (beforeStart) beforeStart(workspace);
  const state = await dispatch({ action: 'start', request, workspace, references: automaticReferences ? [] : [reference] });
  // The caller owns cleanup so failed test evidence can be copied before removal.
  return { root, workspace, reference, state };
}
export async function step(state, output, expectedPhase) {
  const result = await dispatch({ action: 'advance', state, output });
  if (expectedPhase) assert.equal(result.phase, expectedPhase, JSON.stringify(result.error));
  assert.notEqual(result.status, 'failed', JSON.stringify(result.error));
  if (expectedPhase && result.phase !== state.phase) assert.equal(result.correcting, false);
  return result;
}
export async function toPhase(fixture, target, { assets = emptyAssets, assetNeeds = [] } = {}) {
  let state = fixture.state;
  if (target === 'brief') return state;
  state = await step(state, briefing, 'brand');
  if (target === 'brand') return state;
  state = await step(state, brandFor(state), 'page');
  if (target === 'page') return state;
  state = await step(state, pageFor(state, assetNeeds), 'assets');
  if (target === 'assets') return state;
  state = await step(state, assets, 'build');
  if (target === 'build') return state;
  writeFileSync(path.join(state.workspace, 'index.html'), html);
  state = await step(state, buildOutput, 'preview');
  if (target === 'preview') return state;
  state = await step(state, previewPlan, 'preview');
  if (target === 'capture') return state;
  return submitCaptures(fixture, state);
}
export async function submitCaptures(fixture, state, audit = { h1Count: 1, interactiveTargetViolations: [] }) {
  const screenshots = previewPlan.viewports.map((viewport, index) => {
    const file = path.join(fixture.root, `capture-${state.revision}-${index}.png`);
    writeFileSync(file, png(32, 32, 90 + index));
    return { path: file, width: viewport.width, height: viewport.height, domAudit: audit,
      captureHeight: 3200, fullDocument: true };
  });
  return dispatch({ action: 'capture', state, previewUrl: previewPlan.url, screenshots });
}
