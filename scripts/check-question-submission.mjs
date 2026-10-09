// Disposable Chromium fixture: real Enter/form events, no gateway or agent work.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,readFile,rm} from 'node:fs/promises';
import {createServer} from 'node:http';
import {tmpdir} from 'node:os';
import {join,resolve,sep,extname} from 'node:path';
import {fileURLToPath} from 'node:url';
const root=resolve(fileURLToPath(new URL('../monocode/ui/',import.meta.url)));
const profile=await mkdtemp(join(tmpdir(),'phoenix-question-test-'));
const types={'.html':'text/html','.js':'text/javascript','.mjs':'text/javascript','.css':'text/css','.json':'application/json','.png':'image/png','.svg':'image/svg+xml'};
const server=createServer(async(req,res)=>{
  try{
    const pathname=new URL(req.url,'http://localhost').pathname;
    if(pathname==='/fixture-state'){res.setHeader('content-type','application/json');res.end('{"profiles":{},"images":{},"commands":[]}');return;}
    const path=resolve(root,'.'+(pathname==='/'?'/index.html':pathname));
    if(!path.startsWith(root+sep)){res.writeHead(403);res.end();return;}
    let body=await readFile(path);
    if(process.env.PHOENIX_QUESTION_BASELINE&&['/conversation.js','/sidebar.js'].includes(pathname))body=execFileSync('git',['show',(process.env.PHOENIX_QUESTION_BASELINE==='1'?'HEAD':process.env.PHOENIX_QUESTION_BASELINE)+':monocode/ui'+pathname],{cwd:resolve(root,'../..'),maxBuffer:2*1024*1024});
    if(pathname==='/index.html')body=Buffer.from(body.toString().replace('<head>','<head><script>window.__nativeHandlers=new Map();window.__TAURI__={event:{listen:(name,fn)=>{__nativeHandlers.set(name,fn);return Promise.resolve(()=>{})}}};</script>'));
    if(pathname==='/conversation.js')body=Buffer.from(body.toString().replace('  bind(); renderVoiceState();','  window.__questions={state,renderApproval,closeApproval,renderQueue,submitAsk,unlockApprovalVault,setRpc(fn){rpc=fn}};\n  bind(); renderVoiceState();'));
    res.setHeader('content-type',types[extname(path)]||'application/octet-stream');res.end(body);
  }catch{res.writeHead(404);res.end();}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const chrome=spawn('google-chrome',['--headless=new','--disable-gpu','--no-sandbox','--remote-debugging-port=0',`--user-data-dir=${profile}`,'about:blank'],{stdio:['ignore','ignore','pipe']});
let ws;
try{
  const endpoint=await new Promise((resolve,reject)=>{let output='';const timer=setTimeout(()=>reject(Error('Chrome startup timed out')),15000);chrome.stderr.on('data',chunk=>{output+=chunk;const match=output.match(/DevTools listening on (ws:\/\/[^\s]+)/);if(match){clearTimeout(timer);resolve(match[1]);}});chrome.on('error',reject);});
  const url=new URL(endpoint),targets=await(await fetch(`http://${url.host}/json/list`)).json();
  ws=new WebSocket(targets.find(t=>t.type==='page').webSocketDebuggerUrl);
  await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j;});
  let serial=0;const pending=new Map();
  ws.onmessage=event=>{const response=JSON.parse(event.data);if(response.id){pending.get(response.id)?.(response);pending.delete(response.id);}};
  const call=async(method,params={})=>{const id=++serial;const response=await new Promise(r=>{pending.set(id,r);ws.send(JSON.stringify({id,method,params}));});if(response.error)throw Error(JSON.stringify(response.error));return response.result;};
  const evaluate=async expression=>{const result=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(result.exceptionDetails)throw Error(result.exceptionDetails.exception?.description||result.exceptionDetails.text);return result.result.value;};
  const until=async expression=>{for(let i=0;i<80;i++){if(await evaluate(expression))return;await new Promise(r=>setTimeout(r,50));}throw Error('Timed out: '+expression);};
  await call('Page.navigate',{url:`http://127.0.0.1:${server.address().port}/index.html?shot=question-routing-fixture`});
  await until('!!window.__questions&&!!PhoenixUI.state.view');
  await evaluate(`(async()=>{
    const view=PhoenixUI.state.view,p=view.directory.agents[0],a=view.activities[0];
    view.directory.agents.push({...p,agent_id:'school_coach',internal_role:'school_coach',display_name:'Avery',canonical_session_id:'agent-school_coach',browser_profile_id:'agent-school_coach'});
    view.activities.push({...a,item:{kind:'agent',id:'school_coach'},canonical_session_id:'agent-school_coach',status:'idle',unread:false});
    await PhoenixUI.selectItem({kind:'agent',id:'school_coach'});
    __questions.closeApproval();
    window.__requests=[];window.__replies=[];
    __questions.setRpc(request=>new Promise(resolve=>{__requests.push(request);__replies.push(resolve)}));
    window.__ask=(id,approval)=>__questions.renderApproval({id,agent:'school_coach',status:'pending',questions:[{question:'Fixture question',options:[],multi_select:false}],...(approval?{approval}: {})});
    __ask('ordinary');
    document.querySelector('.pending-question-disclosure').open=true;
    const input=document.querySelector('.approval-inline-custom [data-ask-custom-input]');input.value='Fixture answer';input.dispatchEvent(new Event('input',{bubbles:true}));input.focus();
  })()`);
  await call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,text:'\r',unmodifiedText:'\r'});
  await call('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});
  await until('__requests.length===1');
  await evaluate(`document.querySelector('.approval-inline-custom [data-ask-custom-input]').dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',repeat:true,bubbles:true}));void __questions.submitAsk(document.querySelector('.approval-card'),'Fixture answer')`);
  const ordinary=await evaluate('__requests');
  assert.equal(ordinary.length,1,'held Enter and overlapping submit send once');
  assert.equal(ordinary[0].AnswerAsk.owner.id,'school_coach');
  assert.equal(ordinary[0].AnswerAsk.session_id,'agent-school_coach');
  await evaluate(`__nativeHandlers.get('open-conversation')({payload:{kind:'agent',id:'phoenix'}})`);
  assert.equal(await evaluate('PhoenixUI.state.selected.id'),'school_coach','notification cannot switch during submission');
  assert.equal(await evaluate('!!document.querySelector(".toast.actionable")'),true,'notification remains explicitly accessible');
  await evaluate(`__replies.shift()({AskAnswered:{disposition:'delivered'}})`);
  await until('!document.querySelector(".approval-card")');
  assert.equal(await evaluate('PhoenixUI.state.selected.id'),'school_coach');

  await evaluate(`__requests.length=0;__ask('vault',{action:'vault_unlock',approved_option:'Unlock here'});document.querySelector('[data-vault-open]').click();const input=document.querySelector('[data-vault-password]');input.value='fixture-password-only';input.focus();`);
  await call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,text:'\r',unmodifiedText:'\r'});
  await call('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});
  await until('__requests.length===1');
  await evaluate(`document.querySelector('.approval-vault').dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));void __questions.unlockApprovalVault(document.querySelector('.approval-card'));`);
  assert.equal(await evaluate('__requests.length'),1,'overlapping form events unlock once');
  await evaluate(`__nativeHandlers.get('open-conversation')({payload:{kind:'agent',id:'phoenix'}});__replies.shift()({Vault:{result:'unlocked'}});`);
  await until('__requests.length===2');
  assert.equal(await evaluate('__requests[1].AnswerAsk.owner.id'),'school_coach');
  assert.equal(await evaluate('PhoenixUI.state.selected.id'),'school_coach');
  assert.equal(await evaluate('document.querySelector("[data-vault-password]").value'),'','secret is wiped before continuation');
  await evaluate(`__replies.shift()({AskAnswered:{disposition:'late_answer_queued:fixture'}})`);
  await until('!document.querySelector(".approval-card")');
  await evaluate(`__questions.state.queue=[{queue_id:'fixture',state:'queued',preview:'Unlock here',origin:{kind:'ask_answer',ask_id:'vault'}}];__questions.renderQueue();`);
  assert.equal(await evaluate('document.getElementById("queueBlock").hidden'),true,'shown answer is not duplicated in queue');
  await evaluate(`__questions.state.queue[0].state='failed';__questions.renderQueue();`);
  assert.equal(await evaluate('document.getElementById("queueBlock").hidden'),false,'failed continuation stays reviewable');

  await evaluate(`__requests.length=0;__ask('stale-vault',{action:'vault_unlock',approved_option:'Unlock here'});document.querySelector('[data-vault-open]').click();document.querySelector('[data-vault-password]').value='fixture-password-only';window.__staleCard=document.querySelector('.approval-card');void __questions.unlockApprovalVault(__staleCard);`);
  await until('__requests.length===1');
  await evaluate(`__questions.state.loadGeneration++;__questions.state.item={kind:'agent',id:'phoenix'};__questions.state.sessionId='company-phoenix';__replies.shift()({Vault:{result:'unlocked'}});`);
  await until('!__staleCard.hasAttribute("aria-busy")');
  assert.equal(await evaluate('__requests.length'),1,'switch during unlock cannot answer another owner');
  assert.equal(await evaluate('[...__staleCard.querySelectorAll("button,input")].every(n=>!n.disabled)'),true,'cached card can be retried on return');
  console.log('PASS: Enter/form single delivery, Avery ownership, notification focus, secret wiping, queue deduplication, failure review and stale selection.');
}finally{
  ws?.close();chrome.kill();server.close();
  await new Promise(r=>chrome.exitCode!==null?r():chrome.once('exit',r));
  await rm(profile,{recursive:true,force:true,maxRetries:5,retryDelay:150});
}
