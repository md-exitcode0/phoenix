// Render and exercise the actual local VESPER artifact. This is observer
// acceptance, not model work, implementation assistance or a license audit.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdir,mkdtemp,readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {acceptanceBrowser,connectCdp} from './lib/acceptance-browser.mjs';
const [source,output,address='http://127.0.0.1:18806/vesper-original-20260918/']=process.argv.slice(2);
if(![source,output].every(path=>path?.startsWith('/')))throw Error('Absolute source directory and NEW evidence directory required');
const url=new URL(address);assert.equal(url.hostname,'127.0.0.1');
await mkdir(output,{mode:0o700});await mkdir(join(output,'shots'));
const home=await mkdtemp(join(tmpdir(),'vesper-observer-'));
const env={...process.env,PHOENIX_HOME:home,XDG_DATA_HOME:join(home,'data'),XDG_CONFIG_HOME:join(home,'config'),XDG_CACHE_HOME:join(home,'cache'),TMPDIR:join(home,'tmp')};
for(const key of ['DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','DBUS_STARTER_ADDRESS','DBUS_STARTER_BUS_TYPE','PHOENIX_CHROMIUM_BRIDGE_URL','PHOENIX_CHROMIUM_BRIDGE_TOKEN','PHOENIX_BROWSER_ATTACH'])delete env[key];
for(const key of ['XDG_DATA_HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','TMPDIR'])await mkdir(env[key],{recursive:true,mode:0o700});
const hash=bytes=>createHash('sha256').update(bytes).digest('hex'),sleep=ms=>new Promise(r=>setTimeout(r,ms));
const result={startedAt:new Date().toISOString(),source,url:address,modelCalls:0,checks:[],screenshots:[],sourceHashes:[],errors:[],requests:[],limits:['Chromium desktop emulation, not a physical device or WebKit proof.','Source hashes and functional behavior are verified; image-license terms are not independently certified.','Malformed local storage and denied persistence are explicitly injected observer cases.']};
for(const file of ['index.html','styles.css','app.js'])result.sourceHashes.push({file,sha256:hash(await readFile(join(source,file)))});
let stop,browser,ui,surface,observer,phase='startup',fatal;
async function bridge(action,body){
  const endpoint=new URL('browser/'+action,env.PHOENIX_CHROMIUM_BRIDGE_URL);endpoint.searchParams.set('token',env.PHOENIX_CHROMIUM_BRIDGE_TOKEN);
  const response=await fetch(endpoint,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body),signal:AbortSignal.timeout(12000)});
  assert.ok(response.ok,`owned bridge ${action} HTTP${response.status}`);return response;
}
async function check(id,fn){
  try{const evidence=await fn();result.checks.push({id,pass:true,evidence});}
  catch(error){result.checks.push({id,pass:false,error:error.message});}
  await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));
}
async function wait(expression){for(let i=0;i<100;i++){if(await browser.evaluate(expression))return;await sleep(50);}throw Error('Browser did not reach '+expression.slice(0,120));}
async function view(width,height=900){
  await ui.evaluate(`window.__TAURI__.core.invoke('browser_surface_attach',{instance:'agent-vesper-job-observer',rect:{x:0,y:0,width:${width},height:${height}}})`);
  await browser.send('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:false});
  await browser.send('Emulation.setFocusEmulationEnabled',{enabled:true});
}
async function navigate(name,width=1440,reduce=false){
  phase=name;await view(width);await browser.send('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:reduce?'reduce':'no-preference'}]});
  const previous=await browser.evaluate('performance.timeOrigin');await browser.send('Page.navigate',{url:address});
  await wait(`performance.timeOrigin!==${previous}&&document.readyState==='complete'&&document.querySelectorAll('.experience-card').length===4`);
  await browser.evaluate('Promise.race([document.fonts.ready,new Promise(r=>setTimeout(r,3000))])');
}
async function click(selector){
  await browser.evaluate(`document.querySelector(${JSON.stringify(selector)}).scrollIntoView({block:'center',behavior:'instant'})`);await sleep(60);
  const p=await browser.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)}),r=e.getBoundingClientRect(),x=r.x+r.width/2,y=r.y+r.height/2;return{x,y,hit:e.contains(document.elementFromPoint(x,y))};})()`);
  assert.ok(p.hit,'Control is unobstructed: '+selector);
  for(const type of ['mousePressed','mouseReleased'])await browser.send('Input.dispatchMouseEvent',{type,x:p.x,y:p.y,button:'left',clickCount:1});
  await sleep(40);
}
async function key(name,modifiers=0){for(const type of ['keyDown','keyUp'])await browser.send('Input.dispatchKeyEvent',{type,key:name,code:name,windowsVirtualKeyCode:{Tab:9,Enter:13,Escape:27}[name],modifiers});await sleep(30);}
async function capture(name){
  const response=await bridge('capture',{instance:'agent-vesper-job-observer',targetId:surface.targetId,fullPage:false});
  assert.equal(response.headers.get('x-phoenix-target-id'),surface.targetId);
  const bytes=Buffer.from(await response.arrayBuffer());assert.ok(bytes.length>1000);assert.equal(bytes.subarray(1,4).toString(),'PNG');
  await writeFile(join(output,'shots',name+'.png'),bytes);result.screenshots.push({file:'shots/'+name+'.png',sha256:hash(bytes),bytes:bytes.length,phase});
}
const state=()=>browser.evaluate(`({count:document.querySelector('#plan-list').children.length,total:document.querySelector('#plan-total').textContent,ids:[...document.querySelectorAll('#plan-list [data-remove]')].map(e=>e.dataset.remove),status:document.querySelector('#plan-status').textContent,stored:localStorage.getItem('vesper-itinerary-v1')})`);
async function observe(wsUrl){
  const ws=new WebSocket(wsUrl);let id=0;const pending=new Map();
  await new Promise((r,j)=>{ws.addEventListener('open',r,{once:true});ws.addEventListener('error',j,{once:true});});
  const send=(method,params={})=>new Promise((r,j)=>{const n=++id,t=setTimeout(()=>{pending.delete(n);j(Error('Observer timed out'));},10000);pending.set(n,{r,j,t});ws.send(JSON.stringify({id:n,method,params}));});
  ws.addEventListener('message',event=>{const row=JSON.parse(String(event.data));if(row.id){const p=pending.get(row.id);if(p){clearTimeout(p.t);pending.delete(row.id);row.error?p.j(Error(row.error.message)):p.r(row.result);}return;}
    if(row.method==='Runtime.exceptionThrown')result.errors.push({phase,text:row.params.exceptionDetails.exception?.description||row.params.exceptionDetails.text});
    if(row.method==='Fetch.requestPaused'){
      const p=row.params,r=p.request,destination=new URL(r.url),allowed=['GET','HEAD'].includes(r.method)&&(destination.origin===url.origin||['data:','about:'].includes(destination.protocol)||['fonts.googleapis.com','fonts.gstatic.com'].includes(destination.hostname)&&['Stylesheet','Font'].includes(p.resourceType));
      result.requests.push({phase,url:r.url,method:r.method,type:p.resourceType,allowed});
      send(allowed?'Fetch.continueRequest':'Fetch.failRequest',{requestId:p.requestId,...(!allowed?{errorReason:'BlockedByClient'}:{})}).catch(error=>result.errors.push({phase,text:'Observer: '+error.message}));
    }
  });await send('Runtime.enable');await send('Fetch.enable',{patterns:[{urlPattern:'*'}]});
  return{async close(){await send('Fetch.disable');ws.close();}};
}
try{
  stop=await acceptanceBrowser({home,env,output});surface=await(await bridge('open',{instance:'agent-vesper-job-observer'})).json();
  const meta=JSON.parse(await readFile(join(output,'browser.json'))),targets=await fetch(`http://127.0.0.1:${meta.debugPort}/json/list`).then(r=>r.json());
  const target=targets.find(t=>t.id===surface.targetId),main=targets.find(t=>t.url.includes('/ui/index.html'));assert.ok(target&&main);
  browser=await connectCdp(target.webSocketDebuggerUrl);ui=await connectCdp(main.webSocketDebuggerUrl);observer=await observe(target.webSocketDebuggerUrl);
  await navigate('initial');assert.equal((await state()).count,0,'the disposable browser starts without an itinerary');await browser.evaluate('scrollTo({top:0,behavior:"instant"})');await capture('desktop-hero');
  for(const [filter,count] of [['telescope',2],['photography',1],['quiet',1],['all',4]])await check('filter-'+filter,async()=>{
    await click(`[data-filter="${filter}"]`);const n=await browser.evaluate(`document.querySelectorAll('.experience-card:not([hidden])').length`);assert.equal(n,count);return n;
  });
  const ids=['deep-field','red-room','silent-watch','first-light'],prices=[185,220,95,145];
  for(const [index,id]of ids.entries())await check('inspect-and-add-'+id,async()=>{
    await click(`[data-detail="${id}"]`);assert.equal(await browser.evaluate('document.querySelector("#experience-dialog").open'),true);
    if(index===0){await capture('desktop-detail');await browser.evaluate('document.querySelector(".dialog-close").focus()');await key('Tab',8);assert.equal(await browser.evaluate('document.querySelector("#experience-dialog").contains(document.activeElement)'),true);}
    await click(`[data-dialog-add="${id}"]`);assert.equal(await browser.evaluate('document.querySelector("#experience-dialog").open'),false);
    const s=await state();assert.equal(s.count,index+1);assert.equal(s.total,'$'+prices.slice(0,index+1).reduce((a,b)=>a+b,0));return s;
  });
  await check('duplicates-remain-single-selection',async()=>{await click('[data-add="deep-field"]');const s=await state();assert.equal(s.count,4);assert.equal(s.total,'$645');return s;});
  await check('reload-keeps-exact-plan',async()=>{await navigate('reload');const s=await state();assert.deepEqual(s.ids,ids);assert.equal(s.total,'$645');return s;});
  await check('remove-recomputes-exact-total',async()=>{await click('[data-remove="red-room"]');const s=await state();assert.deepEqual(s.ids,['deep-field','silent-watch','first-light']);assert.equal(s.total,'$425');return s;});
  await check('confirmation-is-local-only',async()=>{await click('#confirm-plan');const s=await state();assert.match(s.status,/confirmed locally.*3 sessions.*Nothing was sent or booked/);await capture('desktop-confirmation');return s;});
  await check('keyboard-escape-restores-inspect-focus',async()=>{await click('[data-detail="deep-field"]');await key('Escape');assert.equal(await browser.evaluate('document.querySelector("#experience-dialog").open'),false);assert.equal(await browser.evaluate('document.activeElement.dataset.detail'),'deep-field');});
  await check('reset-clears-storage-and-plan',async()=>{await click('#reset-plan');const s=await state();assert.equal(s.count,0);assert.equal(s.stored,null);return s;});
  for(const width of [390,820,1440]){
    await navigate('layout-'+width,width);
    await browser.evaluate('scrollTo({top:0,behavior:"instant"})');await sleep(50);
    await check('layout-width-'+width,async()=>{const size=await browser.evaluate('({width:innerWidth,scrollWidth:document.documentElement.scrollWidth})');assert.ok(size.scrollWidth<=size.width);return size;});
    await capture(`hero-${width}`);
    await check('loaded-imagery-'+width,async()=>{const bad=await browser.evaluate(`(async()=>{const bad=[];for(const img of document.images){img.loading='eager';await img.decode().catch(()=>bad.push(img.getAttribute('src')));}return bad;})()`);assert.deepEqual(bad,[]);return bad;});
    await click('[data-detail="silent-watch"]');await capture(`detail-${width}`);await key('Escape');
    await click('[data-add="silent-watch"]');await click('.itinerary-jump');await sleep(500);await capture(`plan-${width}`);await click('#reset-plan');
  }
  await navigate('motion');
  await check('normal-sky-actually-changes',async()=>{const before=await browser.evaluate('document.querySelector("#sky-canvas").toDataURL()');await sleep(350);const after=await browser.evaluate('document.querySelector("#sky-canvas").toDataURL()');assert.notEqual(hash(before),hash(after));});
  await check('changed-reduced-motion-removes-visible-animation',async()=>{await browser.send('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:'reduce'}]});await sleep(100);const s=await browser.evaluate('({canvas:getComputedStyle(document.querySelector("#sky-canvas")).display,scroll:getComputedStyle(document.documentElement).scrollBehavior})');assert.equal(s.canvas,'none');assert.equal(s.scroll,'auto');return s;});
  await navigate('motion-initial-reduced',390,true);await capture('reduced-mobile');
  await check('initial-reduced-content-readable',async()=>{assert.equal(await browser.evaluate('getComputedStyle(document.querySelector("#sky-canvas")).display'),'none');assert.ok(await browser.evaluate('document.querySelector("h1").getBoundingClientRect().width>0'));});
  phase='persistence-duplicate-fixture';await browser.evaluate(`localStorage.setItem('vesper-itinerary-v1',JSON.stringify(['deep-field','deep-field','unknown-id']))`);await navigate(phase);
  await check('malformed-persisted-duplicates-do-not-double-price',async()=>{const s=await state();assert.equal(s.count,1);assert.equal(s.total,'$185');return s;});
  await click('#reset-plan');phase='persistence-denied-fixture';
  await check('storage-denial-keeps-state-consistent-and-explains-failure',async()=>{
    const s=await browser.evaluate(`(()=>{const original=Storage.prototype.setItem,errors=[];const onerror=e=>{errors.push(e.message);e.preventDefault()};window.addEventListener('error',onerror);Storage.prototype.setItem=function(k,v){if(k==='vesper-itinerary-v1')throw new DOMException('Fixture quota exhausted','QuotaExceededError');return original.call(this,k,v)};try{document.querySelector('[data-add="deep-field"]').click();}finally{Storage.prototype.setItem=original;window.removeEventListener('error',onerror)}const first={count:document.querySelector('#plan-list').children.length,status:document.querySelector('#plan-status').textContent};document.querySelector('[data-add="deep-field"]').click();return{errors,first,after:{count:document.querySelector('#plan-list').children.length,status:document.querySelector('#plan-status').textContent}};})()`);
    assert.equal(s.errors.length,0,JSON.stringify(s));assert.equal(s.after.count,1,JSON.stringify(s));assert.match(s.first.status,/save|storage|browser|persist|retained/i);return s;
  });
  // The denied-write fixture can deliberately leave the original app's DOM
  // and memory inconsistent, hiding its reset button. Restore only observer
  // storage and reload before the remaining independent integrity checks.
  await browser.evaluate(`localStorage.removeItem('vesper-itinerary-v1')`);
  await navigate('observer-cleanup');
  await check('no-unexpected-runtime-errors',async()=>{const unexpected=result.errors.filter(e=>e.phase!=='persistence-denied-fixture');assert.deepEqual(unexpected,[]);});
  await check('no-external-submission-attempt',async()=>{assert.deepEqual(result.requests.filter(r=>!r.allowed),[]);return{requests:result.requests.length};});
  await check('source-and-served-code-unchanged',async()=>{for(const row of result.sourceHashes){assert.equal(hash(await readFile(join(source,row.file))),row.sha256);assert.equal(hash(Buffer.from(await fetch(new URL(row.file,url)).then(r=>r.arrayBuffer()))),row.sha256);}return result.sourceHashes;});
}catch(error){fatal=String(error.stack||error);result.fatal=fatal;}
finally{
  const cleanupErrors=[];try{await observer?.close();}catch(error){cleanupErrors.push(String(error));}browser?.close();ui?.close();try{await stop?.();}catch(error){cleanupErrors.push(String(error));}
  result.cleanupErrors=cleanupErrors;result.finishedAt=new Date().toISOString();result.counts={passed:result.checks.filter(c=>c.pass).length,failed:result.checks.filter(c=>!c.pass).length};
  await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));console.log(JSON.stringify({output,...result.counts,fatal,cleanupErrors}));
  if(fatal||cleanupErrors.length||result.counts.failed)process.exitCode=1;
}
