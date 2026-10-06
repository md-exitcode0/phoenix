// Bounded transport regressions; native integration is exercised separately.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { EventEmitter } from 'node:events';
const require = createRequire(import.meta.url);
const { captureScreenshot } = require('../canvas-app/chromium-shell/browser-capture.cjs');
function fixture() {
  const calls = [], debuggerApi = new EventEmitter();
  let attached = false;
  debuggerApi.isAttached = () => attached;
  debuggerApi.attach = () => { attached = true; };
  debuggerApi.detach = () => { attached = false; };
  debuggerApi.sendCommand = async (method, params) => {
    calls.push({ method, params });
    if (method === 'Page.getLayoutMetrics') return { cssContentSize: { width: 1700, height: 3000 } };
    if (method === 'Page.captureScreenshot') {
      const png = Buffer.alloc(64);
      Buffer.from([137,80,78,71,13,10,26,10]).copy(png);
      png.write('IHDR', 12); png.writeUInt32BE(params.clip?.width ?? 1280, 16);
      png.writeUInt32BE(params.clip?.height ?? 900, 20);
      return { data: png.toString('base64') };
    }
    return {};
  };
  return { contents: { isDestroyed: () => false, debugger: debuggerApi }, calls, attached: () => attached };
}
const results = [];
for (const invalid of [
  {x:0,y:0,width:0,height:900,scale:1}, {x:0,y:0,width:390,height:12001,scale:1},
  {x:-1,y:0,width:390,height:900,scale:1}, {x:0,y:0,width:390.2,height:900,scale:1},
  {x:0,y:0,width:390,height:900,scale:2}, {x:0,y:0,width:3840,height:12000,scale:1},
]) {
  const f = fixture();
  await assert.rejects(captureScreenshot(f.contents, { fullPage: true, clipOverride: invalid }), /invalid dimensions/);
  assert.equal(f.calls.length, 0); assert.equal(f.attached(), false);
}
results.push('Malformed clips reject before debugger attachment');
{
  const f = fixture(), clip = {x:0,y:0,width:390,height:3000,scale:1};
  const bytes = await captureScreenshot(f.contents, { fullPage: true, clipOverride: clip });
  assert.equal(bytes.readUInt32BE(16), 390);
  const sent = f.calls.find(call => call.method === 'Page.captureScreenshot');
  assert.deepEqual(sent.params.clip, clip); assert.equal(sent.params.captureBeyondViewport, true);
  assert.ok(f.calls.some(call => call.method === 'Page.stopScreencast'));
  assert.equal(f.attached(), false);
  results.push('Exact design width retained despite overflow; full-page capture and cleanup occur');
}
{
  const f = fixture();
  await captureScreenshot(f.contents, { fullPage: true });
  assert.equal(f.calls.find(call => call.method === 'Page.captureScreenshot').params.clip.width, 1700);
  assert.equal(f.attached(), false);
  results.push('Existing unmodified full-page caller retains its original behavior');
}
{
  const f = fixture();
  f.contents.debugger.sendCommand = async (method) => { if (method === 'Page.captureScreenshot') throw Error('injected capture failure'); return {}; };
  await assert.rejects(captureScreenshot(f.contents, { fullPage: true, clipOverride: {x:0,y:0,width:390,height:3000,scale:1} }), /injected/);
  assert.equal(f.attached(), false);
  results.push('Failure still retires the owned debugger');
}
console.log(JSON.stringify({passed:true, checks:results, actualBrowser:false}));
