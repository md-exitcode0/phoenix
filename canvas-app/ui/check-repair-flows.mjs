import assert from "node:assert/strict";
import {readFileSync} from "node:fs";
import vm from "node:vm";

const source=readFileSync(new URL("./conversation.js",import.meta.url),"utf8");
function extract(name) {
  const start=source.search(new RegExp(`^  (?:async )?function ${name}\\(`,"m"));
  assert.ok(start>=0,name);
  const tail=source.slice(start),end=tail.slice(1).search(/\n  (?:async )?function /);
  return end<0?tail:tail.slice(0,end+1);
}
const calls=[],painted=[];
const state={browserOwnerId:"rory",browserSurfaceGeneration:3,browserBoundKey:"agent:rory",browserTabs:[{id:"chosen",active:true}],browserFrameUrl:"about:blank",item:{id:"phoenix"}};
let flush=async()=>{},invoke=async(command,args)=>({supported:true,tabs:[{id:args.targetId,active:true,url:args.url}]});
const context=vm.createContext({state,
  flushBrowserTyping:()=>flush(),
  ui:{invoke:async(command,args)=>{calls.push({command,...args});return invoke(command,args);}},
  renderInspectionBrowserTabs:surface=>painted.push(surface),applyBrowserLocation:()=>{},
  canonicalAgentId:id=>id||"",ownedStoryTurn:row=>row?.execution?.turn_id||"",
  isAuthoredBoundaryEntry:entry=>entry.value?.kind==="user"||entry.value?.role==="user",
});
for(const name of ["navigateNativeBrowser","displayRole","displayTurnId","askIsPending","markQuestionContinuations"])vm.runInContext(extract(name),context);
flush=async()=>{state.browserTabs=[{id:"other",active:true}];};
await context.navigateNativeBrowser("navigate","https://example.test/changed");
assert.equal(calls.at(-1).targetId,"chosen","a tab switch while typing flushes must not redirect navigation");
assert.equal(calls.at(-1).url,"https://example.test/changed");
state.browserTabs=[{id:"chosen",active:true}];
await context.navigateNativeBrowser("reload");
assert.equal(calls.at(-1).targetId,"chosen","reload retains the captured tab too");
assert.equal(calls.at(-1).action,"reload");
flush=async()=>{state.browserBoundKey="agent:theo";};
const before=calls.length;
await assert.rejects(context.navigateNativeBrowser("navigate","https://example.test/other"),/superseded/);
assert.equal(calls.length,before,"a switched conversation gets no foreign navigation");
state.browserBoundKey="agent:rory";flush=async()=>{};
invoke=async()=>{throw new Error("browser surface has no open tab");};
await assert.rejects(context.navigateNativeBrowser("navigate","https://example.test/closed"),/no open tab/);
const paintCount=painted.length;
invoke=async()=>{state.browserSurfaceGeneration++;return {supported:true,tabs:[]};};
await context.navigateNativeBrowser("navigate","https://example.test/stale");
assert.equal(painted.length,paintCount,"a stale receipt must not paint another surface");
console.log("PASS: captured tab navigation/reload, conversation switch, closed tab, and stale completion.");

const row=(turn,kind,extra={})=>({source:"story",turn_id:turn,value:{kind,...extra}});
const question=row("first","ask_pending",{agent:"phoenix",status:"pending"});
const waiting=row("first","answer",{agent:"phoenix",markdown:"Waiting for your choice"});
const coworker=row("first","answer",{agent:"researcher",markdown:"Research result"});
const otherTurn=row("second","answer",{agent:"phoenix",markdown:"Finished another request"});
context.markQuestionContinuations([question,waiting,coworker,otherTurn]);
assert.equal(waiting.value.awaiting_input,true);
assert.equal(coworker.value.awaiting_input,undefined,"one coworker's question cannot hide another's final");
assert.equal(otherTurn.value.awaiting_input,undefined,"a pending question is scoped to its own turn");
question.value.status="answered";question.value.resolved_at="2026-09-30T20:00:00Z";
const restored=JSON.parse(JSON.stringify([question,waiting]));
context.markQuestionContinuations(restored);
assert.equal(restored[1].value.awaiting_input,true,"resolution and journal restore cannot resurrect the earlier final");
const completed=row("first","answer",{agent:"phoenix",markdown:"Completed after your response"});
context.markQuestionContinuations([question,waiting,completed]);
assert.equal(completed.value.awaiting_input,undefined,"a final after a resolved question is still final");
console.log("PASS: inline question waiting, owner/turn isolation, durable resolution, and the completed response.");
