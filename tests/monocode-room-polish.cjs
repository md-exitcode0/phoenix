"use strict";
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");
const root = path.resolve(__dirname, "..");
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

async function main() {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "phoenix-monocode-browser-"));
  const server = spawn("python3", ["-B", path.join(__dirname, "monocode-fixture-server.py")], { stdio: ["ignore", "pipe", "inherit"] });
  let chrome, socket;
  try {
    const port = await new Promise((resolve, reject) => {
      server.stdout.once("data", data => resolve(Number(String(data).trim())));
      server.once("exit", code => reject(new Error(`Fixture server exited: ${code}`)));
    });
    chrome = spawn(process.env.CHROME_BINARY || "google-chrome-stable", ["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"], { stdio: "ignore" });
    const deadline = Date.now() + 15000;
    const active = path.join(profile, "DevToolsActivePort");
    while (!fs.existsSync(active) && Date.now() < deadline) await sleep(100);
    const debugPort = Number(fs.readFileSync(active, "utf8").split("\n")[0]);
    const targets = await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();
    const target = targets.find(row => row.type === "page");
    socket = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { socket.addEventListener("open", resolve, {once:true}); socket.addEventListener("error", reject, {once:true}); });
    let sequence = 0;
    const waiting = new Map();
    socket.addEventListener("message", event => {
      const reply = JSON.parse(String(event.data));
      if (!waiting.has(reply.id)) return;
      const {resolve,reject} = waiting.get(reply.id); waiting.delete(reply.id);
      if (reply.error) reject(new Error(JSON.stringify(reply.error))); else resolve(reply.result);
    });
    const command = (method, params = {}) => new Promise((resolve, reject) => {
      const id = ++sequence; waiting.set(id,{resolve,reject}); socket.send(JSON.stringify({id,method,params}));
    });
    const evaluate = async expression => {
      const reply = await command("Runtime.evaluate", {expression,returnByValue:true,awaitPromise:true});
      if (reply.exceptionDetails) throw new Error(JSON.stringify(reply.exceptionDetails));
      return reply.result.value;
    };
    await command("Emulation.setDeviceMetricsOverride",{width:1280,height:900,deviceScaleFactor:1,mobile:false});
    await command("Page.navigate", {url:`http://127.0.0.1:${port}/?skin=monocode`});
    let ready = false;
    while (!ready && Date.now() < deadline + 15000) { ready = await evaluate("Boolean(window.MonocodeRoomTest?.ui.state.view)"); if (!ready) await sleep(100); }
    assert.ok(ready, "MonoCode renderer loaded");
    const result = await evaluate(`(() => {
      const t=window.MonocodeRoomTest,d=t.ui.state.view.directory,group=d.groups[0];
      const ids=d.members.filter(m=>m.group_id===group.group_id).map(m=>m.agent_id);
      const [owner,peer]=ids.map(id=>d.agents.find(a=>a.agent_id===id));
      if(!owner||!peer)throw Error('Two fixture room members required');
      t.clearFeed();t.state.item={kind:'group',id:group.group_id};t.ui.state.selected=t.state.item;
      t.state.activeTurnId='room-polish-fixture';t.state.renderingTurnId=t.state.activeTurnId;
      t.state.displayRows=[{source:'history',turn_id:t.state.activeTurnId,value:{role:'user',text:'Room request',initiating_agent_id:owner.agent_id}}];
      const own={turn_id:t.state.activeTurnId,agent_id:owner.agent_id,agent_name:owner.display_name,markdown:'Owner closing answer',message_id:'owner-answer'};
      const other={turn_id:t.state.activeTurnId,agent_id:peer.agent_id,agent_name:peer.display_name,markdown:'Teammate speaks in the room',message_id:'peer-answer'};
      t.renderGroupMessage(own);t.renderGroupMessage(other);t.renderGroupMessage(other);
      const feed=document.getElementById('conversationFeed'),messages=[...feed.querySelectorAll(':scope > .group-message')];
      const replies=feed.querySelectorAll(':scope > .member-message').length;
      const ordering=messages.map(n=>n.querySelector('header strong')?.textContent);
      const foldedReceipts=feed.querySelectorAll('.coworker-return').length;
      t.state.browserOwnerAgentId=peer.agent_id;t.state.browserOwnerId='agent-'+peer.agent_id;t.state.browserTabRenderSignature='';
      t.renderInspectionBrowserTabs({tabs:[{id:'fixture-tab',title:'New tab',url:'about:blank',active:true}]});
      const browserOwner=document.querySelector('.browser-owner-chip')?.textContent;
      t.state.mentions=[];t.renderComposerText('@');
      const input=document.getElementById('composerInput');input.focus();
      const range=document.createRange();range.selectNodeContents(input);range.collapse(false);
      const selection=getSelection();selection.removeAllRanges();selection.addRange(range);t.detectMention();
      const pickerHeader=document.querySelector('#mentionPicker .mention-picker-head')?.textContent;
      document.body.classList.add('sidebar-collapsed');
      const logo=document.querySelector('#sidebarWake .phoenix-raster-logo'),logoSize=getComputedStyle(logo).width;
      const logoRect=logo.getBoundingClientRect();
      t.clearFeed();t.state.item={kind:'agent',id:peer.agent_id};t.state.renderingTurnId='direct-fixture';
      const cluster=t.ensureWorkCluster(peer.agent_id);cluster.querySelector('.work-tools').insertAdjacentHTML('beforeend','<div class="work-tool">Read a project file</div>');cluster.classList.add('has-tools');
      t.renderAnswer('Direct answer',peer.agent_id);const button=feed.querySelector('.answer-work-toggle');
      const folded=getComputedStyle(cluster).display==='none';button.click();
      const workOpened=getComputedStyle(cluster).display!=='none'&&getComputedStyle(cluster.querySelector('.work-trace')).display!=='none';button.click();
      const workClosed=getComputedStyle(cluster).display==='none';
      return {replies,ordering,foldedReceipts,browserOwner,expectedOwner:peer.display_name.split(/\\s+/)[0]+"'s browser",pickerHeader,logoSize,logoTop:logoRect.top,folded,workOpened,workClosed,skin:document.documentElement.dataset.skin};
    })()`);
    console.log(JSON.stringify(result,null,2));
    assert.equal(result.skin,"monocode");
    assert.equal(result.replies,1,"Peer replay keeps one room message");
    assert.equal(result.foldedReceipts,0,"Room reply is not folded into a return receipt");
    assert.equal(result.ordering.length,2,"Owner and member both speak");
    assert.equal(result.browserOwner,result.expectedOwner,"Browser chip identifies its real owner");
    assert.equal(result.pickerHeader,"Bring someone in");
    assert.equal(result.logoSize,"18px");
    assert.ok(result.logoTop >= 0 && result.logoTop < 40,"Collapsed logo stays inside the top band");
    assert.ok(result.folded&&result.workOpened&&result.workClosed,"Answer arrow opens and closes the real MonoCode work trace");
    const motion = await evaluate(`(async () => {
      const t=window.MonocodeRoomTest,d=t.ui.state.view.directory,group=d.groups[0];
      const ids=d.members.filter(m=>m.group_id===group.group_id).map(m=>m.agent_id);
      group.leader_agent_id=ids.at(-1);
      await t.ui.selectItem({kind:'group',id:group.group_id});
      t.syncGroupPals({status:'working',active_agent_ids:[ids[0]]});
      const host=document.getElementById('groupPals');
      const leader=host.querySelector('[data-seat="leader"]').dataset.agent;
      const leaderSize=parseFloat(getComputedStyle(host.querySelector('[data-seat="leader"]')).width);
      const chatHeroSize=parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--mc-hero-diameter'));
      const peerSize=parseFloat(getComputedStyle(host.querySelector('.pal-seat:not([data-seat="leader"])')).width);
      const before=[...host.querySelectorAll('.pal-seat')].map(n=>n.dataset.agent);
      const peerSpeaking=!!host.querySelector('.pal-seat.speaking[data-agent="'+ids[0]+'"]');
      t.syncGroupPals({status:'working',active_agent_ids:[group.leader_agent_id]});
      const stableSeats=JSON.stringify(before)===JSON.stringify([...host.querySelectorAll('.pal-seat')].map(n=>n.dataset.agent));
      t.ui.configureItem({kind:'group',id:group.group_id});
      const picker=document.querySelector('[data-group-leader]');
      const pickerValue=picker.value,pickerOptions=[...picker.options].map(o=>o.value);
      t.ui.closeModal();
      const jump=document.getElementById('jumpLatest');jump.hidden=false;
      const anchored=jump.parentElement.id==='composerZone';
      const aboveComposer=jump.getBoundingClientRect().bottom<=document.getElementById('composerZone').getBoundingClientRect().top;
      await t.ui.selectItem({kind:'agent',id:ids[0]});
      t.clearFeed();t.state.activeTurnId='motion-direct';t.state.displayRows=[];
      t.state.attachments=[];t.state.mentions=[];t.renderComposerText('Use the blue palette instead.');
      t.setWorking(true);t.syncSendMode();
      const sendNow=document.getElementById('sendButton').getAttribute('aria-label').startsWith('Send now');
      const queueBefore=t.state.queuedDrafts.size;
      await t.submitTurn();
      const bubble=document.querySelector('.user-message.steered-message');
      const immediate=!!bubble&&bubble.textContent.includes('Use the blue palette instead.')&&!!bubble.querySelector('.steered-label');
      const notQueued=t.state.queuedDrafts.size===queueBefore&&document.getElementById('queueBlock').hidden;
      const stillWorking=t.state.working;
      const composerCleared=!document.getElementById('composerInput').value;
      return {leader,expectedLeader:group.leader_agent_id,leaderSize,chatHeroSize,peerSize,peerSpeaking,stableSeats,pickerValue,pickerOptions,expectedMembers:ids,anchored,aboveComposer,sendNow,immediate,notQueued,stillWorking,composerCleared};
    })()`);
    console.log(JSON.stringify({motion},null,2));
    assert.equal(motion.leader,motion.expectedLeader,'Configured group leader owns the central seat');
    assert.equal(motion.leaderSize,motion.chatHeroSize,'Room leader matches the regular chat hero');
    assert.equal(motion.peerSize,66,'Room coworkers are three times the original 22px');
    assert.ok(motion.peerSpeaking&&motion.stableSeats,'Speaking highlights preserve stable seats');
    assert.equal(motion.pickerValue,motion.expectedLeader,'Configure dialog preserves the leader');
    assert.deepEqual(motion.pickerOptions.sort(),motion.expectedMembers.sort(),'Leader picker contains exactly the room members');
    assert.ok(motion.anchored&&motion.aboveComposer,'Back to latest stays above the composer');
    assert.ok(motion.sendNow&&motion.immediate&&motion.notQueued&&motion.stillWorking&&motion.composerCleared,'A working direct conversation receives its new message immediately');
    const passes=await evaluate(`(()=>{
      const p=window.PhoenixPasses;if(!p)throw Error('Passes kit did not load');
      const form=document.createElement('form');document.body.append(form);
      form.innerHTML=p.renderFields('login',null,{values:{site:'example.com'},lockSite:true});p.bind(form);
      const fixedSite=form.querySelector('[name="site"]').readOnly;
      const masked=form.querySelector('[name="password"]').type==='password';
      const missingPassword=p.collect(form,'login')===null;
      form.querySelector('[name="username"]').value='fixture@example.com';form.querySelector('[name="password"]').value='fixture-only-password';
      const login=p.collect(form,'login');p.wipe(form);const wiped=[...form.querySelectorAll('input')].every(input=>!input.value);
      form.innerHTML=p.renderFields('card');p.bind(form);
      form.querySelector('[name="number"]').value='4242424242424242';form.querySelector('[name="number"]').dispatchEvent(new Event('input',{bubbles:true}));
      form.querySelector('[name="cvc"]').value='123';form.querySelector('[name="expiry"]').value='12 / 99';
      const card=p.collect(form,'card'),formatted=form.querySelector('[name="number"]').value==='4242 4242 4242 4242';
      const secondarySealed=card.fields.cvc==='123'&&card.fields.expiry==='12 / 99'&&!('cvc' in card.metadata)&&!('expiry' in card.metadata);
      p.wipe(form);form.remove();
      return {fixedSite,masked,missingPassword,wiped,loginValid:login.kind==='login'&&login.site==='example.com',formatted,secondarySealed};
    })()`);
    console.log(JSON.stringify({passes},null,2));
    assert.ok(Object.values(passes).every(Boolean),'Passes forms validate, mask, and wipe secrets while keeping card fields out of public metadata');
    if (process.env.PHOENIX_TEST_SCREENSHOT) {
      await evaluate(`(async()=>{const t=window.MonocodeRoomTest;await t.ui.selectItem({kind:'group',id:t.ui.state.view.directory.groups[0].group_id});t.syncGroupPals();})()`);
      const screenshot=await command("Page.captureScreenshot",{format:"png"});
      fs.writeFileSync(process.env.PHOENIX_TEST_SCREENSHOT,Buffer.from(screenshot.data,"base64"));
      if (process.env.PHOENIX_TEST_NARROW_SCREENSHOT) {
        await command("Emulation.setDeviceMetricsOverride",{width:480,height:900,deviceScaleFactor:1,mobile:false});
        await sleep(350);
        const narrow=await evaluate(`(()=>{const host=document.getElementById('groupPals'),stage=document.getElementById('conversationStage').getBoundingClientRect(),body=document.getElementById('conversationBody').getBoundingClientRect(),faces=[...host.querySelectorAll('.pal-seat')].map(n=>n.getBoundingClientRect());return {stage:host.dataset.stage,inside:faces.every(r=>r.left>=stage.left&&r.right<=stage.right),aboveMessages:faces.every(r=>r.bottom<=body.top),sizes:faces.map(r=>Math.round(r.width))};})()`);
        assert.ok(narrow.inside&&narrow.aboveMessages,'Enlarged room avatars fit the narrow stage and stay above messages');
        assert.ok(narrow.sizes.every(size=>size===66||size===115),'Narrow layout keeps the enlarged avatar sizes');
        console.log(JSON.stringify({narrow},null,2));
        const narrowShot=await command("Page.captureScreenshot",{format:"png"});
        fs.writeFileSync(process.env.PHOENIX_TEST_NARROW_SCREENSHOT,Buffer.from(narrowShot.data,"base64"));
      }
    }
  } finally {
    socket?.close();chrome?.kill();server.kill();
    await sleep(200);
    fs.rmSync(profile,{recursive:true,force:true});
  }
}
main().catch(error => { console.error(error); process.exitCode=1; });
