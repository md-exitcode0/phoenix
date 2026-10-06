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
    if (process.env.PHOENIX_TEST_SCREENSHOT) {
      const screenshot=await command("Page.captureScreenshot",{format:"png"});
      fs.writeFileSync(process.env.PHOENIX_TEST_SCREENSHOT,Buffer.from(screenshot.data,"base64"));
    }
  } finally {
    socket?.close();chrome?.kill();server.kill();
    await sleep(200);
    fs.rmSync(profile,{recursive:true,force:true});
  }
}
main().catch(error => { console.error(error); process.exitCode=1; });
