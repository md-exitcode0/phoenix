#!/usr/bin/env node
// Actual Canvas sources, isolated preview RPCs, no gateway or model calls.
// Use an already-installed Playwright runtime and Chromium; install nothing.
// PHOENIX_PLAYWRIGHT=/absolute/path/to/playwright-core/index.mjs node scripts/check-conversation-disclosure.mjs [output-dir]
import {createServer} from 'node:http';
import {readFile, mkdir, writeFile} from 'node:fs/promises';
import {resolve, join, extname, sep, basename} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';

const root=resolve(process.env.PHOENIX_UI_ROOT||fileURLToPath(new URL('../canvas-app/ui/',import.meta.url)));
const out=resolve(process.argv[2]||'artifacts/conversation-disclosure');
await mkdir(out,{recursive:true});
const {chromium}=await import(process.env.PHOENIX_PLAYWRIGHT?pathToFileURL(process.env.PHOENIX_PLAYWRIGHT).href:'playwright-core');
const hook=`window.__conversationReview={state,clearFeed,closeApproval,replaceDisplayRows,renderUser,renderAnswer,renderStory,renderHistory,renderApproval,renderCommentary,openBrowser,ensureWorkCluster,settleWorkCluster,finishTurnActivity,setWorking,paintBrowserImageFrame,showInspectionSidebar,hideInspectionSidebar,openLinkInAgentBrowser,navigateBrowser,selectConversation,
  setResume:fn=>{resumeInspectionBrowser=fn;},setOpen:fn=>{openBrowser=fn;},setNavigate:fn=>{navigateBrowser=fn;},setFlush:fn=>{flushBrowserTyping=fn;},setCommand:fn=>{browserCommand=fn;},setClose:fn=>{closeBrowser=fn;}};`;
const browserPreviewHTML='<!doctype html><html><head><meta charset="utf-8"><style>body{margin:0;padding:48px;background:#faf9f6;color:#252521;font:18px/1.6 system-ui}main{max-width:680px;margin:auto}small{color:#666}h1{font-size:36px;line-height:1.2}section{margin-top:28px;padding:24px;border:1px solid #ddd;border-radius:12px;background:white}</style></head><body><main><small>LOCAL LAYOUT FIXTURE</small><h1>A calm conversation,<br>with the work close by.</h1><p>This page is rendered locally for the isolated Phoenix preview.</p><section><strong>Review notes</strong><p>Activity details are available on request. Questions, approvals and useful progress stay visible.</p></section></main></body></html>';
const types={'.js':'text/javascript','.css':'text/css','.html':'text/html','.svg':'image/svg+xml','.png':'image/png','.woff2':'font/woff2','.json':'application/json'};
const server=createServer(async(req,res)=>{
  const url=new URL(req.url,'http://localhost'),path=resolve(root,'.'+decodeURIComponent(url.pathname==='/'?'/index.html':url.pathname));
  if(url.pathname==='/review-page.html'){res.writeHead(200,{'content-type':'text/html'});res.end(browserPreviewHTML);return;}
  if(path!==root&&!path.startsWith(root+sep)){res.writeHead(403);res.end();return;}
  try{let body=await readFile(path);if(path===join(root,'conversation.js'))body=Buffer.from(body.toString().replace('  bind(); renderVoiceState(); autosize();',hook+'\n  bind(); renderVoiceState(); autosize();'));
    res.writeHead(200,{'content-type':types[extname(path)]||'application/octet-stream'});res.end(body);
  }catch{res.writeHead(404);res.end();}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const origin=`http://127.0.0.1:${server.address().port}`;
let browser;
const results={},errors=[];
try{
  browser=await chromium.launch({executablePath:process.env.PHOENIX_CHROMIUM||'/usr/bin/google-chrome',headless:true,args:['--disable-background-networking','--disable-component-update','--disable-sync','--no-first-run']});
  const context=await browser.newContext({viewport:{width:1440,height:1000},reducedMotion:'reduce'});
  await context.addInitScript(()=>localStorage.setItem('phoenix-theme','dark'));
  // Permit only this fixture server. Even fonts/images cannot reach outside.
  await context.route('**/*',route=>route.request().url().startsWith(origin+'/')?route.continue():route.abort());
  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
  const load=async(shot='review-local')=>{await page.goto(`${origin}/index.html?shot=${shot}`);await page.waitForFunction(()=>window.__conversationReview?.state.item);};
  const shot=async(name)=>page.screenshot({path:join(out,name+'.png')});
  const check=(name,value)=>{results[name]=value;assert.equal(value,true,name);};
  if(process.env.PHOENIX_FIXTURES_ONLY){
    for(const fixture of ['worker-presence-proof','team-roster-proof','team-block-proof','owner-answers-proof','compact-errors-proof','conversation-isolation']){await load(fixture);await page.waitForFunction(()=>/^(PASS|FAIL)/.test(document.title));results[fixture]={title:await page.title(),checks:await page.evaluate(()=>Object.fromEntries(Object.entries(document.documentElement.dataset).filter(([k])=>/Checks$/.test(k))))};}
    await writeFile(join(out,'checks.json'),JSON.stringify({results,errors},null,2));console.log(JSON.stringify({results,errors},null,2));
  }else{
  await load();
  check('serverRejectsPrefixSibling',(await page.request.get(`${origin}/..%2F${basename(root)}-private/secret.txt`)).status()===403);
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.closeApproval();t.replaceDisplayRows([],false);t.state.activeTurnId='review-turn';t.state.sessionId='review-session';t.state.working=false;t.renderUser('Hey, can we make this feel a little calmer?');t.renderAnswer('Yes. I’ll keep the useful updates easy to see, with the details there when you want them.');});
  check('ordinaryChatHasNoEmptyWorkBlock',await page.locator('.work-cluster').count()===0);await shot('01-ordinary-chat-1440');
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-turn';t.state.turnStartedAt=Date.now()-12000;const prompt=t.renderUser('Check the conversation layout and show me what changed.');t.setWorking(true);t.renderStory({kind:'narration',agent:'phoenix',text:'I’ve found the activity settings. I’m checking that questions and approvals stay easy to reach.'});for(let i=1;i<=3;i++)t.renderStory({kind:'tool',agent:'phoenix',tool:'read',target:`source-${i}.js`,ok:true,detail:`Checked source ${i}.`});t.renderStory({kind:'tool_start',agent:'phoenix',tool:'read',target:'conversation.js'});});
  const work=page.locator('.work-cluster').first(),header=work.locator('.group-work-agent');
  check('activeWorkStartsClosed',await header.getAttribute('aria-expanded')==='false');
  check('activeProgressVisible',await header.isVisible()&&(await header.textContent()).includes('Running tools'));
  await page.evaluate(()=>window.__conversationReview.renderStory({kind:'reasoning',agent:'researcher',text:'PRIVATE_PEER_NOT_USER: checking internal handoff state.'}));
  check('peerNarrationStaysInTrace',!(await page.locator('.work-progress,.group-work-agent').allTextContents()).join(' ').includes('PRIVATE_PEER_NOT_USER'));
  await page.evaluate(()=>document.querySelector('.work-cluster[data-agent=researcher]')?.remove());
  check('collapsedOwnerNarrationVisible',await work.locator('.work-progress').isVisible()&&(await work.locator('.work-progress').textContent()).includes('questions and approvals stay easy to reach'));
  check('closedToolsHidden',!(await work.locator('.work-tool').first().isVisible()));
  check('disclosureControlsTrace',Boolean(await header.getAttribute('aria-controls')));await shot('02-active-work-1440');
  await header.focus();await page.keyboard.press('Enter');
  check('keyboardExpandsDetails',await header.getAttribute('aria-expanded')==='true'&&await work.locator('.work-trace').isVisible());check('expansionDoesNotDuplicateNarration',await page.locator('.work-progress').count()===0);const toolGroup=work.locator('.trace-subgroup').first();await toolGroup.locator('summary').focus();await page.keyboard.press('Enter');check('keyboardExpandsToolGroup',await toolGroup.locator('.work-tool').first().isVisible());await shot('03-expanded-details-1440');
  await page.evaluate(()=>{const t=window.__conversationReview;t.renderStory({kind:'tool',agent:'phoenix',tool:'read',target:'conversation.js',ok:true,detail:'Verified the disclosure controls.'});t.finishTurnActivity();t.setWorking(false);t.renderAnswer('The activity row stays compact, and you can open the checks whenever you need them. Questions and approvals stay in view.');});
  check('completedAnswerRemovesProgress',await page.locator('.work-progress').count()===0);
  check('deliberateExpansionSurvivesSettling',await header.getAttribute('aria-expanded')==='true');
  await header.click();await shot('04-completed-work-1440');
  check('settledWorkStillClosed',await header.getAttribute('aria-expanded')==='false');
  check('completedChatHasNoFloatingProgress',await page.locator('.work-progress').count()===0);
  check('allToolReceiptsRetained',await work.locator('.work-tool').count()===4);
  await header.click();await page.reload();await page.waitForFunction(()=>window.__conversationReview?.state.item);
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-turn';t.state.painting=true;t.renderHistory({role:'tool',agent:'phoenix',tool:'read',target:'conversation.js',ok:true,detail:'Verified the disclosure controls.'});t.state.painting=false;t.settleWorkCluster(document.querySelector('.work-cluster'));});
  check('expansionSurvivesReloadAndReplay',await page.locator('.work-cluster .group-work-agent').first().getAttribute('aria-expanded')==='true');
  await page.locator('.work-cluster .group-work-agent').first().click();
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-turn';t.state.painting=true;t.renderHistory({role:'tool',agent:'phoenix',tool:'read',target:'conversation.js',ok:true});t.state.painting=false;});
  check('collapseSurvivesRepaint',await page.locator('.work-cluster .group-work-agent').first().getAttribute('aria-expanded')==='false');
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-failure';t.setWorking(true);t.renderUser('Check this file.');t.renderStory({kind:'tool',agent:'phoenix',tool:'read',target:'missing.js',ok:false,detail:'File not found: missing.js'});t.finishTurnActivity();t.setWorking(false);});
  check('failureVisibleWithoutTrace',await page.locator('.work-attention').isVisible()&&(await page.locator('.group-work-agent').textContent()).includes('failed call'));
  check('failureReceiptRetained',await page.locator('.work-tool.failed').count()===1);await shot('05-failure-1440');
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-interrupted';t.setWorking(true);t.renderStory({kind:'tool_start',agent:'phoenix',tool:'read',target:'unfinished.js'});t.finishTurnActivity(true);t.setWorking(false);});
  check('interruptionVisible',/Stopped/.test(await page.locator('.group-work-agent').textContent()));
  check('interruptionDoesNotInventSuccess',await page.locator('.work-tool[data-state=cancelled]').count()===1&&await page.locator('.tr-status.s-cancelled').count()===1);
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-unknown';t.setWorking(true);t.renderStory({kind:'tool_start',agent:'phoenix',tool:'read',target:'unfinished.js'});t.finishTurnActivity();t.setWorking(false);});
  check('missingReceiptVisible',/unconfirmed call/.test(await page.locator('.group-work-agent').textContent()));
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-empty';t.state.working=false;const c=t.ensureWorkCluster('phoenix');window.reviewEmptySummary=c.querySelector('small').textContent;t.settleWorkCluster(c);});
  check('emptyStateDistinct',await page.evaluate(()=>window.reviewEmptySummary==='No activity recorded'&&!document.querySelector('.work-cluster')));
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-question';t.renderUser('Let’s adjust the conversation.');t.renderStory({kind:'tool',agent:'phoenix',tool:'read',target:'conversation.js',ok:true});t.renderApproval({id:'review-question',agent:'phoenix',questions:[{question:'How much detail would you like?',options:['Keep it conversational','Show the technical detail'],multi_select:false}]});});
  check('pendingQuestionOutsideTrace',await page.locator('.approval-question-card').isVisible()&&await page.locator('.work-trace .approval-card').count()===0);
  await page.locator('.qa-option').first().focus();await page.keyboard.press('Enter');
  check('questionKeyboardChoice',await page.locator('.qa-option').first().getAttribute('aria-checked')==='true');await shot('06-question-1440');
  await page.locator('.qa-next').focus();await page.keyboard.press('Enter');
  await page.waitForFunction(()=>!document.querySelector('.approval-question-card'));
  check('questionKeyboardSubmit',await page.locator('.approval-question-card').count()===0);
  await page.evaluate(()=>{const t=window.__conversationReview;t.closeApproval();t.renderApproval({id:'review-approval',agent:'phoenix',questions:[{question:'Post this exact message: “The preview is ready for review.”',options:['Allow once','Not now']}],approval:{action:'governed_effect',subject:'Post the prepared message',details:{effect:'external_message'}}});});
  check('approvalOutsideTrace',await page.locator('.approval-decision-card').isVisible()&&await page.locator('.work-trace .approval-card').count()===0);
  check('approvalExactActionVisible',(await page.locator('.approval-decision-card').textContent()).includes('The preview is ready for review'));await shot('07-approval-1440');
  const deny=page.locator('.approval-decision-card [data-ask-option="Not now"]');await deny.focus();await page.keyboard.press('Enter');
  await page.waitForFunction(()=>!document.querySelector('.approval-decision-card'));
  check('approvalKeyboardDeny',await page.locator('.approval-decision-card').count()===0);
  for(const fixture of ['worker-presence-proof','team-roster-proof','team-block-proof','owner-answers-proof','compact-errors-proof','conversation-isolation']){
    await load(fixture);await page.waitForFunction(()=>/^(PASS|FAIL)/.test(document.title));
    results[fixture]={title:await page.title(),checks:await page.evaluate(()=>Object.fromEntries(Object.entries(document.documentElement.dataset).filter(([k])=>/Checks$/.test(k))))};
    if(fixture==='team-block-proof')await shot('08-team-work-1440');
  }
  await load('review-local');
  check('groupOwnerProgressVisibleAndPeersPrivate',await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.closeApproval();t.state.item={kind:'group',id:'launch-room'};t.state.activeTurnId='review-owner-progress';t.replaceDisplayRows([{source:'history',turn_id:'review-owner-progress',value:{role:'user',text:'Check the layout.',initiating_agent_id:'phoenix'}}],false);t.setWorking(true);t.renderUser('Check the layout.');t.renderStory({kind:'narration',agent:'phoenix',text:'Owner progress for the conversation.'});t.renderStory({kind:'narration',agent:'researcher',text:'PRIVATE_PEER_NOT_USER'});const note=document.querySelector('.team-work-block>.work-progress');return !!note&&note.textContent.includes('Owner progress')&&!note.textContent.includes('PRIVATE_PEER_NOT_USER')&&!note.closest('.team-work-body');}));
  await load('review-local');
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.activeTurnId='review-prose-only';t.setWorking(true);t.renderStory({kind:'commentary',agent:'phoenix',text:'I found the setting. I’m checking the question controls next.'});t.renderStory({kind:'reasoning',agent:'phoenix',text:'INTERNAL_REASONING_NOT_PROGRESS'});});
  check('authoredCommentaryVisibleAndReasoningPrivate',(await page.locator('.work-progress').textContent()).includes('question controls')&&!(await page.locator('.group-work-agent,.work-progress').allTextContents()).join(' ').includes('INTERNAL_REASONING_NOT_PROGRESS'));
  await page.evaluate(()=>{const t=window.__conversationReview;t.renderAnswer('The question controls are ready.');});
  check('proseOnlyReceiptRetained',await page.locator('.work-trace .agent-update').count()===1&&await page.locator('.work-progress').count()===0);
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.state.painting=true;t.renderHistory({role:'narration',agent:'phoenix',text:'I found the setting. I’m checking the question controls next.'});t.renderHistory({role:'answer',agent:'phoenix',text:'The question controls are ready.'});t.state.painting=false;});
  check('completedReplayHasNoFloatingProgress',await page.locator('.work-progress').count()===0&&await page.locator('.work-trace .agent-update').count()===1);
  await load('review-local');
  check('delayedCloseCancelsBrowserRebind',await page.evaluate(async()=>{const t=window.__conversationReview,s=t.state;s.item={kind:'agent',id:'researcher'};s.sessionId='researcher-session';s.loadGeneration+=1;s.browserOwnerId='agent-phoenix';s.browserBoundKey='agent:phoenix';let release;t.setClose(()=>new Promise(resolve=>{release=resolve;}));const pending=t.openBrowser('researcher','browse');s.item={kind:'agent',id:'coder'};s.sessionId='coder-session';s.loadGeneration+=1;release();await pending;return s.browserOwnerId==='agent-phoenix'&&s.browserBoundKey==='agent:phoenix';}));
  await load('review-local');
  check('delayedResumeCancelsOnConversationSwitch',await page.evaluate(async()=>{
    const t=window.__conversationReview,s=t.state,calls=[];s.item={kind:'agent',id:'researcher'};s.sessionId='researcher-session';s.loadGeneration+=1;s.browserOwnerId='';s.browserBoundKey='';
    let release;t.setResume(()=>new Promise(resolve=>{release=resolve;}));t.setOpen(async owner=>{calls.push(['open',owner]);});t.setNavigate(async url=>{calls.push(['navigate',url]);});
    const pending=t.openLinkInAgentBrowser('https://example.invalid/source');s.item={kind:'agent',id:'coder'};s.sessionId='coder-session';s.loadGeneration+=1;release();await pending;return calls.length===0;
  }));
  check('linkUsesClickedOwner',await page.evaluate(async()=>{
    const t=window.__conversationReview,s=t.state,calls=[];s.item={kind:'agent',id:'researcher'};s.sessionId='researcher-session';s.loadGeneration+=1;s.browserOwnerId='';s.browserBoundKey='';t.setResume(async()=>{});
    t.setOpen(async owner=>{calls.push(['open',owner]);s.browserOwnerAgentId=owner;s.browserOwnerId='agent-'+owner;s.browserBoundKey='agent:'+owner;s.browserTabs=[{id:'blank',url:'about:blank',active:true}];});t.setNavigate(async(url,newTab)=>{calls.push(['navigate',url,newTab]);});await t.openLinkInAgentBrowser('https://example.invalid/source');return JSON.stringify(calls)===JSON.stringify([['open','researcher'],['navigate','https://example.invalid/source',false]]);
  }));
  const contentPage=await context.newPage();await contentPage.setViewportSize({width:900,height:700});await contentPage.goto(`${origin}/review-page.html`);const contentImage='data:image/png;base64,'+(await contentPage.screenshot()).toString('base64');await contentPage.close();
  await load('browser-proof');await page.waitForTimeout(700);await page.evaluate(src=>{const frame=document.getElementById('browserFrame');frame.src=src;frame.hidden=false;},contentImage);await shot('09-chat-browser-1440');
  await page.setViewportSize({width:1100,height:850});await page.waitForTimeout(100);await shot('10-chat-browser-1100');
  const composerFits=()=>page.evaluate(()=>{const c=document.getElementById('composer').getBoundingClientRect();return [...document.querySelectorAll('.composer-toolbar > button,.composer-toolbar > details,.composer-toolbar #attachmentMenuMount')].every(node=>{const r=node.getBoundingClientRect();return r.left>=c.left-1&&r.right<=c.right+1;});});
  check('primaryComposerControlsFit1100',await composerFits());
  const options=page.locator('#composerOptions'),optionsSummary=options.locator('summary');await optionsSummary.focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>document.querySelector('#composerOptions').open&&document.querySelector('#composerOptions summary').getAttribute('aria-expanded')==='true');
  check('optionsKeyboardDisclosure',await page.locator('#modelButton').isVisible()&&await page.locator('#reasoningControl').isVisible()&&await optionsSummary.getAttribute('aria-expanded')==='true');await shot('15-conversation-options-1100');
  await page.keyboard.press('Tab');check('optionsTabReachesModel',await page.locator('#modelButton').evaluate(node=>node===document.activeElement));
  await page.keyboard.press('Escape');check('optionsEscapeRestoresFocus',await options.evaluate(node=>!node.open)&&await optionsSummary.evaluate(node=>node===document.activeElement));
  await optionsSummary.focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>document.querySelector('#composerOptions').open);await page.locator('#reasoningControl').focus();await page.keyboard.press('Enter');
  const effort=page.locator('.goo-layer [role=slider]');await effort.waitFor({state:'visible'});await effort.focus();await page.keyboard.press('End');
  check('optionsRetainKeyboardReasoningSetting',await page.evaluate(()=>{const s=window.__conversationReview.state;return s.reasoning===s.selectedModel.effort_levels.at(-1)&&document.getElementById('composerOptions').open;}));
  await page.keyboard.press('Escape');await page.waitForFunction(()=>!document.querySelector('.goo-layer'));check('nestedOptionsEscapeRestoresTrigger',await options.evaluate(node=>node.open)&&await page.locator('#reasoningControl').evaluate(node=>node===document.activeElement));
  check('immediateNestedEscapeKeepsOptionsAndClosesPopup',await page.evaluate(()=>{const trigger=document.getElementById('reasoningControl');trigger.click();const event=new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true});trigger.dispatchEvent(event);return event.defaultPrevented&&document.getElementById('composerOptions').open&&trigger.getAttribute('aria-expanded')==='false'&&!document.querySelector('.goo-layer')&&document.activeElement===trigger;}));
  // Closing before the deferred outside listener runs must not leave a stale
  // listener capable of dismissing a later popup or stealing Escape.
  await page.evaluate(()=>new Promise(resolve=>setTimeout(resolve,0)));
  check('rapidPopupReplacementDoesNotLeakEscape',await page.evaluate(()=>{const trigger=document.getElementById('reasoningControl'),content=()=>document.createElement('div'),kit=window.PhoenixAgentKit;const old=kit.gooPopover(trigger,content());old.close(true);const current=kit.gooPopover(trigger,content());const event=new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true});trigger.dispatchEvent(event);return event.defaultPrevented&&!document.querySelector('.goo-layer')&&document.getElementById('composerOptions').open&&current.panel.inert;}));
  const chosenEffort=await page.evaluate(()=>window.__conversationReview.state.reasoning);await page.locator('#stageName').click();check('optionsOutsideClickClosesWithoutResetting',await options.evaluate(node=>!node.open)&&await page.evaluate(value=>window.__conversationReview.state.reasoning===value,chosenEffort));
  const draftText='Keep this unsent draft.\nI still want to finish this thought.';await page.evaluate(text=>{const input=document.getElementById('composerInput');input.value=text;input.dispatchEvent(new Event('input',{bubbles:true}));},draftText);
  await page.evaluate(()=>{window.__conversationReview.state.attachments=[{name:'draft-reference.txt',path:'/isolated-fixture/draft-reference.txt',size:20,type:'text/plain'}];});
  const draftPreserved=()=>page.evaluate(text=>document.getElementById('composerInput').value===text&&window.__conversationReview.state.attachments[0]?.path==='/isolated-fixture/draft-reference.txt',draftText);
  await optionsSummary.click();await page.locator('#modelButton').click();await page.waitForFunction(()=>!document.getElementById('menuLayer').hidden);await page.keyboard.press('Escape');
  check('modelMenuDismissPreservesOptions',await options.evaluate(node=>node.open));check('modelMenuDismissPreservesDraft',await draftPreserved());
  await page.keyboard.press('Escape');check('optionsDismissPreservesDraft',await options.evaluate(node=>!node.open)&&await draftPreserved());
  await page.setViewportSize({width:760,height:900});await page.waitForTimeout(100);check('narrowResizePreservesSidebarAccess',await page.evaluate(()=>document.body.classList.contains('sidebar-collapsed')));await page.waitForTimeout(100);await page.evaluate(async src=>{const blob=await(await fetch(src)).blob();window.__conversationReview.paintBrowserImageFrame({blob,url:'https://preview.phoenix.local'});},contentImage);await page.waitForFunction(()=>!document.getElementById('browserFrame').hidden&&document.getElementById('browserEmpty').hidden);
  check('narrowPanelsStackAboveChat',await page.evaluate(()=>{const panel=document.getElementById('inspectionSidebar').getBoundingClientRect(),chat=document.getElementById('conversationStage').getBoundingClientRect();return document.body.classList.contains('inspection-stacked')&&panel.bottom<=chat.top+1&&chat.height>400&&chat.bottom<=innerHeight+1;}));
  check('primaryComposerControlsFit760',await composerFits());check('narrowBrowserContentVisible',await page.locator('#browserOverlay').isVisible());await shot('16-chat-browser-760');
  await page.evaluate(()=>{const t=window.__conversationReview;t.renderApproval({id:'review-stacked-question',agent:'phoenix',questions:[{question:'Should I show more detail?',options:['Keep it simple','Show more']}]});});
  await page.waitForTimeout(150);await shot('17-question-browser-760');
  check('questionVisibleWithNarrowBrowser',await page.locator('.approval-question-card').isVisible()&&await page.locator('.approval-question-card').evaluate(node=>{const r=node.getBoundingClientRect(),p=document.getElementById('inspectionSidebar').getBoundingClientRect();return r.top>=p.bottom&&r.bottom<=innerHeight;}));await shot('17-question-browser-760');
  await page.setViewportSize({width:390,height:800});await page.waitForTimeout(100);check('questionAndComposerFit390',await composerFits()&&await page.locator('.approval-question-card').evaluate(node=>{const r=node.getBoundingClientRect(),p=document.getElementById('inspectionSidebar').getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.top>=p.bottom&&r.bottom<=innerHeight;}));await shot('18-question-browser-390');
  await page.locator('.qa-option').first().focus();await page.keyboard.press('Enter');check('narrowQuestionKeyboardChoice',await page.locator('.qa-option').first().getAttribute('aria-checked')==='true');
  await page.setViewportSize({width:760,height:900});await page.waitForTimeout(100);
  await page.evaluate(()=>{const t=window.__conversationReview;t.closeApproval();t.renderStory({kind:'tool_start',agent:'phoenix',tool:'read',target:'panel-check.js'});t.setWorking(true);for(const tab of ['desktop','browser','changes','browser','desktop','browser'])t.showInspectionSidebar(tab);t.finishTurnActivity(true);t.setWorking(false);});
  check('interruptedPanelChangesKeepStopVisible',await page.locator('#sendButton').isVisible()&&await composerFits());
  check('interruptedPanelChangesPreserveTypedDraftAndAttachment',await draftPreserved());
  await page.evaluate(()=>{const t=window.__conversationReview;t.hideInspectionSidebar();});await page.waitForTimeout(100);check('closingNarrowPanelRestoresChat',await page.evaluate(()=>!document.body.classList.contains('inspection-stacked')&&document.getElementById('conversationStage').getBoundingClientRect().top<100));
  await page.evaluate(()=>window.__conversationReview.showInspectionSidebar('browser'));await page.setViewportSize({width:1100,height:850});await page.waitForTimeout(100);check('resizingRestoresSideBySide',await page.evaluate(()=>!document.body.classList.contains('inspection-stacked')&&document.getElementById('conversationStage').getBoundingClientRect().right<=document.getElementById('inspectionSidebar').getBoundingClientRect().left+1));
  check('panelClosingAndResizePreserveDraft',await draftPreserved());

  await page.setViewportSize({width:1440,height:1000});await page.evaluate(src=>{const t=window.__conversationReview;t.showInspectionSidebar('desktop');window.PhoenixDesktopViewer.update({visible:true,context:'local-desktop-layout',owners:[{id:'phoenix',label:'Phoenix'}],rpc:async message=>message==='DesktopWorkspaces'?{DesktopWorkspaces:[{scope_key:'layout-fixture',owner_agent_id:'phoenix',label:'Phoenix · Local preview',running:true,in_use:false}]}:{DesktopObservation:{scope_key:'layout-fixture',captured_at_ms:Date.now(),data_url:src,width:900,height:700,kind:'desktop'}}});},contentImage);await page.waitForFunction(()=>!document.getElementById('desktopObservationImage').hidden);await shot('13-chat-desktop-1440');
  await page.setViewportSize({width:1100,height:850});await shot('14-chat-desktop-1100');
  await page.setViewportSize({width:760,height:900});await page.waitForTimeout(100);await shot('19-chat-desktop-760');check('narrowDesktopAndComposerFit',await composerFits()&&await page.locator('#desktopObservationImage').evaluate(node=>{const r=node.getBoundingClientRect();return r.width>0&&r.height>0&&r.right<=innerWidth+1&&r.bottom<=document.getElementById('inspectionSidebar').getBoundingClientRect().bottom+1;}));
  await page.setViewportSize({width:1100,height:850});
  await load('approval-decision');await page.waitForTimeout(500);await shot('11-approval-1100');
  await page.setViewportSize({width:760,height:900});await load('approval-question');await page.waitForTimeout(500);if(!await page.evaluate(()=>document.body.classList.contains('sidebar-collapsed')))await page.locator('#sidebarToggle').click();check('narrowQuestionVisibleWithSidebarCollapsed',await page.locator('.approval-question-card').isVisible());await shot('12-question-760');
  await load('review-local');
  await page.evaluate(()=>{const t=window.__conversationReview;t.clearFeed();t.closeApproval();t.replaceDisplayRows([],false);t.hideInspectionSidebar();t.setWorking(false);t.renderUser('Hey, how’s the cleanup going?');t.renderAnswer('The conversation has more room now. Activity stays compact, and the useful updates remain visible while I work.');});await shot('20-ordinary-chat-760');
  check('threadSwitchPreservesSeparateDraftsAndAttachments',await page.evaluate(async()=>{const t=window.__conversationReview,input=document.getElementById('composerInput'),original={...t.state.item},sessionId=t.state.sessionId;input.value='First conversation draft\nStill unsent';t.state.attachments=[{name:'first.txt',path:'/isolated-fixture/first.txt',size:3,type:'text/plain'}];await t.selectConversation({item:{kind:'agent',id:'coder'},sessionId:'company-coder'});const separate=input.value==='';input.value='Coder draft';await t.selectConversation({item:original,sessionId});const restored=input.value==='First conversation draft\nStill unsent'&&t.state.attachments[0]?.path==='/isolated-fixture/first.txt';await t.selectConversation({item:{kind:'agent',id:'coder'},sessionId:'company-coder'});return separate&&restored&&input.value==='Coder draft'&&t.state.attachments.length===0;}));
  check('noPageErrors',errors.length===0);
  for(const fixture of ['worker-presence-proof','team-roster-proof','team-block-proof','owner-answers-proof','compact-errors-proof','conversation-isolation'])assert.match(results[fixture].title,/^PASS/,JSON.stringify(results[fixture]));
  await writeFile(join(out,'checks.json'),JSON.stringify({results,errors,scope:'Isolated actual-source browser preview. No native runtime or model behaviour verified.'},null,2));
  console.log(JSON.stringify({passed:Object.keys(results).length,results,errors},null,2));
  }
}catch(error){await writeFile(join(out,'checks.json'),JSON.stringify({results,errors,failure:String(error)},null,2));throw error;}
finally{await browser?.close();await new Promise(resolve=>server.close(resolve));}
