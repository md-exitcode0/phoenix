#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

const port = Number(process.env.PHOENIX_CHROMIUM_DEBUG_PORT || "17442");
const proofUrl = process.env.PHOENIX_AVERY_PROOF_URL || "";

async function gatewayRequest(request) {
  const token=(await readFile(join(homedir(),".phoenix","gateway.token"),"utf8")).trim();
  const gatewayPort=Number(process.env.PHOENIX_GATEWAY_WS_PORT||"7469");
  return new Promise((resolve,reject)=>{const socket=new WebSocket(`ws://127.0.0.1:${gatewayPort}/?token=${encodeURIComponent(token)}`),timer=setTimeout(()=>{socket.close();reject(new Error("gateway request timed out"));},15000);socket.addEventListener("open",()=>socket.send(JSON.stringify(request)));socket.addEventListener("message",(event)=>{clearTimeout(timer);socket.close();resolve(JSON.parse(String(event.data)));});socket.addEventListener("error",()=>{clearTimeout(timer);reject(new Error("gateway socket failed"));});});
}

async function gatewaySurfaceStatus(instance) {
  let reply=null;
  for(let attempt=0;attempt<20;attempt+=1){reply=await gatewayRequest({BrowserSurface:{instance,action:"status"}});if(reply?.BrowserSurface)return reply;await new Promise((resolve)=>setTimeout(resolve,150));}
  return reply;
}

async function targetList() {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(5000) });
  if (!response.ok) throw new Error(`DevTools target list failed: HTTP ${response.status}`);
  return response.json();
}

function cdp(url) {
  const socket = new WebSocket(url);let id=0;const pending=new Map();
  const ready=new Promise((resolve,reject)=>{socket.addEventListener("open",resolve,{once:true});socket.addEventListener("error",()=>reject(new Error("DevTools socket failed")),{once:true});});
  socket.addEventListener("message",(event)=>{const message=JSON.parse(String(event.data)),job=pending.get(message.id);if(!job)return;pending.delete(message.id);clearTimeout(job.timer);message.error?job.reject(new Error(message.error.message)):job.resolve(message.result);});
  return{async send(method,params={}){await ready;const requestId=++id;return new Promise((resolve,reject)=>{const timer=setTimeout(()=>{pending.delete(requestId);reject(new Error(`${method} timed out`));},60000);pending.set(requestId,{resolve,reject,timer});socket.send(JSON.stringify({id:requestId,method,params}));});},close(){socket.close();}};
}

async function evaluate(client,expression){const result=await client.send("Runtime.evaluate",{expression,awaitPromise:true,returnByValue:true});if(result.exceptionDetails)throw new Error(result.exceptionDetails.exception?.description||result.exceptionDetails.text||"renderer evaluation failed");return result.result?.value;}

const target=(await targetList()).find((entry)=>entry.type==="page"&&entry.title==="Phoenix");
if(!target)throw new Error("Phoenix UI target is missing");
const client=cdp(target.webSocketDebuggerUrl);await client.send("Runtime.enable");
let report;try{report=await evaluate(client,`(async()=>{
  const wait=async(predicate,timeout=12000)=>{const deadline=performance.now()+timeout;while(performance.now()<deadline){const value=predicate();if(value)return value;await new Promise((resolve)=>setTimeout(resolve,40));}throw new Error("live check timed out");};
  const stage=(value)=>document.documentElement.dataset.averyCheckStage=value;
  await wait(()=>window.PhoenixUI?.state?.view&&window.PhoenixConversation);
  stage("selecting-avery");
  const started=performance.now();await Promise.race([window.PhoenixUI.selectItem({kind:"agent",id:"school_coach"}),new Promise((_,reject)=>setTimeout(()=>reject(new Error("Avery selection timed out")),15000))]);
  await wait(()=>document.getElementById("stageName")?.textContent==="Avery"&&!document.getElementById("conversationFeed")?.hasAttribute("aria-busy")&&!document.querySelector(".conversation-loading"));
  stage("avery-loaded");
  const loadMs=performance.now()-started,feed=document.getElementById("conversationFeed"),initial={loadMs,totalRows:Number(feed.dataset.totalRows),renderedRows:Number(feed.dataset.renderedRows),renderedTurns:Number(feed.dataset.renderedTurns),hasEarlier:feed.dataset.hasEarlier==="true",opacity:getComputedStyle(feed).opacity,domChildren:feed.children.length,oldProviderFailureVisible:feed.textContent.includes("could not complete its turn: codex stream reported an error")};
  feed.scrollTop=0;document.querySelector(".conversation-history-loader")?.click();let pagingTimedOut=false;try{await wait(()=>Number(feed.dataset.renderedTurns)>=10,10000);}catch{pagingTimedOut=true;}const paging={renderedTurns:Number(feed.dataset.renderedTurns),renderedRows:Number(feed.dataset.renderedRows),scrollTop:feed.scrollTop,hasEarlier:feed.dataset.hasEarlier==="true",timedOut:pagingTimedOut};
  stage("history-paged");
  const promptRail=document.getElementById("conversationPromptRail"),wasCollapsed=document.body.classList.contains("sidebar-collapsed");
  if(!wasCollapsed)document.getElementById("sidebarToggle").click();
  await wait(()=>document.body.classList.contains("sidebar-collapsed"),3000);
  const settingsButton=document.getElementById("settingsButton"),uiChecks={promptMarkers:!promptRail.hidden&&getComputedStyle(promptRail).display==="flex"&&promptRail.querySelectorAll("button").length>0,collapsedSettings:getComputedStyle(settingsButton).display!=="none"&&settingsButton.getBoundingClientRect().width>=34};
  if(!wasCollapsed)document.getElementById("sidebarToggle").click();
  await wait(()=>document.body.classList.contains("sidebar-collapsed")===wasCollapsed,3000);
  window.PhoenixConversation.showInspectionSidebar("changes");
  await wait(()=>document.getElementById("reviewTurnPicker")?.textContent.includes("Avery edits"),5000);
  uiChecks.agentReview=document.getElementById("reviewTurnPicker").textContent.includes("Avery edits");
  stage("ui-checked");
  const proofUrl=${JSON.stringify(proofUrl)};
  if(proofUrl){
    const toast=document.getElementById("toastRegion");toast.replaceChildren();
    stage("opening-browser");
    await Promise.race([window.PhoenixConversation.openBrowser("school_coach","browse"),new Promise((_,reject)=>setTimeout(()=>reject(new Error("browser open timed out")),25000))]);
    stage("browser-open");
    const beforeTabs=document.querySelectorAll("[data-browser-tab-id]").length;
    document.getElementById("browserNewTab").click();
    await wait(()=>document.querySelectorAll("[data-browser-tab-id]").length>beforeTabs,10000);
    stage("temporary-tab-open");
    const address=document.getElementById("browserAddress");address.value=proofUrl;address.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true}));
    await wait(()=>{try{return new URL(address.value).host===new URL(proofUrl).host;}catch{return false;}},20000);
    stage("temporary-tab-navigated");
    await new Promise((resolve)=>setTimeout(resolve,500));
    const viewport=document.getElementById("browserViewport");
    const activeTab=document.querySelector('[data-browser-tab-id][aria-selected="true"]');
    const proof={url:address.value,createdTabId:activeTab?.dataset.browserTabId||"",native:viewport.classList.contains("native-surface"),nativeError:viewport.dataset.nativeError||"",sidebarOpen:!document.getElementById("browserOverlay").hidden,owner:window.PhoenixUI.state.selected?.id,toast:toast.textContent.trim()};
    window.PhoenixConversation.closeInspectionSidebar();
    await wait(()=>document.getElementById("browserOverlay").hidden,3000);
    proof.hiddenWithoutClosing=true;
    window.PhoenixConversation.showInspectionSidebar("browser");
    await wait(()=>!document.getElementById("browserOverlay").hidden&&viewport.classList.contains("native-surface"),10000);
    stage("sidebar-reopened");
    await new Promise((resolve)=>setTimeout(resolve,500));
    proof.reopenedUrl=document.getElementById("browserAddress").value;
    proof.reopenedNative=viewport.classList.contains("native-surface");
    return{initial,paging,uiChecks,browser:{before:0,after:0,error:"",tabs:[],cleaned:true},proof,selected:window.PhoenixUI.state.selected};
  }
  const toast=document.getElementById("toastRegion");toast.replaceChildren();document.getElementById("browserNewTab").click();
  await wait(()=>!document.getElementById("browserOverlay").hidden&&document.querySelectorAll("[data-browser-tab-id]").length>=1,20000);
  const before=document.querySelectorAll("[data-browser-tab-id]").length;document.getElementById("browserNewTab").click();
  await wait(()=>document.querySelectorAll("[data-browser-tab-id]").length>before||toast.textContent.trim(),20000);
  const after=document.querySelectorAll("[data-browser-tab-id]").length,error=toast.textContent.trim(),tabs=[...document.querySelectorAll("[data-browser-tab-id]")].map((tab)=>({id:tab.dataset.browserTabId,title:tab.textContent.trim(),active:tab.getAttribute("aria-selected")==="true"}));
  if(after>before){document.querySelectorAll("[data-browser-tab-id]")[after-1].querySelector("[data-close-browser-tab]")?.click();await wait(()=>document.querySelectorAll("[data-browser-tab-id]").length===before,10000);}
  let proof=null;
  return{initial,paging,uiChecks,browser:{before,after,error,tabs,cleaned:document.querySelectorAll("[data-browser-tab-id]").length===before},proof,selected:window.PhoenixUI.state.selected};
})()`);}catch(error){let stage="unknown";try{stage=await evaluate(client,'document.documentElement.dataset.averyCheckStage||"unset"');}catch{}client.close();throw new Error(`${error.message} (stage: ${stage})`);}
const gatewaySurface=proofUrl?await gatewaySurfaceStatus("agent-school_coach"):null;
const sharedTabs=gatewaySurface?.BrowserSurface?.tabs||[];
const proofOk=!proofUrl||(report.proof?.url&&new URL(report.proof.url).host===new URL(proofUrl).host&&report.proof?.native&&report.proof?.sidebarOpen&&report.proof?.owner==="school_coach"&&!report.proof?.toast&&report.proof?.hiddenWithoutClosing&&report.proof?.reopenedNative&&new URL(report.proof.reopenedUrl).host===new URL(proofUrl).host&&sharedTabs.some((tab)=>{try{return new URL(tab.url).host===new URL(proofUrl).host;}catch{return false;}}));
const browserOk=proofUrl?true:report.browser.after===report.browser.before+1&&!report.browser.error&&report.browser.cleaned;
const ok=report.initial.loadMs<1000&&report.initial.totalRows>report.initial.renderedRows&&report.initial.renderedTurns===5&&report.initial.hasEarlier&&report.initial.opacity==="1"&&!report.initial.oldProviderFailureVisible&&report.paging.renderedTurns===10&&report.paging.scrollTop>0&&Object.values(report.uiChecks).every(Boolean)&&browserOk&&report.selected?.id==="school_coach"&&proofOk;
if(report.proof?.createdTabId){await evaluate(client,`(async()=>{const id=${JSON.stringify(report.proof?.createdTabId)},wait=async(predicate)=>{const end=performance.now()+10000;while(performance.now()<end){if(predicate())return;await new Promise((resolve)=>setTimeout(resolve,40));}};document.querySelector('[data-browser-tab-id="'+CSS.escape(id)+'"] [data-close-browser-tab]')?.click();await wait(()=>!document.querySelector('[data-browser-tab-id="'+CSS.escape(id)+'"]'));return true;})()`);}
const gatewayAfterCleanup=report.proof?.createdTabId?await gatewaySurfaceStatus("agent-school_coach"):null;
const cleanupOk=!report.proof?.createdTabId||!gatewayAfterCleanup?.BrowserSurface?.tabs?.some((tab)=>tab.id===report.proof.createdTabId);
client.close();
console.log(`PHOENIX_AVERY_LIVE ${JSON.stringify({ok:ok&&cleanupOk,...report,gatewaySurface,gatewayAfterCleanup,cleanupOk})}`);if(!ok||!cleanupOk)process.exitCode=2;
