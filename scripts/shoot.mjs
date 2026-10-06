#!/usr/bin/env node
// Viewport screenshots of a local page at several scroll depths, after
// animations settle — what a visitor actually sees, not a full-page render
// that misrepresents sticky/scroll-driven layouts.
//
//   node scripts/shoot.mjs <file-or-url> <out-prefix> [width=1440] [height=900] [stops=0,1,2,3]
import {spawn} from 'node:child_process';
import {mkdtemp, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';

const [target, prefix, w = '1440', h = '900', stops = '0,1,2,3'] = process.argv.slice(2);
const url = /^https?:|^file:/.test(target) ? target : 'file://' + resolve(target);
const port = 9300 + Math.floor(Math.random() * 500);
const profile = await mkdtemp(join(tmpdir(), 'shoot-'));
const chrome = spawn('google-chrome', ['--headless=new', '--disable-gpu', '--no-sandbox', '--hide-scrollbars',
  `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, 'about:blank'], {stdio: 'ignore'});
const sleep = ms => new Promise(r => setTimeout(r, ms));
let targets;
for (let i = 0; i < 50 && !targets; i++) {
  try { targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); } catch { await sleep(100); }
}
const page = targets.find(t => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, {once: true}));
let id = 0; const pending = new Map();
ws.addEventListener('message', e => { const m = JSON.parse(e.data); pending.get(m.id)?.(m); pending.delete(m.id); });
const call = (method, params = {}) => new Promise(r => { const n = ++id; pending.set(n, r); ws.send(JSON.stringify({id: n, method, params})); });
await call('Emulation.setDeviceMetricsOverride', {width: +w, height: +h, deviceScaleFactor: 1, mobile: +w < 700});
await call('Page.enable');
await call('Page.navigate', {url});
await sleep(2500);
const evalJs = async expression => (await call('Runtime.evaluate', {expression, returnByValue: true})).result?.result?.value;
const total = await evalJs('document.documentElement.scrollHeight');
for (const stop of stops.split(',').map(Number)) {
  await evalJs(`window.scrollTo(0, ${stop * +h})`);
  await sleep(900);
  const shot = await call('Page.captureScreenshot', {format: 'png'});
  await writeFile(`${prefix}-${w}-${stop}.png`, Buffer.from(shot.result.data, 'base64'));
}
console.log(JSON.stringify({url, width: +w, scrollHeight: total, screens: +(total / +h).toFixed(1)}));
ws.close(); chrome.kill();
