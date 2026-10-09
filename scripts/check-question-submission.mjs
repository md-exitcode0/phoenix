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
    if(pathname==='/conversation.js')body=Buffer.from(body.toString().replace('  bind(); renderVoiceState();','  window.__questions={state,renderStory,renderDisplayEntry,repaintOwnedTurn,reconcileHistory,renderAskAnswerTurn,stopTurn,hideInspectionSidebar,showInspectionSidebar,renderApproval,closeApproval,renderQueue,submitAsk,unlockApprovalVault,setRpc(fn){rpc=fn}};\n  bind(); renderVoiceState();'));
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
  await call('Page.navigate',{url:`http://127.0.0.1:${server.address().port}/index.html?skin=phoenix&shot=question-routing-fixture`});
  await until('!!window.__questions&&!!PhoenixUI.state.view');
  await evaluate(`(async()=>{
    const view=PhoenixUI.state.view,p=view.directory.agents[0],a=view.activities[0];
    view.directory.agents.push({...p,agent_id:'school_coach',internal_role:'school_coach',display_name:'Avery',canonical_session_id:'agent-school_coach',browser_profile_id:'agent-school_coach'});
    view.activities.push({...a,item:{kind:'agent',id:'school_coach'},canonical_session_id:'agent-school_coach',status:'idle',unread:false});
    await PhoenixUI.selectItem({kind:'agent',id:'school_coach'});
    __questions.closeApproval();
    window.__requests=[];window.__replies=[];
    __questions.setRpc(request=>new Promise(resolve=>{__requests.push(request);__replies.push({request,resolve})}));
    window.__respond=(key,reply)=>{const i=__replies.findIndex(p=>p.request[key]);if(i<0)throw Error('No pending '+key);__replies.splice(i,1)[0].resolve(reply)};
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
  await evaluate(`__respond('AnswerAsk',{AskAnswered:{disposition:'delivered'}})`);
  await until('!document.querySelector(".approval-card")');
  assert.equal(await evaluate('PhoenixUI.state.selected.id'),'school_coach');

  await evaluate(`__requests.length=0;__ask('vault',{action:'vault_unlock',approved_option:'Unlock here'});document.querySelector('[data-vault-open]').click();const input=document.querySelector('[data-vault-password]');input.value='fixture-password-only';input.focus();`);
  await call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,text:'\r',unmodifiedText:'\r'});
  await call('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});
  await until('__requests.length===1');
  await evaluate(`document.querySelector('.approval-vault').dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));void __questions.unlockApprovalVault(document.querySelector('.approval-card'));`);
  assert.equal(await evaluate('__requests.length'),1,'overlapping form events unlock once');
  await evaluate(`__nativeHandlers.get('open-conversation')({payload:{kind:'agent',id:'phoenix'}});__respond('Vault',{Vault:{result:'unlocked'}});`);
  await until('__requests.length===2');
  assert.equal(await evaluate('__requests[1].AnswerAsk.owner.id'),'school_coach');
  assert.equal(await evaluate('PhoenixUI.state.selected.id'),'school_coach');
  assert.equal(await evaluate('document.querySelector("[data-vault-password]").value'),'','secret is wiped before continuation');
  await evaluate(`__respond('AnswerAsk',{AskAnswered:{disposition:'late_answer_queued:fixture'}})`);
  await until('!document.querySelector(".approval-card")');
  await evaluate(`__questions.state.queue=[{queue_id:'fixture',state:'queued',preview:'Unlock here',origin:{kind:'ask_answer',ask_id:'vault'}}];__questions.renderQueue();`);
  assert.equal(await evaluate('document.getElementById("queueBlock").hidden'),true,'shown answer is not duplicated in queue');
  await evaluate(`__questions.state.queue[0].state='failed';__questions.renderQueue();`);
  assert.equal(await evaluate('document.getElementById("queueBlock").hidden'),false,'failed continuation stays reviewable');

  await evaluate(`document.documentElement.dataset.conversationView='compact';__questions.renderStory({kind:'tool',agent:'school_coach',tool:'str_replace',call_id:'large-diff-fixture',target:'fixture.txt',ok:true,diff:'@@ -1,1200 +1,1200 @@\\n'+Array.from({length:1200},(_,i)=>'+fixture line '+i).join('\\n')});window.__diffRow=[...document.querySelectorAll('.work-tool')].find(row=>row.dataset.toolCallId==='large-diff-fixture');`);
  assert.equal(await evaluate('__diffRow.querySelectorAll(".fd-line").length'),0,'collapsed large diffs do not populate the DOM');
  await evaluate(`__diffRow.closest('details').open=true;__diffRow.querySelector('.work-tool-summary').click();`);
  assert.equal(await evaluate('__diffRow.querySelectorAll(".fd-line").length'),1200,'opening a diff retains every line');
  assert.equal(await evaluate('__diffRow.querySelector(".tool-detail-copy").getAttribute("aria-label")'),'Copy diff','copy remains available after opening');
  assert.equal(await evaluate(`(()=>{const cluster=__diffRow.closest('.work-cluster'),trash=cluster.querySelector('.work-delete-button').getBoundingClientRect(),summary=__diffRow.closest('details').querySelector('summary').getBoundingClientRect();return trash.left>=summary.right})()`),true,'delete control cannot cover the disclosure');
  await evaluate(`__diffRow.querySelector('.work-tool-summary').click();`);

  await evaluate(`__requests.length=0;__questions.hideInspectionSidebar();for(let n=0;n<10;n++)__questions.renderStory({kind:'tool_start',agent:'school_coach',tool:'browser_act',call_id:'browser-fixture-'+n,target:'fixture.test'});`);
  assert.equal(await evaluate('document.body.classList.contains("inspection-open")'),false,'agent browser clicks cannot reopen a closed panel');
  assert.equal(await evaluate('__requests.length'),0,'agent browsing does not activate a browser surface');
  await evaluate(`__questions.showInspectionSidebar('desktop')`);
  assert.equal(await evaluate('document.body.classList.contains("inspection-open")'),true,'explicit user control still opens the workspace');
  await evaluate(`__questions.hideInspectionSidebar();__replies.splice(0).forEach(({resolve})=>resolve({DesktopWorkspaces:[],Pong:null}));`);

  await evaluate(`const reviewer=PhoenixUI.activityFor({kind:'agent',id:'critic'}),parent=PhoenixUI.activityFor({kind:'agent',id:'school_coach'});reviewer.status='idle';parent.status='working';parent.active_agent_ids=['school_coach','critic'];window.__delegate={agentId:'critic',sessionId:parent.canonical_session_id,execution:{turn_id:'review-fixture',task_id:'review-fixture',attempt_id:'review-attempt'}};PhoenixFluffies.activity.begin({...__delegate,sequence:1});PhoenixFluffies.activity.ingest({...__delegate,sequence:2,kind:'tool_start',event:{tool:'read',call_id:'review'}});dispatchEvent(new CustomEvent('phoenix:directory-status'));`);
  assert.equal(await evaluate('PhoenixFluffies.activity.snapshot("critic").mode'),'coding','an actively delegated coworker is not cleared by an idle private chat');
  await evaluate(`PhoenixUI.activityFor({kind:'agent',id:'school_coach'}).active_agent_ids=['school_coach'];dispatchEvent(new CustomEvent('phoenix:directory-status'));`);
  assert.equal(await evaluate('PhoenixFluffies.activity.snapshot("critic").mode'),'idle','returned reviewer goes idle while the parent keeps working');

  await evaluate(`window.__oldTurn=[{source:'history',turn_id:'old-receipt-fixture',value:{role:'user',text:'Old request'}},{source:'history',turn_id:'old-receipt-fixture',value:{role:'tool',agent:'school_coach',tool:'read',target:'old.txt',detail:'Read old file',ok:true}},{source:'history',turn_id:'old-receipt-fixture',value:{role:'answer',agent:'school_coach',text:'Old answer'}}];__questions.state.displayRows.push(...__oldTurn);__oldTurn.forEach(__questions.renderDisplayEntry);const current={source:'history',turn_id:'current-receipt-fixture',value:{role:'user',text:'Current request'}};__questions.state.displayRows.push(current);__questions.renderDisplayEntry(current);window.__currentPrompt=[...document.querySelectorAll('.user-message')].at(-1);__ask('repaint-question');document.querySelector('.pending-question-disclosure').open=true;window.__draftInput=document.querySelector('.approval-inline-custom [data-ask-custom-input]');__draftInput.value='Still composing my answer';__draftInput.focus();window.__unrelatedRemoved=false;window.__receiptObserver=new MutationObserver(records=>{if(records.some(r=>[...r.removedNodes].includes(__currentPrompt)))__unrelatedRemoved=true});__receiptObserver.observe(document.getElementById('conversationFeed'),{childList:true});__questions.repaintOwnedTurn('old-receipt-fixture');__questions.repaintOwnedTurn('old-receipt-fixture');`);
  await evaluate(`new Promise(resolve=>setTimeout(resolve,0))`);
  assert.equal(await evaluate('__unrelatedRemoved'),false,'late receipts leave the current conversation attached');
  assert.equal(await evaluate('document.activeElement===__draftInput&&__draftInput.value==="Still composing my answer"'),true,'late receipt repairs preserve the focused unsent answer');
  await evaluate(`__receiptObserver.disconnect();__questions.closeApproval();`);

  await evaluate(`__questions.state.working=true;__requests.length=0;void __questions.stopTurn();void __questions.stopTurn();`);
  await until('__requests.some(request=>request.QueuedTurns)');
  await evaluate(`__respond('QueuedTurns',{QueuedTurns:[{queue_id:'waiting-unlock',state:'queued',target_agent:'school_coach',origin:{kind:'ask_answer'}},{queue_id:'authored-prompt',state:'queued'},{queue_id:'other-agent-answer',state:'queued',target_agent:'critic',origin:{kind:'ask_answer'}},{queue_id:'running-answer',state:'running',origin:{kind:'ask_answer'}}]});`);
  await until('__requests.some(request=>request.CancelQueuedTurn)');
  assert.equal(await evaluate('__requests.some(request=>request.Cancel)'),false,'pending unlock is retired before Stop releases the active lane');
  assert.equal(await evaluate('__requests.find(request=>request.CancelQueuedTurn).CancelQueuedTurn.queue_id'),'waiting-unlock','Stop only retires pending question continuations for this owner');
  await evaluate(`__respond('CancelQueuedTurn',{Done:{completion:'canceled'}});`);
  await until('__requests.some(request=>request.Cancel)');
  assert.equal(await evaluate('__requests.filter(request=>request.QueuedTurns).length'),1,'repeated Stop is single flight');
  assert.equal(await evaluate('__requests.find(request=>request.Cancel).Cancel.target_agent'),'school_coach','Stop keeps the captured owner');
  await evaluate(`__respond('Cancel',{Done:{completion:'canceled',route:'cancel',main_session_id:'agent-school_coach'}});`);
  await until('!__questions.state.working');

  await evaluate(`{__requests.length=0;__ask('placement-fixture');const newer={source:'history',turn_id:'newer-message-fixture',value:{role:'answer',text:'A newer message before the reply'}};__questions.state.displayRows.push(newer);__questions.renderDisplayEntry(newer);window.__newerMessage=[...document.querySelectorAll('#conversationFeed > .agent-message')].at(-1);document.querySelector('.pending-question-disclosure').open=true;const input=document.querySelector('.approval-inline-custom [data-ask-custom-input]');input.value='My saved reply';input.dispatchEvent(new Event('input',{bubbles:true}));void __questions.submitAsk(document.querySelector('.approval-card'),'My saved reply');}`);
  await until('__requests.some(request=>request.AnswerAsk)');
  await evaluate(`__respond('AnswerAsk',{AskAnswered:{disposition:'late_answer_queued:placement',continuation_turn_id:'placement-wake-fixture'}});`);
  await until('!document.querySelector(".approval-card")');
  assert.equal(await evaluate(`!!(__newerMessage.compareDocumentPosition(document.querySelector('[data-ask-id="placement-fixture"].answer-resume-message'))&Node.DOCUMENT_POSITION_FOLLOWING)`),true,'reply is placed after the newest message, not back at the question');
  await evaluate(`window.__replyBubble=document.querySelector('[data-ask-id="placement-fixture"].answer-resume-message');window.__replyBefore=__replyBubble.previousElementSibling;__questions.renderAskAnswerTurn({origin:{kind:'ask_answer',ask_id:'placement-fixture',display:'My saved reply'}});`);
  assert.equal(await evaluate('__replyBubble.isConnected&&__replyBubble.previousElementSibling===__replyBefore'),true,'wake acknowledgement leaves the original reply attached and in place');
  const recovery=await evaluate(`__questions.reconcileHistory([{role:'user',text:'My saved reply',origin:{kind:'ask_answer',ask_id:'',display:'My saved reply'}},{role:'user',text:'My saved reply',origin:{kind:'ask_answer',ask_id:'',display:'My saved reply'}}],{appendOnly:true})`);
  assert.equal(recovery.added.length,0,'legacy history copies of one saved question cannot append another reply');

  await evaluate(`{const style=document.createElement('style');style.textContent='*{animation:none!important;transition:none!important}';document.head.append(style);const feed=document.getElementById('conversationFeed');window.__scrollSpacer=document.createElement('div');__scrollSpacer.className='work-cluster';__scrollSpacer.style.height='2400px';feed.insertBefore(__scrollSpacer,__newerMessage);window.__scrollTail=document.createElement("div");__scrollTail.style.height="1400px";feed.insertBefore(__scrollTail,document.getElementById("conversationTail"));PhoenixUI.applyVisualPrefs({...PhoenixUI.visualPrefs(),conversationView:'chat'});__questions.state.pinToLatest=true;feed.scrollTop=feed.scrollHeight;document.getElementById('conversationDetailToggle').click();}`);
  await evaluate(`new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))`);
  assert.equal(await evaluate(`(()=>{const f=document.getElementById('conversationFeed');return f.scrollHeight-f.clientHeight-f.scrollTop<2})()`),true,'showing tools at the latest message keeps the latest message visible');
  await evaluate(`{const feed=document.getElementById('conversationFeed');__questions.state.pinToLatest=false;feed.scrollTop=__newerMessage.offsetTop-80;window.__messageOffset=__newerMessage.getBoundingClientRect().top-feed.getBoundingClientRect().top;document.getElementById('conversationDetailToggle').click();}`);
  await evaluate(`new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))`);
  const position=await evaluate(`({before:__messageOffset,after:__newerMessage.getBoundingClientRect().top-document.getElementById('conversationFeed').getBoundingClientRect().top})`);
  assert.ok(Math.abs(position.after-position.before)<2,'hiding older tools preserves the message being read: '+JSON.stringify(position));
  await evaluate(`__scrollSpacer.remove();__scrollTail.remove();`);

  await evaluate(`__requests.length=0;__ask('stale-vault',{action:'vault_unlock',approved_option:'Unlock here'});document.querySelector('[data-vault-open]').click();document.querySelector('[data-vault-password]').value='fixture-password-only';window.__staleCard=document.querySelector('.approval-card');void __questions.unlockApprovalVault(__staleCard);`);
  await until('__requests.some(request=>request.Vault)');
  await evaluate(`__questions.state.loadGeneration++;__questions.state.item={kind:'agent',id:'phoenix'};__questions.state.sessionId='company-phoenix';__respond('Vault',{Vault:{result:'unlocked'}});`);
  await until('!__staleCard.hasAttribute("aria-busy")');
  assert.equal(await evaluate('__requests.filter(request=>request.AnswerAsk).length'),0,'switch during unlock cannot answer another owner');
  assert.equal(await evaluate('__requests.filter(request=>request.Vault).length'),1,'stale unlock still runs once');
  assert.equal(await evaluate('[...__staleCard.querySelectorAll("button,input")].every(n=>!n.disabled)'),true,'cached card can be retried on return');
  await evaluate(`const node=document.createElement('section');node.className='settings-view';node.innerHTML='<button class="button primary">Add pass</button>';document.body.append(node);window.__contrastButton=node.firstChild;`);
  for(const theme of ['dark','light']){
    await evaluate(`document.documentElement.dataset.theme=${JSON.stringify(theme)}`);
    assert.notEqual(await evaluate('getComputedStyle(__contrastButton).color'),'rgba(0, 0, 0, 0)','primary label is visible in '+theme);
    assert.notEqual(await evaluate('getComputedStyle(__contrastButton).color'),await evaluate('getComputedStyle(__contrastButton).backgroundColor'),'primary label contrasts with the button in '+theme);
  }
  console.log('PASS: Enter/form single delivery, owner routing, pending-unlock Stop, stable reply position, tool-mode scroll anchoring, queue deduplication, browser controls, delegated activity, lazy diffs, primary labels, and focused draft preservation.');
}finally{
  ws?.close();chrome.kill();server.close();
  await new Promise(r=>chrome.exitCode!==null?r():chrome.once('exit',r));
  await rm(profile,{recursive:true,force:true,maxRetries:5,retryDelay:150});
}
