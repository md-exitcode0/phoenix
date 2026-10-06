// Exercise the actual Electron profile, download and screenshot destinations.
import assert from 'node:assert/strict';
import http from 'node:http';
import {mkdir,readFile,readdir,stat,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {withAcceptanceGateway,once} from './lib/acceptance-gateway.mjs';
import {acceptanceBrowser,connectCdp} from './lib/acceptance-browser.mjs';
const [binary,output]=process.argv.slice(2);
if(![binary,output].every(path=>path?.startsWith('/')))throw Error('Absolute binary and NEW output paths required');
await mkdir(output,{mode:0o700});
await withAcceptanceGateway({binary,output,workspace:output,authSource:'codex',setup:args=>acceptanceBrowser({...args,output})},async({home,socketPath,env})=>{
  const server=http.createServer((req,res)=>{
    if(req.url.startsWith('/download')){const name=new URL(req.url,'http://fixture').searchParams.get('case')||'isolated-check';res.writeHead(200,{'content-type':'application/octet-stream','content-disposition':`attachment; filename="${name}.txt"`});res.end('isolated download');}
    else{res.setHeader('content-type','text/html');res.end('<!doctype html><title>Isolated browser check</title><h1>Real browser output</h1><a href="/download" download>Download</a>');}
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  let browser,ui,worker;
  try{
    const opened=await once(socketPath,{BrowserSurface:{instance:'agent-coder',action:'open'}},20000);
    assert.ok(!opened.Error);
    const meta=JSON.parse(await readFile(join(output,'browser.json')));
    const targets=await fetch(`http://127.0.0.1:${meta.debugPort}/json/list`).then(r=>r.json());
    const main=targets.find(t=>t.type==='page'&&t.url.includes('/ui/index.html'));
    const target=targets.find(t=>t.type==='page'&&t.id!==main?.id);
    assert.ok(main&&target,'both actual shell and embedded browser exist');
    browser=await connectCdp(target.webSocketDebuggerUrl);ui=await connectCdp(main.webSocketDebuggerUrl);
    await browser.send('Page.navigate',{url:`http://127.0.0.1:${server.address().port}/`});
    for(let i=0;i<60;i++){if(await browser.evaluate('document.title')==='Isolated browser check')break;await new Promise(r=>setTimeout(r,50));}
    assert.equal(await browser.evaluate('document.querySelector("h1")?.textContent'),'Real browser output');
    await browser.evaluate('document.querySelector("a").click()');
    const download=join(home,'downloads/agent-coder/isolated-check.txt');
    let content;
    for(let i=0;i<100;i++){try{content=await readFile(download,'utf8');break;}catch{}await new Promise(r=>setTimeout(r,50));}
    assert.equal(content,'isolated download');
    let hiddenCaptureRejected=false;
    try{
      const hidden=await ui.evaluate('window.__TAURI__.core.invoke("browser_surface_screenshot",{instance:"agent-coder"})');
      assert.ok((await stat(hidden.path)).size>0,'a successful hidden capture cannot be empty');
    }catch(error){
      if(!String(error).includes('no rendered frame'))throw error;
      hiddenCaptureRejected=true;
      assert.equal((await readdir(join(home,'downloads/agent-coder'))).filter(name=>name.endsWith('.png')).length,0,'failed capture leaves no fake image');
    }
    await ui.evaluate('window.__TAURI__.core.invoke("browser_surface_attach",{instance:"agent-coder",rect:{x:0,y:0,width:800,height:600}})');
    await browser.evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
    const screenshot=await ui.evaluate('window.__TAURI__.core.invoke("browser_surface_screenshot",{instance:"agent-coder"})');
    assert.ok(resolve(screenshot.path).startsWith(home+'/downloads/agent-coder/'));
    const bytes=await readFile(screenshot.path);assert.ok(bytes.length>1000);assert.equal(bytes.subarray(1,4).toString(),'PNG');
    assert.ok((await stat(join(home,'chromium-shell/Partitions'))).isDirectory());
    const bridge=async(action,body)=>{
      const endpoint=new URL('browser/'+action,env.PHOENIX_CHROMIUM_BRIDGE_URL);endpoint.searchParams.set('token',env.PHOENIX_CHROMIUM_BRIDGE_TOKEN);
      const response=await fetch(endpoint,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)}).catch(()=>{throw Error('Owned test bridge unavailable');});
      assert.ok(response.ok,'test bridge action accepted');return response.json();
    };
    const workerId='agent-coder-job-download-proof';
    const workerSurface=await bridge('open',{instance:workerId,profileOwner:'agent-coder'});
    const all=await fetch(`http://127.0.0.1:${meta.debugPort}/json/list`).then(r=>r.json());
    worker=await connectCdp(all.find(row=>row.id===workerSurface.targetId).webSocketDebuggerUrl);
    await browser.evaluate('document.cookie="shared_fixture=yes; path=/"');
    await worker.send('Page.navigate',{url:`http://127.0.0.1:${server.address().port}/`});
    for(let i=0;i<60;i++){if(await worker.evaluate('document.title')==='Isolated browser check')break;await new Promise(r=>setTimeout(r,50));}
    assert.ok((await worker.evaluate('document.cookie')).includes('shared_fixture=yes'),'worker truly shares the parent session');
    async function routedDownload(client,owner,name,other){
      await client.evaluate(`(()=>{const a=document.querySelector('a');a.href='/download?case=${name}';a.click();})()`);
      const expected=join(home,'downloads',owner,name+'.txt');let body;
      for(let i=0;i<100;i++){try{body=await readFile(expected,'utf8');break;}catch{}await new Promise(r=>setTimeout(r,50));}
      assert.equal(body,'isolated download',`${name} arrives in its originating surface`);
      await assert.rejects(stat(join(home,'downloads',other,name+'.txt')),error=>error.code==='ENOENT');
    }
    await routedDownload(browser,'agent-coder','parent-after-worker-open',workerId);
    await routedDownload(worker,workerId,'worker-download','agent-coder');
    worker.close();worker=null;
    await bridge('discard',{instance:workerId});
    await routedDownload(browser,'agent-coder','parent-after-worker-close',workerId);
    const result={realBrowser:true,downloadInSelectedHome:true,screenshotInSelectedHome:true,partitionInSelectedHome:true,
      sharedAuthentication:true,downloadsFollowOrigin:true,closedWorkerCannotRedirectParent:true,hiddenCaptureRejected,modelCalls:0};
    await writeFile(join(output,'browser.png'),bytes);
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));console.log(JSON.stringify(result));
  }finally{browser?.close();ui?.close();worker?.close();await new Promise(resolve=>server.close(resolve));}
});
