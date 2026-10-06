#!/usr/bin/env node
// Open a canvas UI ?shot= fixture in headless Chrome and print its verdict.
//   node scripts/ui-shot.mjs <shot> [screenshot.png] [width] [height]
import {spawn} from 'node:child_process';
import {mkdtemp, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import http from 'node:http';
import {readFile} from 'node:fs/promises';
const [shot, png, w = '1440', h = '1000'] = process.argv.slice(2);
const root = process.env.UI_ROOT || fileURLToPath(new URL('../canvas-app/ui/', import.meta.url));
const types = {'.js':'text/javascript','.css':'text/css','.html':'text/html','.svg':'image/svg+xml','.png':'image/png','.woff2':'font/woff2','.json':'application/json'};
const server = http.createServer(async (req, res) => {
  const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  try { const body = await readFile(join(root, path === '/' ? 'index.html' : path)); res.writeHead(200, {'content-type': types[path.slice(path.lastIndexOf('.'))] || 'application/octet-stream'}); res.end(body); }
  catch { res.writeHead(404); res.end(); }
}).listen(0);
const httpPort = server.address().port, port = 9800 + Math.floor(Math.random() * 150);
const chrome = spawn('google-chrome', ['--headless=new','--disable-gpu','--no-sandbox',`--remote-debugging-port=${port}`,`--user-data-dir=${await mkdtemp(join(tmpdir(),'uishot-'))}`,'about:blank'], {stdio:'ignore'});
const sleep = ms => new Promise(r => setTimeout(r, ms));
let targets; for (let i = 0; i < 50 && !targets; i++) { try { targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); } catch { await sleep(100); } }
const ws = new WebSocket(targets.find(t => t.type === 'page').webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, {once:true}));
let id = 0; const pending = new Map(); const logs = [];
ws.addEventListener('message', e => { const m = JSON.parse(e.data); if (m.method === 'Runtime.exceptionThrown') logs.push(m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text); pending.get(m.id)?.(m); pending.delete(m.id); });
const call = (method, params = {}) => new Promise(r => { const n = ++id; pending.set(n, r); ws.send(JSON.stringify({id:n, method, params})); });
await call('Runtime.enable'); await call('Log.enable');
await call('Emulation.setDeviceMetricsOverride', {width:+w, height:+h, deviceScaleFactor:1, mobile:false});
await call('Page.navigate', {url:`http://127.0.0.1:${httpPort}/index.html?shot=${encodeURIComponent(shot)}`});
let title = '';
for (let i = 0; i < 90; i++) { await sleep(500); title = (await call('Runtime.evaluate', {expression:'document.title', returnByValue:true})).result.result.value; if (/^(PASS|FAIL|READY)/.test(title)) break; }
if (png) { const s = await call('Page.captureScreenshot', {format:'png'}); await writeFile(png, Buffer.from(s.result.data, 'base64')); }
const checks = (await call('Runtime.evaluate', {expression:'document.documentElement.dataset.conversationAcceptanceChecks||""', returnByValue:true})).result.result.value;
const step = (await call('Runtime.evaluate', {expression:'document.documentElement.dataset.conversationAcceptanceStep||""', returnByValue:true})).result.result.value;
console.log(JSON.stringify({title, step, errors: logs.slice(0, 5)}));
if (checks) { const c = JSON.parse(checks); console.log('failed:', Object.entries(c).filter(([, v]) => !v).map(([k]) => k).join(', ') || 'none', `(${Object.keys(c).length} checks)`); }
ws.close(); chrome.kill(); server.close();
