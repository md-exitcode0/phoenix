import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { once } from 'node:events';
import { runtimeRoot } from './iris-design-runtime.mjs';
import { html, previewPlan } from './iris-design-runtime-fixtures.mjs';

function workspace(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'iris-preview-test-'));
  writeFileSync(path.join(root, 'index.html'), html);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}
async function reserve(t) {
  const server = http.createServer((_, response) => response.end('other owner'));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  return `http://127.0.0.1:${server.address().port}/`;
}
function service(t, root, plan, { send = true } = {}) {
  const child = spawn(process.execPath, [path.join(runtimeRoot, 'scripts/iris-design-preview.mjs')], {
    cwd: root, env: { ...process.env, PATH: `${path.dirname(process.execPath)}${path.delimiter}${process.env.PATH ?? ''}` },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const lines = [];
  let buffer = '';
  let stderr = '';
  const waiters = [];
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk) => {
    buffer += chunk;
    while (buffer.includes('\n')) {
      const index = buffer.indexOf('\n');
      const line = buffer.slice(0, index); buffer = buffer.slice(index + 1);
      if (!line) continue;
      const value = JSON.parse(line); lines.push(value);
      waiters.shift()?.(value);
    }
  });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const exit = once(child, 'exit');
  t.after(async () => {
    child.stdin.end();
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
    await exit;
  });
  const first = () => lines.length ? Promise.resolve(lines[0]) : new Promise((resolve) => waiters.push(resolve));
  if (send) child.stdin.write(`${JSON.stringify({ workspace: root, plan })}\n`);
  return { child, exit, first, lines, stderr: () => stderr };
}
async function unavailable(url) {
  await assert.rejects(fetch(url, { signal: AbortSignal.timeout(1000) }));
}

test('original static preview chooses another occupied port and closes on EOF', { timeout: 10000 }, async (t) => {
  const root = workspace(t);
  const occupied = await reserve(t);
  const owner = service(t, root, { ...previewPlan, url: occupied });
  const ready = await owner.first();
  assert.equal(ready.ready, true, JSON.stringify(ready));
  assert.notEqual(ready.url, occupied);
  assert.deepEqual(ready.viewports, previewPlan.viewports);
  const response = await fetch(ready.url);
  assert.equal(await response.text(), html);
  assert.ok(response.headers.get('x-harness-preview-id'));
  assert.equal(response.headers.get('cache-control'), 'no-store');
  owner.child.stdin.end();
  assert.deepEqual(await owner.exit, [0, null]);
  await unavailable(ready.url);
  assert.equal(await (await fetch(occupied)).text(), 'other owner');
  assert.equal(owner.stderr(), '');
});

test('static preview retains path boundaries and rejects non-GET requests', { timeout: 10000 }, async (t) => {
  const root = workspace(t);
  writeFileSync(path.join(root, '.env'), 'PRIVATE_TEST_VALUE');
  const owner = service(t, root, previewPlan);
  const ready = await owner.first(); assert.equal(ready.ready, true, JSON.stringify(ready));
  for (const suffix of ['.env', '..%2Foutside.txt', '%2e%2e%5coutside.txt', '.git/config']) {
    const response = await fetch(ready.url + suffix);
    assert.equal(response.status, 404);
    assert.doesNotMatch(await response.text(), /PRIVATE_TEST_VALUE/);
  }
  assert.equal((await fetch(ready.url, { method: 'POST' })).status, 405);
  owner.child.stdin.end(); await owner.exit;
});

test('original static resource validation stops a preview with missing local media', { timeout: 10000 }, async (t) => {
  const root = workspace(t);
  writeFileSync(path.join(root, 'index.html'), html + '<img src="missing.png">');
  const owner = service(t, root, previewPlan);
  const result = await owner.first();
  assert.equal(result.ready, false); assert.match(result.error.message, /resource is unavailable/);
  assert.deepEqual(await owner.exit, [1, null]);
});

test('original node preview runs workspace code, strips caller credentials and stops on SIGTERM', { timeout: 10000 }, async (t) => {
  const root = workspace(t);
  writeFileSync(path.join(root, 'server.mjs'), `import http from 'node:http';
http.createServer((request,response)=>response.end(JSON.stringify({home:process.env.HOME,secret:process.env.IRIS_TEST_SECRET??null}))).listen(Number(process.env.PORT),'127.0.0.1');
`);
  const occupied = await reserve(t);
  const previous = process.env.IRIS_TEST_SECRET;
  process.env.IRIS_TEST_SECRET = 'must-not-reach-preview';
  const owner = service(t, root, { ...previewPlan, kind: 'command', command: 'node', args: ['server.mjs'], url: occupied });
  if (previous === undefined) delete process.env.IRIS_TEST_SECRET; else process.env.IRIS_TEST_SECRET = previous;
  const ready = await owner.first(); assert.equal(ready.ready, true, JSON.stringify(ready));
  assert.notEqual(ready.url, occupied);
  assert.deepEqual(await (await fetch(ready.url)).json(), { home: root, secret: null });
  owner.child.kill('SIGTERM');
  assert.deepEqual(await owner.exit, [0, null]);
  await unavailable(ready.url);
});

test('preview validation rejects eval, network runners, undeclared scripts and remote targets', { timeout: 10000 }, async (t) => {
  const root = workspace(t);
  const cases = [
    [{ ...previewPlan, kind: 'command', command: 'node', args: ['--eval', '0'] }, /workspace script/],
    [{ ...previewPlan, kind: 'command', command: 'npx', args: ['some-package'] }, /must be one of/],
    [{ ...previewPlan, kind: 'command', command: 'npm', args: ['run', 'missing'] }, /not declared/],
    [{ ...previewPlan, kind: 'command', command: 'npm', args: ['--prefix', '..', 'run', 'dev'] }, /workspace selectors/],
    [{ ...previewPlan, url: 'https://example.com/' }, /127.0.0.1/],
    [{ ...previewPlan, cwd: '../' }, /inside the workspace/],
  ];
  for (const [plan, pattern] of cases) {
    const owner = service(t, root, plan);
    const result = await owner.first();
    assert.equal(result.ready, false); assert.match(result.error.message, pattern);
    assert.deepEqual(await owner.exit, [1, null]);
  }
});

test('EOF immediately after a start line cannot leave a server or emit a ready result', { timeout: 10000 }, async (t) => {
  const root = workspace(t);
  const owner = service(t, root, previewPlan, { send: false });
  owner.child.stdin.end(`${JSON.stringify({ workspace: root, plan: previewPlan })}\n`);
  assert.deepEqual(await owner.exit, [0, null]);
  assert.equal(owner.lines.some((line) => line.ready), false);
});

test('a second preview request is rejected and the first owned server is stopped', { timeout: 10000 }, async (t) => {
  const root = workspace(t);
  const owner = service(t, root, previewPlan);
  const ready = await owner.first(); assert.equal(ready.ready, true, JSON.stringify(ready));
  owner.child.stdin.write('{}\n');
  assert.deepEqual(await owner.exit, [1, null]);
  assert.equal(owner.lines.at(-1).error.code, 'INVALID_INPUT');
  await unavailable(ready.url);
});

for (const method of ['EOF', 'SIGTERM']) {
  test(`${method} cancels an already spawned command before its readiness timeout`, { timeout: 10000 }, async (t) => {
    const root = workspace(t);
    writeFileSync(path.join(root, 'slow.mjs'), `import {writeFileSync} from 'node:fs';
writeFileSync('started.json',JSON.stringify({pid:process.pid}));
setInterval(()=>{},1000);
`);
    const owner = service(t, root, { ...previewPlan, kind: 'command', command: 'node', args: ['slow.mjs'] });
    const marker = path.join(root, 'started.json');
    const deadline = Date.now() + 4000;
    while (!existsSync(marker) && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 20));
    assert.ok(existsSync(marker), 'Upstream runner spawned the slow fixture');
    const { pid } = JSON.parse(readFileSync(marker, 'utf8'));
    const start = Date.now();
    if (method === 'EOF') owner.child.stdin.end(); else owner.child.kill('SIGTERM');
    assert.deepEqual(await owner.exit, [0, null]);
    assert.ok(Date.now() - start < 5000, 'Cancellation should not wait the 30-second readiness timeout');
    assert.equal(owner.lines.some((line) => line.ready), false);
    assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
  });
}
