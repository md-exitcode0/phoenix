// A real Electron idle/capture lifetime check, with no provider or user data.
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,readFile,writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {acceptanceBrowser,connectCdp} from './lib/acceptance-browser.mjs';
const [output]=process.argv.slice(2);
if(!output?.startsWith('/'))throw Error('New absolute evidence directory required');
await mkdir(output,{mode:0o700});
const home=await mkdtemp(join(tmpdir(),'phoenix-ui-acceptance-browser-lifetime-'));
const env={...process.env,PHOENIX_HOME:home,XDG_DATA_HOME:join(home,'data'),XDG_CONFIG_HOME:join(home,'config'),XDG_CACHE_HOME:join(home,'cache'),TMPDIR:join(home,'tmp')};
for(const name of ['DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','DBUS_STARTER_ADDRESS','DBUS_STARTER_BUS_TYPE',
  'PHOENIX_CHROMIUM_BRIDGE_URL','PHOENIX_CHROMIUM_BRIDGE_TOKEN','PHOENIX_BROWSER_ATTACH'])delete env[name];
for(const name of ['XDG_DATA_HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','TMPDIR'])await mkdir(env[name],{recursive:true,mode:0o700});
const observations=[];let cleanup,browser;
try {
  cleanup=await acceptanceBrowser({home,env,output});
  const endpoint=new URL('health',env.PHOENIX_CHROMIUM_BRIDGE_URL);
  endpoint.searchParams.set('token',env.PHOENIX_CHROMIUM_BRIDGE_TOKEN);
  for(let second=0;second<=30;second+=5){
    if(second)await new Promise(resolve=>setTimeout(resolve,5000));
    const response=await fetch(endpoint,{signal:AbortSignal.timeout(2000)});
    const health=await response.json();assert.equal(response.status,200);assert.equal(health.ok,true);
    observations.push({second,ok:true,rustConnected:health.rustConnected});
  }
  const opened=new URL('browser/open',env.PHOENIX_CHROMIUM_BRIDGE_URL);
  opened.searchParams.set('token',env.PHOENIX_CHROMIUM_BRIDGE_TOKEN);
  const response=await fetch(opened,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({instance:'agent-coder'}),signal:AbortSignal.timeout(5000)});
  assert.equal(response.status,200);const surface=await response.json();
  const meta=JSON.parse(await readFile(join(output,'browser.json')));
  const targets=await fetch(`http://127.0.0.1:${meta.debugPort}/json/list`).then(r=>r.json());
  const target=targets.find(row=>row.id===surface.targetId);assert.ok(target,'same owned surface is available after idle');
  browser=await connectCdp(target.webSocketDebuggerUrl);
  await browser.send('Page.navigate',{url:'data:text/html,<title>Lifetime proof</title><h1>Still drawable after idle</h1>'});
  for(let attempt=0;attempt<50;attempt++){
    if(await browser.evaluate('document.title')==='Lifetime proof')break;
    await new Promise(resolve=>setTimeout(resolve,100));
  }
  assert.equal(await browser.evaluate('document.querySelector("h1").textContent'),'Still drawable after idle');
  // Hidden managed pages use Phoenix's exact-target frame pump, not raw CDP
  // capture (which can wait forever on an unopened WebContentsView).
  const capture=new URL('browser/capture',env.PHOENIX_CHROMIUM_BRIDGE_URL);
  capture.searchParams.set('token',env.PHOENIX_CHROMIUM_BRIDGE_TOKEN);
  const image=await fetch(capture,{method:'POST',headers:{'Content-Type':'application/json'},
    body:JSON.stringify({instance:'agent-coder',targetId:surface.targetId,fullPage:false}),signal:AbortSignal.timeout(12000)});
  assert.equal(image.status,200);assert.equal(image.headers.get('x-phoenix-target-id'),surface.targetId);
  const bytes=Buffer.from(await image.arrayBuffer());assert.ok(bytes.length>1000);assert.equal(bytes.subarray(1,4).toString(),'PNG');
  await writeFile(join(output,'browser.png'),bytes);
  await writeFile(join(output,'result.json'),JSON.stringify({passed:true,observations,modelCalls:0,
    idleSeconds:30,embeddedPageOperable:true,reasonForEarlierDeath:'not established'},null,2));
  console.log('PASS: owned Electron remains reachable beyond 30 seconds and opens/renders its exact embedded surface; no model request.');
} catch(error) {
  await writeFile(join(output,'result.json'),JSON.stringify({passed:false,observations,error:String(error.message),modelCalls:0},null,2));throw error;
} finally {browser?.close();await cleanup?.();}
