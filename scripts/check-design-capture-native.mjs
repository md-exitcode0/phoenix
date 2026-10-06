import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, copyFile } from 'node:fs/promises';
import { join, resolve, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { acceptanceBrowser } from './lib/acceptance-browser.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = resolve(process.argv[2] ?? 'artifacts/iris-design-mode-2026-09-20/prime/native-capture');
await mkdir(output, { mode: 0o700 });
const temporary = await mkdtemp(join(tmpdir(), 'phoenix-design-capture-'));
const home = join(temporary, 'state');
await mkdir(home, { mode: 0o700 });
await writeFile(join(home, 'design-capture-fixture.marker'), 'private no-model fixture\n', { mode: 0o600 });
const env = { ...process.env };
for (const key of Object.keys(env)) if (/TOKEN|API_KEY|SECRET|PASSWORD|CREDENTIAL|COOKIE|^COMPOSIO|^OPENAI|^ANTHROPIC|^PHOENIX_|^CODEX_|^DISPLAY$|^WAYLAND_DISPLAY$|^DBUS_|^XDG_ACTIVATION_TOKEN$/.test(key)) delete env[key];
env.PHOENIX_HOME = home;
env.PHOENIX_BROWSER_LOGIN_SOURCE = 'none';
env.PHOENIX_NODE = process.execPath;
env.PATH = dirname(process.execPath) + ':' + env.PATH;
for (const [key, name] of Object.entries({XDG_DATA_HOME:'data',XDG_CONFIG_HOME:'config',XDG_CACHE_HOME:'cache',TMPDIR:'tmp'})) {
  env[key] = join(temporary, name); await mkdir(env[key], { mode: 0o700 });
}
const html = `<!doctype html><html><head><meta charset="utf-8"><style>body{margin:0;font:18px sans-serif;background:#f5f2ed;color:#222}h1{padding:30px}section{height:1300px;padding:30px;box-sizing:border-box}#small{width:26px;height:20px;padding:0;border:0}#wide{width:1700px;background:#eab29e}</style></head><body><h1>Native design capture fixture</h1><button id="small" aria-label="Deliberately undersized">x</button><section id="wide">Overflow remains an observable defect, not a widened capture.</section><section>Full page content</section><script>document.querySelectorAll=()=>[];window.__fakeAudit={h1Count:1,interactiveTargetViolations:[]};</script></body></html>`;
const server = createServer((request, response) => { response.writeHead(200, { 'content-type':'text/html','cache-control':'no-store' }); response.end(html); });
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
env.PHOENIX_DESIGN_CAPTURE_TEST_URL = `http://127.0.0.1:${server.address().port}/`;
env.PHOENIX_DESIGN_CAPTURE_TEST_OUTPUT = join(output, 'result.json');
let stop, exitCode;
try {
  stop = await acceptanceBrowser({ home, env, output });
  const child = spawn('cargo', ['test','--offline','--lib','tools::browser_native::design_preview::tests::native_design_capture_preserves_viewports_and_real_dom_failures','--','--ignored','--exact','--test-threads=1','--nocapture'], {
    cwd: root, env: { ...env, CARGO_BUILD_JOBS:'1' }, stdio:['ignore','pipe','pipe'],
  });
  // Test filter is the full name when --exact is requested.
  let stdout='', stderr='';
  child.stdout.on('data', bytes => { stdout += bytes; });
  child.stderr.on('data', bytes => { stderr += bytes; });
  const timer = setTimeout(() => child.kill('SIGTERM'), 180_000);
  try { exitCode = await new Promise((resolve,reject) => { child.once('error',reject);child.once('exit',resolve); }); }
  finally { clearTimeout(timer); }
  await writeFile(join(output, 'test.log'), stdout+stderr);
  assert.equal(exitCode, 0, (stdout+stderr).slice(-6000));
  const result = JSON.parse(await readFile(env.PHOENIX_DESIGN_CAPTURE_TEST_OUTPUT));
  assert.equal(result.passed, true);
  for (const capture of result.screenshots) await copyFile(capture.path, join(output, `${capture.width}.png`));
  console.log(JSON.stringify({ passed:true, screenshots:result.screenshots.length, modelRequests:0, output }));
} finally {
  await stop?.();
  await new Promise(resolve => server.close(resolve));
}
