// One actual Iris assignment through the normal Phoenix gateway and provider.
// Reuses the established disposable gateway/browser/account lifecycle. No model
// response, site implementation, layout or repair is injected by this driver.
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile, copyFile, readdir, stat, open } from 'node:fs/promises';
import { resolve, join, isAbsolute, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { withAcceptanceGateway, once, messages } from './lib/acceptance-gateway.mjs';
import { acceptanceBrowser } from './lib/acceptance-browser.mjs';
import { acceptanceAuthOptions } from './lib/acceptance-account.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const [binary, output, ...authFlags] = process.argv.slice(2);
assert.ok(binary && output && isAbsolute(binary) && isAbsolute(output), 'Use absolute binary and new evidence directory');
const auth = acceptanceAuthOptions(authFlags);
const workspace = join(output, 'work');
await mkdir(output, { mode: 0o700 });
await mkdir(workspace, { mode: 0o700 });
await mkdir(join(workspace, 'assets'));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const sourceAssets = [
  ['canvas-app/ui/assets/phoenix_logo.png', 'phoenix-logo.png'],
  ['web/landing/assets/phoenix-team-light.png', 'phoenix-team-light.png'],
  ['web/landing/assets/phoenix-team-dark.png', 'phoenix-team-dark.png'],
  ['web/landing/assets/phoenix-settings-dark.png', 'phoenix-settings-dark.png'],
];
const provenance = [];
for (const [source, name] of sourceAssets) {
  const bytes = await readFile(join(root, source));
  await copyFile(join(root, source), join(workspace, 'assets', name));
  provenance.push({ path: `assets/${name}`, source, sha256: hash(bytes), bytes: bytes.length });
}
await writeFile(join(workspace, 'assets/provenance.json'), JSON.stringify({
  meaning: 'Phoenix-owned product logo and actual Phoenix renderer captures with example conversation content, not live task results. These are product assets, not page-layout references.',
  files: provenance,
}, null, 2) + '\n');
await writeFile(join(workspace, 'PRODUCT.md'), `# Phoenix product information

Phoenix is a local-first desktop application with a persistent team of AI coworkers. It brings conversations, files, a terminal, a managed browser and desktop tools into one workspace. Users give a coworker a task, follow its tool activity and inspect the resulting work. The product is currently being developed and tested on Linux. This is a local landing-page build for the product, not a public release or a fictional business.

## Product behavior

Phoenix supports code and website implementation, research with sources, documents and files, work in the managed browser, and native desktop operations. These are capabilities under development, not a guarantee of flawless completion. Do not promise that every task finishes autonomously or that any benchmark success rate has been established.

Each coworker has a persistent conversation. Users can work directly with one coworker or bring coworkers together in a group. Phoenix is the coordinator, not a required middleman for every action. Tool activity and coworker contributions appear in the conversation. Model and account routes can be selected for different jobs. Existing provider accounts remain subject to their providers' limits and supported connection methods.

Workspaces and permission choices determine allowed access. Users can inspect changes and stop work. Conversation and workspace state is stored locally; requests to chosen model providers and connected services can send relevant task content to those services. Local-first does not mean that cloud model requests stay offline.

The desktop has light and dark themes, adjustable text size, interface density, corners, and other settings. The supplied interface captures show the actual application with clearly example conversation content. They are usable product assets, not a requirement to copy their colors, typography or surrounding page layout. The logo is the product identity and should not be replaced.

## Coworkers

Phoenix — coordination and synthesis.
Iris — product design, frontend and visual verification.
Leo — engineering, code, tests and builds.
Theo — research, current facts and sources.
Elena — documents, reports and knowledge.
Remy — reliability, testing and failure analysis.
Nico — correspondence and writing.
Maya — scheduling and coordination.
Vera — finance, subscriptions and purchasing controls.
Owen — relationships and follow-ups.
June — publishing and campaigns.
Cleo — operations and recurring work.

## Release facts and boundaries

No final public domain, release date, installer URL, price, waitlist service, customer testimonials or measured productivity figures are supplied. Do not fabricate them. A preview, example task or product walkthrough can be a real interaction on this landing page; it must not submit a model task, collect personal information, contact a real person, purchase anything or deploy. Do not advertise a fake working download or signup.

## Assets and source provenance

assets/phoenix-logo.png is the existing Phoenix product logo.
assets/phoenix-team-light.png and phoenix-team-dark.png are real application-renderer captures with example content.
assets/phoenix-settings-dark.png is a real settings capture.
assets/provenance.json records exact source hashes. The supplied folder contains no prior landing-page implementation and no dictated design direction. Use the managed design runtime's selected composition references and the actual product material.
`);
const request = 'Build a complete animated landing page for Phoenix. Use the product information in PRODUCT.md and the supplied product assets. Give it a distinctive, polished design and working responsive interactions. Deliver the runnable website in this workspace. Do not deploy it.';
await writeFile(join(output, 'request.txt'), request + '\n');
await writeFile(join(output, 'setup.json'), JSON.stringify({
  createdUtc: new Date().toISOString(), binary, workspace, request, auth,
  actor: 'frontend', workflow: 'actual-managed-iris-design',
  model: 'gpt-5.6-sol', effort: 'medium', nativeVision: true,
  observerAuthoredSite: false, observerCoaching: false, designReferenceAttachments: [],
  sourceAssets: provenance, timeLimitMs: 30 * 60 * 1000,
}, null, 2) + '\n');

await withAcceptanceGateway({ binary, output, workspace, ...auth,
  memory: false, nativeVision: true, preserveState: true,
  setup: async ({ home, env }) => {
    // Package root is an implementation dependency, never a second user switch.
    env.PHOENIX_IRIS_DESIGN_ROOT = root;
    env.PHOENIX_NODE = process.execPath;
    env.PATH = dirname(process.execPath) + ':' + env.PATH;
    return acceptanceBrowser({ home, env, output });
  },
}, async ({ socketPath, home }) => {
  await once(socketPath, { Onboarding: { action: 'choose_company', choice: 'founding_company' } });
  const directory = await once(socketPath, { CompanyDirectory: { action: 'status' } });
  await writeFile(join(output, 'directory.json'), JSON.stringify(directory, null, 2) + '\n');
  const snapshot = directory.CompanyDirectory?.directory;
  const candidates = snapshot?.agents ?? [];
  const actor = candidates.find(item => item.agent_id === 'frontend');
  assert.ok(actor, 'Actual Iris directory entry must exist');
  const profile = actor.profile ?? actor;
  const session_id = profile.canonical_session_id;
  assert.ok(session_id, 'Iris must have a canonical conversation');
  const turn_id = `iris-design-phoenix-${Date.now()}`;
  const submission = { Turn: {
    session_id, turn_id, user_request: request, workspace,
    permission_mode: 'full_access', interaction_mode: 'execute',
    journal: true, delivery: 'queue', target_agent: profile.agent_id,
  } };
  await writeFile(join(output, 'submission.json'), JSON.stringify(submission, null, 2) + '\n');
  const terminals = [];
  const log = await open(join(output, 'events.jsonl'), 'wx', 0o600);
  const abort = new AbortController();
  let cancelledByHarness = false;
  const timer = setTimeout(async () => {
    cancelledByHarness = true;
    await once(socketPath, { Cancel: { session_id } }, 10_000).catch(() => {});
  }, 30 * 60 * 1000);
  const start = Date.now();
  let failure = null;
  try {
    for await (const value of messages(socketPath, submission, { signal: AbortSignal.any([abort.signal, AbortSignal.timeout(30 * 60 * 1000 + 30_000)]) })) {
      await log.write(JSON.stringify({ elapsed_ms: Date.now() - start, frame: value }) + '\n');
      if (value.Story && ['tool','status','warning','answer'].includes(value.Story.kind)) {
        const { kind, status, title, text } = value.Story;
        console.log(JSON.stringify({ elapsedSeconds: Math.round((Date.now() - start) / 1000), kind, status, title, text: typeof text === 'string' ? text.slice(0, 700) : undefined }));
      }
      if (value.Done || value.Error) {
        terminals.push(value);
        console.log(JSON.stringify({ elapsedSeconds: Math.round((Date.now() - start) / 1000), terminal: value }));
        break;
      }
    }
  } catch (error) {
    failure = String(error.stack ?? error);
    await once(socketPath, { Cancel: { session_id } }, 10_000).catch(() => {});
  }
  finally { clearTimeout(timer); await log.close(); }
  const files = [];
  async function scan(directory, prefix = '') {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (entry.isSymbolicLink() || ['node_modules', '.git', '.next'].includes(entry.name)) continue;
      const relative = prefix + entry.name, file = join(directory, entry.name);
      if (entry.isDirectory()) await scan(file, relative + '/');
      else if (entry.isFile()) {
        const info = await stat(file);
        if (info.size <= 32 * 1024 * 1024) files.push({ path: relative, bytes: info.size, sha256: hash(await readFile(file)) });
      }
    }
  }
  if (terminals.length) await scan(workspace);
  const result = { finishedUtc: new Date().toISOString(), elapsedMs: Date.now() - start,
    session_id, turn_id, cancelledByHarness, failure,
    done: terminals.filter(event => event.Done), errors: terminals.filter(event => event.Error),
    terminalAcknowledged: terminals.length > 0,
    files, observerAuthoredSite: false, observerCoaching: false,
    visualAcceptance: 'pending independent inspection of the actual saved site and design state',
  };
  await writeFile(join(output, 'result.json'), JSON.stringify(result, null, 2) + '\n');
  // Preserve the exact managed controller evidence if its root is outside the
  // established company/session state copy. Never copy credentials or tokens.
  const designRoot = join(home, 'iris_design');
  try {
    const { cp } = await import('node:fs/promises');
    if ((await stat(designRoot)).isDirectory()) await cp(designRoot, join(output, 'iris-design-state'), { recursive: true, dereference: false, errorOnExist: true, force: false });
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  console.log(JSON.stringify({ finished: true, elapsedMs: result.elapsedMs, cancelledByHarness, failure, files: files.length }));
});
