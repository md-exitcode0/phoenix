import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { checkPackage, dispatch, MAX_WIRE_BYTES, runtimeRoot } from './iris-design-runtime.mjs';
import * as upstream from '../vendor/tastecode-design/dist/index.js';
import { DESIGN_FONT_POOLS } from '../vendor/tastecode-design/dist/typography.js';
import { createFixture, toPhase, step, submitCaptures, briefing, brandFor, pageFor, buildOutput, previewPlan, passReview, repairReview, html, png } from './iris-design-runtime-fixtures.mjs';

async function fixture(t, options) {
  const f = await createFixture(t, options);
  t.after(() => rmSync(f.root, { recursive: true, force: true }));
  return f;
}

test('complete phase sequence uses upstream prompts and only accepts a passing review', async (t) => {
  const f = await fixture(t);
  let state = f.state;
  assert.ok((await dispatch({ action: 'context', state })).prompt.endsWith(upstream.designBriefingPrompt(state.request)));
  state = await step(state, briefing, 'brand');
  const fontDraw = structuredClone(state.typographyCandidates);
  let ctx = await dispatch({ action: 'context', state });
  assert.ok(ctx.prompt.endsWith(upstream.designBrandPrompt(state.approvedBrief, state.referenceAttachments, fontDraw)));
  assert.deepEqual(ctx.attachments, [f.reference]);
  state = await step(state, brandFor(state), 'page');
  assert.equal(state.approvedBrand.colorPalette.length, 12);
  state = await step(state, pageFor(state), 'assets');
  state = await step(state, { version: 1, assets: [] }, 'build');
  writeFileSync(path.join(f.workspace, 'index.html'), html);
  state = await step(state, buildOutput, 'preview');
  state = await step(state, previewPlan, 'preview');
  ctx = await dispatch({ action: 'context', state });
  assert.equal(ctx.kind, 'capture'); assert.equal(ctx.prompt, null);
  state = await submitCaptures(f, state);
  assert.equal(state.phase, 'review');
  ctx = await dispatch({ action: 'context', state });
  assert.equal(ctx.attachments.length, 3);
  assert.ok(ctx.prompt.endsWith(upstream.designReviewPrompt(state.approvedBrief, state.approvedBrand, state.approvedPage, state.screenshots, state.referenceAttachments, [])));
  state = await step(state, passReview, 'complete');
  ctx = await dispatch({ action: 'context', state });
  assert.equal(ctx.accepted, true); assert.equal(ctx.outcome, 'passed'); assert.equal(ctx.kind, 'terminal');
  assert.deepEqual(state.typographyCandidates, fontDraw);
});

test('automatic reference library and randomized font draws persist across JSON restart', async (t) => {
  const f = await fixture(t, { automaticReferences: true });
  const state = await step(f.state, briefing, 'brand');
  // Upstream selects three feature and two about compositions, plus one per
  // remaining family (fourteen); Phoenix adds two more hero options so Page
  // can choose the identity that fits the brief.
  assert.equal(state.referenceDeck.length, 16);
  assert.equal(state.referenceDeck.filter((entry) => entry.family === 'hero').length, 3);
  assert.equal(new Set(state.referenceDeck.map((entry) => entry.group)).size, state.referenceDeck.length);
  assert.ok(state.referenceDeckSnapshot.length >= state.referenceDeck.length);
  const before = await dispatch({ action: 'context', state });
  const after = await dispatch({ action: 'context', state: JSON.parse(JSON.stringify(state)) });
  assert.deepEqual(before, after);
  assert.ok(before.prompt.includes('<selected-reference-workflow version="0.5">'));
  for (const [category, families] of Object.entries(state.typographyCandidates)) {
    assert.equal(families.length, 10);
    assert.deepEqual([...families].sort(), [...DESIGN_FONT_POOLS[category]].sort());
  }
  const candidates = upstream.loadReviewedReferences();
  const deck1 = upstream.selectReviewedReferences(state.approvedBrief, candidates, () => 0);
  const deck2 = upstream.selectReviewedReferences(state.approvedBrief, candidates, () => 0);
  assert.deepEqual(deck1, deck2);
});

test('font choice outside the persisted first draw requires an explicit user requirement', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'brand');
  const invalid = brandFor(state);
  invalid.typefaces[0].family = 'Undrawn Fixture Font';
  state = await step(state, invalid, 'brand');
  assert.equal(state.correcting, true);
  assert.match(state.error.message, /first-draw/);
  const context = await dispatch({ action: 'context', state });
  for (const [category, families] of Object.entries(state.typographyCandidates)) {
    assert.match(context.prompt, new RegExp(`${category}: ${families[0].replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`));
  }
  assert.match(context.prompt, /do not repeat the rejected family/i);
  state = await step(state, brandFor(state), 'page');
  assert.equal(state.correcting, false);
});

test('malformed briefing gets one correction, then an inspectable failed state', async (t) => {
  const f = await fixture(t);
  let state = await step(f.state, '{invalid', 'brief');
  assert.equal(state.correcting, true);
  state = await dispatch({ action: 'advance', state, output: '{invalid' });
  assert.equal(state.status, 'failed');
  const ctx = await dispatch({ action: 'context', state });
  assert.equal(ctx.kind, 'terminal'); assert.equal(ctx.error.code, 'PHASE_FAILED');
  await assert.rejects(dispatch({ action: 'advance', state, output: briefing }), /not awaiting model output/);
});

test('autonomous briefing refuses questions and unrelated tasks have an explicit terminal outcome', async (t) => {
  const f = await fixture(t);
  const question = { status: 'questions', message: 'Clarify.', brief: null, questions: [{ id: 'subject', header: 'Subject', question: 'What subject?', allowOther: true, options: [{ label: 'One', description: 'First option' }] }] };
  const corrected = await step(f.state, question, 'brief');
  assert.match(corrected.error.message, /autonomous/);
  const state = await dispatch({ action: 'advance', state: f.state, output: { status: 'not_design', message: 'Unrelated request.', questions: [], brief: null } });
  const ctx = await dispatch({ action: 'context', state });
  assert.equal(ctx.outcome, 'not_design'); assert.equal(ctx.accepted, false);
});

test('planning permits three distinct schema corrections and rejects a fourth or duplicate', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'brand');
  const base = brandFor(state);
  const invalids = ['foundation', 'creativeDirection', 'typefaces', 'imageDirection'].map((field) => ({ ...base, [field]: null }));
  for (const invalid of invalids.slice(0, 3)) {
    state = await step(state, invalid, 'brand');
    assert.equal(state.correcting, true);
  }
  assert.equal(state.correctionErrors.length, 3);
  const failed = await dispatch({ action: 'advance', state, output: invalids[3] });
  assert.equal(failed.status, 'failed');
  const duplicate = await dispatch({ action: 'advance', state, output: invalids[0] });
  assert.equal(duplicate.status, 'failed');
});

test('page reference IDs stay frozen and invalid palette recipes are not accepted', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'brand');
  const brand = brandFor(state);
  brand.paletteRecipe.themes.light.accentSeed = 'not-a-color';
  const invalidBrand = await step(state, brand, 'brand');
  assert.ok(invalidBrand.error);
  state = await step(state, brandFor(state), 'page');
  const page = pageFor(state); page.sections[0].referenceDirectionId = 'invented-reference';
  state = await step(state, page, 'page');
  assert.match(state.error.message, /referenceDirectionId/);
});

test('approved artifact edits are restored atomically, preserving unrelated files', async (t) => {
  const f = await fixture(t);
  const state = await toPhase(f, 'brand');
  const unrelated = path.join(f.workspace, 'keep.txt'); writeFileSync(unrelated, 'preserve');
  writeFileSync(path.join(f.workspace, '.taste/brief.json'), '{}');
  await assert.rejects(dispatch({ action: 'context', state }), /restored brief.json/);
  assert.deepEqual(upstream.readDesignBrief(f.workspace), state.approvedBrief);
  assert.equal(readFileSync(unrelated, 'utf8'), 'preserve');
  await dispatch({ action: 'context', state });
});

test('current upstream write-failure correction ordering stays bounded in brand, page and assets', async (t) => {
  for (const phase of ['brand', 'page', 'assets']) {
    const f = await fixture(t);
    let state = await toPhase(f, phase);
    const output = phase === 'brand' ? brandFor(state) : phase === 'page' ? pageFor(state) : { version: 1, assets: [] };
    // A valid parsed response still cannot atomically replace a directory.
    mkdirSync(path.join(f.workspace, '.taste', `${phase}.json`));
    state = await step(state, output, phase);
    assert.equal(state.correcting, true);
    assert.match(state.error.message, /EISDIR|directory/);
    // Atomic temporary names can make each OS diagnostic distinct. The three
    // distinct-error allowance still ends on the fourth failed write.
    for (let attempt = 0; attempt < 3 && state.status !== 'failed'; attempt++)
      state = await dispatch({ action: 'advance', state, output });
    assert.equal(state.status, 'failed', `${phase} must preserve the prior failed write guard`);
  }
});

test('an artifact file symlink is replaced without modifying its external target', async (t) => {
  const f = await fixture(t);
  const state = await toPhase(f, 'brand');
  const target = path.join(f.root, 'outside.json'); writeFileSync(target, '{"keep":true}');
  const artifact = path.join(f.workspace, '.taste/brief.json'); rmSync(artifact); symlinkSync(target, artifact);
  await assert.rejects(dispatch({ action: 'context', state }), /restored/);
  assert.equal(readFileSync(target, 'utf8'), '{"keep":true}');
  assert.deepEqual(upstream.readDesignBrief(f.workspace), state.approvedBrief);
});

test('approved supplied reference changes block later phases', async (t) => {
  const f = await fixture(t); const state = await toPhase(f, 'brand');
  writeFileSync(f.reference, png(32, 32, 120));
  await assert.rejects(dispatch({ action: 'context', state }), /changed after approval/);
});

test('unresolved meaningful assets trigger one blueprint replan, then bounded failure', async (t) => {
  const f = await fixture(t); let state = await toPhase(f, 'assets', { assetNeeds: ['hero-photo'] });
  const manifest = { version: 1, assets: [{ id: 'hero-photo', kind: 'image', role: 'photography', status: 'needed', purpose: 'Hero photo.', requirements: [], sectionIds: ['hero'], aspectRatio: '1:1', composition: 'Centered subject.' }] };
  state = await step(state, manifest, 'page');
  assert.equal(state.assetReplanned, true);
  assert.match((await dispatch({ action: 'context', state })).prompt, /Acquisition found unavailable visual assets/);
  state = await step(state, pageFor(state, ['hero-photo']), 'assets');
  state = await step(state, manifest, 'assets');
  assert.equal(state.correcting, true);
  state = await dispatch({ action: 'advance', state, output: manifest });
  assert.equal(state.status, 'failed');
});

test('asset records must match page needs and approved file content stays frozen', async (t) => {
  const f = await fixture(t); let state = await toPhase(f, 'assets', { assetNeeds: ['data-record'] });
  const invalid = await step(state, { version: 1, assets: [] }, 'assets');
  assert.match(invalid.error.message, /missing: data-record/);
  writeFileSync(path.join(f.workspace, 'record.json'), '{"value":1}');
  const manifest = { version: 1, assets: [{ id: 'data-record', kind: 'data', role: 'data', status: 'ready', purpose: 'Fixture data.', requirements: [], sectionIds: ['hero'], source: { kind: 'project', reference: 'record.json' }, destination: 'record.json' }] };
  state = await step(state, manifest, 'build');
  writeFileSync(path.join(f.workspace, 'record.json'), '{"value":2}');
  await assert.rejects(dispatch({ action: 'context', state }), /approved Design asset changed/);
});

test('exact deliverable validation accounts for files created during planning', async (t) => {
  const f = await fixture(t, { request: 'Create exactly index.html; no other files.', beforeStart: (workspace) => writeFileSync(path.join(workspace, 'keep.txt'), 'existing') });
  let state = await toPhase(f, 'build');
  writeFileSync(path.join(f.workspace, 'index.html'), html);
  writeFileSync(path.join(f.workspace, 'helper.txt'), 'unexpected');
  state = await step(state, buildOutput, 'build');
  assert.match(state.error.message, /unexpected files: helper.txt/);
  assert.match((await dispatch({ action: 'context', state })).prompt, /exact deliverable validation/);
  rmSync(path.join(f.workspace, 'helper.txt'));
  state = await step(state, buildOutput, 'preview');
  assert.equal(readFileSync(path.join(f.workspace, 'keep.txt'), 'utf8'), 'existing');
});

test('a catalog component downloaded into .taste/components is accepted as a ready component', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'page');
  const page = pageFor(state);
  page.sections[0].componentNeeds = ['primary_button'];
  state = await step(state, page, 'assets');
  mkdirSync(path.join(f.workspace, '.taste/components'), { recursive: true });
  writeFileSync(path.join(f.workspace, '.taste/components/primary_button.json'), JSON.stringify({ name: 'button-magnetic', files: [{ path: 'components/motion/button/magnetic.tsx', content: 'export const MagneticButton = () => null' }] }));
  state = await step(state, { version: 1, assets: [{ id: 'primary_button', kind: 'component', role: 'component', status: 'ready', purpose: 'Primary call to action.', requirements: [], sectionIds: ['hero'], source: { kind: 'external', reference: 'https://beui.dev/components/motion/button', license: 'free to use: https://beui.dev' }, destination: '.taste/components/primary_button.json' }] }, 'build');
  assert.equal(state.approvedAssets.assets[0].status, 'ready');
});

test('inline SVG shipped inside a downloaded catalog component passes the source check; hand-drawn SVG does not', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'page');
  const page = pageFor(state);
  page.sections[0].componentNeeds = ['nav_bar'];
  state = await step(state, page, 'assets');
  const goo = '<svg className="absolute" aria-hidden><filter id="goo"><feGaussianBlur stdDeviation="8" /></filter></svg>';
  mkdirSync(path.join(f.workspace, '.taste/components'), { recursive: true });
  writeFileSync(path.join(f.workspace, '.taste/components/nav_bar.json'), JSON.stringify({ name: 'gooey-nav', files: [{ path: 'components/ui/gooey-nav.tsx', content: `export function GooeyNav() {\n  return <nav>\n    ${goo}\n  </nav>\n}` }] }));
  state = await step(state, { version: 1, assets: [{ id: 'nav_bar', kind: 'component', role: 'component', status: 'ready', purpose: 'Site navigation.', requirements: [], sectionIds: ['hero'], source: { kind: 'external', reference: 'https://www.rareui.com/components/gooey-nav', license: 'free to use: https://github.com/swamimalode07/rare-ui' }, destination: '.taste/components/nav_bar.json' }] }, 'build');
  writeFileSync(path.join(f.workspace, 'index.html'), html);
  mkdirSync(path.join(f.workspace, 'src/components/ui'), { recursive: true });
  writeFileSync(path.join(f.workspace, 'src/components/ui/gooey-nav.tsx'), `import { cn } from "@/lib/utils"\nexport function GooeyNav() {\n      return <nav className={cn("x")}>\n        ${goo}\n      </nav>\n}`);
  const passed = await step(state, { ...buildOutput, files: ['index.html', 'src/components/ui/gooey-nav.tsx'] }, 'preview');
  assert.equal(passed.error, undefined);
  writeFileSync(path.join(f.workspace, 'src/components/ui/arrow.tsx'), 'export const Arrow = () => <svg viewBox="0 0 10 10"><path d="M0 5h10" /></svg>');
  const refused = await step(state, { ...buildOutput, files: ['index.html', 'src/components/ui/arrow.tsx'] }, 'build');
  assert.match(refused.error.message, /arrow\.tsx: unmanifested inline SVG/);
});

test('a phase report that omits version 1 is accepted instead of failing the run', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'page');
  const { version, ...page } = pageFor(state);
  assert.equal(version, 1);
  state = await step(state, page, 'assets');
  state = await step(state, { assets: [] }, 'build');
  assert.equal(state.error, undefined);
});

test('build checks reported as objects are flattened instead of failing a finished build', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'build');
  writeFileSync(path.join(f.workspace, 'index.html'), html);
  state = await step(state, { ...buildOutput, checks: [{ name: 'viewport 390', result: 'pass' }, 'reduced motion'] }, 'preview');
  assert.equal(state.error, undefined);
});

test('source quality corrections scan the actual implementation even when output files are empty', async (t) => {
  const f = await fixture(t); let state = await toPhase(f, 'build');
  writeFileSync(path.join(f.workspace, 'index.html'), html + '<style>.feature-card{border-left:3px solid red}</style><svg></svg>');
  state = await step(state, { ...buildOutput, files: [] }, 'build');
  assert.match(state.error.message, /one-sided card-edge border|unmanifested inline SVG/);
  assert.match((await dispatch({ action: 'context', state })).prompt, /source-quality gate/);
  writeFileSync(path.join(f.workspace, 'index.html'), html);
  await step(state, buildOutput, 'preview');
});

test('source quality baseline preserves pre-existing patterns', async (t) => {
  const f = await fixture(t, { beforeStart: (workspace) => writeFileSync(path.join(workspace, 'legacy.html'), '<svg></svg>') });
  assert.ok(f.state.designSourceBaseline.length);
  await toPhase(f, 'preview');
});

test('native preview failure permits one plan correction and cannot reset that bound', async (t) => {
  const f = await fixture(t); let state = await toPhase(f, 'capture');
  state = await dispatch({ action: 'preview_error', state, reason: 'Server script failed.' });
  assert.equal(state.correcting, true);
  state = await step(state, previewPlan, 'preview');
  assert.equal(state.correcting, true);
  state = await dispatch({ action: 'preview_error', state, reason: 'Another server failure.' });
  assert.equal(state.status, 'failed');
});

test('captures require every viewport, genuine raster bytes, unique paths and valid DOM data', async (t) => {
  const f = await fixture(t); const state = await toPhase(f, 'capture');
  await assert.rejects(dispatch({ action: 'capture', state, previewUrl: previewPlan.url, screenshots: [] }), /every planned viewport/);
  const file = path.join(f.root, 'fake.png'); writeFileSync(file, 'not an image');
  const shots = previewPlan.viewports.map((v, i) => ({ ...v, path: i ? f.reference : file }));
  await assert.rejects(dispatch({ action: 'capture', state, previewUrl: previewPlan.url, screenshots: shots }), /recognizable/);
  writeFileSync(file, png());
  const duplicates = shots.map((shot) => ({ ...shot, path: file }));
  await assert.rejects(dispatch({ action: 'capture', state, previewUrl: previewPlan.url, screenshots: duplicates }), /unique/);
  await assert.rejects(dispatch({ action: 'capture', state, previewUrl: 'https://example.com/', screenshots: shots }), /explicit local/);
  shots[0].domAudit = { h1Count: -1, interactiveTargetViolations: [] };
  await assert.rejects(dispatch({ action: 'capture', state, previewUrl: previewPlan.url, screenshots: shots }), /h1Count/);
});

test('DOM findings override a claimed pass and visual correction can continue past two cycles', async (t) => {
  const f = await fixture(t); let state = await toPhase(f, 'capture');
  const audit = { h1Count: 2, interactiveTargetViolations: [{ selector: 'button', label: 'Small', width: 20, height: 20 }] };
  state = await submitCaptures(f, state, audit);
  for (let attempt = 1; attempt <= 3; attempt++) {
    state = await step(state, passReview, 'repair');
    assert.equal(state.repairAttempt, attempt);
    assert.ok(state.review.findings.some((finding) => finding.id === 'document_h1_count'));
    assert.ok(state.review.findings.some((finding) => finding.id === 'interactive_target_size'));
    const context = await dispatch({ action: 'context', state });
    assert.match(context.prompt, new RegExp(`visual correction cycle ${attempt}`));
    assert.doesNotMatch(context.prompt, /attempt \d+ of \d+/);
    state = await step(state, buildOutput, 'preview');
    state = await submitCaptures(f, state, audit);
  }
  state = await step(state, passReview, 'repair');
  assert.equal(state.repairAttempt, 4);
  assert.equal(state.status, 'running');
});

test('native full-page PNG captures larger than the asset limit are validated without weakening asset limits', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'preview');
  const plan = { ...previewPlan, viewports: [{ name: 'desktop', width: 1440, height: 1000 }] };
  state = await step(state, plan, 'preview');
  const file = path.join(f.root, 'full-page.png');
  writeFileSync(file, png(1440, 12000));
  const input = { action: 'capture', state, previewUrl: plan.url,
    screenshots: [{ path: file, width: 1440, height: 1000, captureHeight: 12000, fullDocument: true, domAudit: { h1Count: 1, interactiveTargetViolations: [] } }] };
  const next = await dispatch(input);
  assert.equal(next.phase, 'review');
  const corrupt = readFileSync(file); corrupt[corrupt.length - 1] ^= 1; writeFileSync(file, corrupt);
  await assert.rejects(dispatch(input), /checksum/);
});

test('successful repair recaptures before acceptance and screenshots remain immutable during review', async (t) => {
  const f = await fixture(t); let state = await toPhase(f, 'review');
  const original = readFileSync(state.screenshots[0].path);
  writeFileSync(state.screenshots[0].path, png(32, 32, 150));
  await assert.rejects(dispatch({ action: 'context', state }), /changed after approval/);
  writeFileSync(state.screenshots[0].path, original);
  state = await step(state, repairReview, 'repair');
  state = await step(state, buildOutput, 'preview');
  await assert.rejects(dispatch({ action: 'advance', state, output: passReview }), /not awaiting model output/);
  state = await submitCaptures(f, state);
  state = await step(state, passReview, 'complete');
  assert.equal(state.outcome, 'passed'); assert.match(state.completion, /after 1 correction cycle/);
});

test('current upstream repair validation retains its correction guard after a parsed complete report', async (t) => {
  const f = await fixture(t);
  let state = await toPhase(f, 'review');
  state = await step(state, repairReview, 'repair');
  writeFileSync(path.join(f.workspace, 'index.html'), html + '<svg></svg>');
  state = await step(state, buildOutput, 'repair');
  assert.equal(state.correcting, true);
  state = await dispatch({ action: 'advance', state, output: buildOutput });
  assert.equal(state.status, 'failed');
  assert.match(state.error.message, /unmanifested inline SVG/);
});

test('missing capture produces explicit unaccepted review-skipped completion', async (t) => {
  const f = await fixture(t); const pending = await toPhase(f, 'capture');
  const state = await dispatch({ action: 'capture_unavailable', state: pending, reason: 'Desktop capture is unavailable' });
  const ctx = await dispatch({ action: 'context', state });
  assert.equal(ctx.outcome, 'review_skipped'); assert.equal(ctx.accepted, false);
  assert.match(ctx.completion, /Desktop capture is unavailable/);
});

test('package manifest binds code and assets, rejects omitted or modified files, and relocates', async (t) => {
  const f = await fixture(t);
  const installed = path.join(f.root, 'installed');
  const manifest = JSON.parse(readFileSync(path.join(runtimeRoot, 'vendor/tastecode-design/MANIFEST.json')));
  const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
  // Copy only the files needed by the integrity verifier into a tiny trusted
  // fixture package. Production package coverage is separately checked in full.
  const kept = manifest.files.filter((entry) => ['scripts/iris-design-runtime.mjs', 'scripts/iris-design-preview.mjs', 'vendor/tastecode-design/dist/index.js', 'vendor/tastecode-design/server/dist/preview.js', 'vendor/tastecode-design/references/library/catalog.json'].includes(entry.path));
  for (const entry of kept) { mkdirSync(path.dirname(path.join(installed, entry.path)), { recursive: true }); cpSync(path.join(runtimeRoot, entry.path), path.join(installed, entry.path)); }
  const manifestPath = path.join(installed, 'vendor/tastecode-design/MANIFEST.json');
  writeFileSync(manifestPath, JSON.stringify({ ...manifest, files: kept }));
  const expectedManifestSha256 = hash(readFileSync(manifestPath));
  assert.equal(checkPackage({ root: installed, expectedManifestSha256 }).ok, true);
  writeFileSync(path.join(installed, 'scripts/iris-design-runtime.mjs'), 'modified');
  assert.throws(() => checkPackage({ root: installed, expectedManifestSha256 }), /file changed|check failed/);
  cpSync(path.join(runtimeRoot, 'scripts/iris-design-runtime.mjs'), path.join(installed, 'scripts/iris-design-runtime.mjs'));
  writeFileSync(path.join(installed, 'vendor/tastecode-design/extra.js'), 'untracked');
  assert.throws(() => checkPackage({ root: installed, expectedManifestSha256 }), /Unmanifested/);
  rmSync(manifestPath);
  assert.throws(() => checkPackage({ root: installed }), /manifest unavailable/);
  assert.throws(() => checkPackage({ expectedManifestSha256: '0'.repeat(64) }), /changed since this run started/);
  assert.equal(checkPackage().ok, true);
});

test('transport is one bounded JSON object with explicit process failure and no fallback', async () => {
  const file = path.join(runtimeRoot, 'scripts/iris-design-runtime.mjs');
  for (const input of ['invalid', '{}\n{}', '{"action":"unknown"}']) {
    const result = spawnSync(process.execPath, [file], { input, encoding: 'utf8', maxBuffer: MAX_WIRE_BYTES, timeout: 10000 });
    assert.equal(result.status, 1);
    assert.equal(typeof JSON.parse(result.stdout).error.message, 'string');
  }
  const oversized = spawnSync(process.execPath, [file], { input: 'x'.repeat(MAX_WIRE_BYTES + 1), encoding: 'utf8', timeout: 10000 });
  assert.equal(oversized.status, 1); assert.equal(JSON.parse(oversized.stdout).error.code, 'INPUT_TOO_LARGE');
});
