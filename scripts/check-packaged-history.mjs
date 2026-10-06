// Open the real built desktop with stopped test-company history in an isolated
// display/home. No new task is submitted and no user profile is opened.
import assert from 'node:assert/strict';
import net from 'node:net';
import {spawn} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {cp,lstat,mkdir,open,readFile,readdir,writeFile} from 'node:fs/promises';
import {join,dirname} from 'node:path';
import {withAcceptanceGateway,once} from './lib/acceptance-gateway.mjs';
import {connectCdp} from './lib/acceptance-browser.mjs';
const [binary,desktop,source,output]=process.argv.slice(2);
if(![binary,desktop,source,output].every(path=>path?.startsWith('/')))throw Error('Absolute gateway, desktop, stopped evidence and NEW output paths required');
const previous=JSON.parse(await readFile(join(source,'execution.json')));
const submission=JSON.parse(await readFile(join(source,'submission.json')));
if(previous.gateway?.code!==0||!previous.credentialRemoved||!previous.durableState||!submission.group)throw Error('Require a clean stopped test group snapshot');
await mkdir(output,{mode:0o700});
async function portAvailable(port){
  const server=net.createServer();
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(port,'127.0.0.1',resolve);});
  const selected=server.address().port;
  await new Promise((resolve,reject)=>server.close(error=>error?reject(error):resolve()));return selected;
}
// These are fixed by the actual desktop; refuse to disturb an existing app.
await portAvailable(17442);await portAvailable(17443);
const websocketPort=await portAvailable(0);
await withAcceptanceGateway({binary,output,workspace:output,authSource:'codex',setup:async({home,env})=>{
  env.PHOENIX_WS_PORT=String(websocketPort);
  for(const name of ['company','sessions','cas']){
    const path=join(previous.durableState,name);
    if(await lstat(path).catch(error=>{if(error.code==='ENOENT')return null;throw error;}))await cp(path,join(home,name),{recursive:true,errorOnExist:true,force:false});
  }
}},async({home,env,socketPath})=>{
  // Complete only this disposable company's setup through its public API.
  // Accepting the configured route performs no live provider probe; no browser
  // cookies are imported and the temporary vault contains no credentials.
  for(const action of ['accept_detected_provider','skip_default_account_email','skip_cookie_import','review_company_defaults','review_powers_setup'])
    await once(socketPath,{Onboarding:{action}});
  await once(socketPath,{Vault:{action:'initialize',master_password:randomBytes(32).toString('hex')}});
  await once(socketPath,{Vault:{action:'lock'}});
  const setup=await once(socketPath,{Onboarding:{action:'complete'}});
  assert.ok(setup.Onboarding?.complete,'private setup completed without a provider request');
  const log=await open(join(output,'desktop.log'),'wx',0o600);
  const child=spawn('xvfb-run',['--auto-servernum','--server-args=-screen 0 1440x960x24 -nolisten tcp',desktop],{
    detached:true,cwd:dirname(desktop),stdio:['ignore',log.fd,log.fd],
    env:{...env,PATH:dirname(process.execPath)+':'+env.PATH,PHOENIX_GATEWAY_BINARY:binary,
      GDK_BACKEND:'x11',WEBKIT_DISABLE_DMABUF_RENDERER:'1',PHOENIX_CHROMIUM_OZONE_PLATFORM:'x11'}});
  const done=new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));});done.catch(()=>{});
  let ui;
  try{
    let target;
    for(let i=0;i<150;i++){
      if(child.exitCode!==null||child.signalCode!==null)throw Error('Built desktop exited during startup; inspect desktop.log');
      try{const targets=await fetch('http://127.0.0.1:17442/json/list',{signal:AbortSignal.timeout(500)}).then(response=>response.json());target=targets.find(row=>row.type==='page'&&row.url.includes('/ui/index.html'));if(target)break;}catch{}
      await new Promise(resolve=>setTimeout(resolve,200));
    }
    assert.ok(target,'actual Electron UI appeared');
    ui=await connectCdp(target.webSocketDebuggerUrl);
    await ui.evaluate(`(async()=>{for(let i=0;i<150;i++){if(window.PhoenixUI?.state?.view&&window.PhoenixConversation)return true;await new Promise(r=>setTimeout(r,100));}throw Error('Desktop services not ready');})()`);
    await ui.evaluate(`void window.PhoenixUI.selectItem({kind:'group',id:${JSON.stringify(submission.group.group_id)}})`);
    await ui.evaluate(`(async()=>{for(let i=0;i<150;i++){if(document.getElementById('stageName')?.textContent.includes('Tideline')&&!document.querySelector('.conversation-loading'))return true;await new Promise(r=>setTimeout(r,100));}throw Error('Saved group did not load');})()`);
    const observed=await ui.evaluate(`(()=>{const feed=document.getElementById('conversationFeed');feed.scrollTop=feed.scrollHeight;const rect=feed.getBoundingClientRect(),top=document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2);return {name:document.getElementById('stageName')?.textContent,text:feed.innerText,active:feed.querySelectorAll('.work-cluster.running,.team-work-block.working').length,teams:feed.querySelectorAll('.team-work-block').length,feedUnobstructed:feed.contains(top),onboardingHidden:document.getElementById('onboardingView')?.hidden===true,sidebar:document.getElementById('sidebar')?.textContent||'',width:innerWidth,scrollWidth:document.documentElement.scrollWidth};})()`);
    const screenshot=await ui.send('Page.captureScreenshot',{format:'png',fromSurface:true});
    await writeFile(join(output,'desktop.png'),Buffer.from(screenshot.data,'base64'));
    const calls=(await readdir(join(home,'runs'))).filter(name=>name.startsWith('run_'));
    const checks={savedGroupVisible:observed.name.includes('Tideline'),actualHistoryVisible:/TIDELINE|frontend|Iris/.test(observed.text),feedUnobstructed:observed.feedUnobstructed,onboardingHidden:observed.onboardingHidden,noLiveWork:observed.active===0,noModelRuns:calls.length===0,windowFits:observed.scrollWidth<=observed.width};
    await writeFile(join(output,'result.json'),JSON.stringify({checks,observed,source,modelRuns:calls},null,2));
    console.log(JSON.stringify({checks,name:observed.name,teams:observed.teams}));
    if(Object.values(checks).some(value=>!value))process.exitCode=1;
  }finally{
    ui?.close();
    if(child.exitCode===null&&child.signalCode===null){try{process.kill(-child.pid,'SIGTERM');}catch(error){if(error.code!=='ESRCH')throw error;}
      const timer=setTimeout(()=>{try{process.kill(-child.pid,'SIGKILL');}catch{}},5000);try{await done;}finally{clearTimeout(timer);}}
    await log.close();
  }
});
