"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),{EventEmitter}=require("node:events");
const {install}=require("./frontend-recovery.cjs");
const URL="http://127.0.0.1:47845/?skin=phoenix&chromium=1";
const flush=()=>new Promise(r=>setImmediate(r));
class Contents extends EventEmitter{
 constructor(pid=42){super();this.pid=pid;this.url=URL;this.destroyed=false;this.crashed=false;this.actions=[];}
 isDestroyed(){return this.destroyed;}getURL(){return this.url;}getOSProcessId(){return this.pid;}isCrashed(){return this.crashed;}
 forcefullyCrashRenderer(){this.actions.push("crash");this.crashed=true;this.emit("render-process-gone",{}, {reason:"killed"});}
 reload(){this.actions.push("reload");}
}
function fixture(){
 const contents=new Contents(),window=new EventEmitter(),timers=new Map(),questions=[],logs=[],other=new Contents(43);
 window.webContents=contents;window.isDestroyed=()=>false;window.visible=true;window.minimized=false;window.focused=true;window.actions=[];window.isVisible=()=>window.visible;window.isMinimized=()=>window.minimized;window.isFocused=()=>window.focused;window.hide=()=>window.actions.push("hide");window.show=()=>window.actions.push("show");window.showInactive=()=>window.actions.push("showInactive");
 const options={frontendURL:URL,getAllWebContents:()=>[contents,other],log:s=>logs.push(s),
  setTimer:fn=>{const key={unref(){}};timers.set(key,fn);return key;},clearTimer:key=>timers.delete(key),
  dialog:{showMessageBox:(owner,options)=>new Promise(resolve=>questions.push({owner,options,resolve}))}};
 const control=install(window,options);
 return{window,contents,other,control,options,timers,questions,logs,
  tick(){const item=timers.entries().next().value;if(item){timers.delete(item[0]);item[1]();}},
  stall(){contents.emit("unresponsive");},answer(value){questions.at(-1).resolve({response:value});}};
}
test("installation is idempotent",()=>{const f=fixture();assert.equal(install(f.window,f.options),f.control);assert.equal(f.contents.listenerCount("unresponsive"),1);f.control.dispose();});
test("transient unresponsiveness never opens a recovery question",()=>{const f=fixture();f.stall();f.contents.emit("responsive");f.tick();assert.equal(f.questions.length,0);assert.equal(f.control.status().stalled,false);f.control.dispose();});
test("repeated hang events open exactly one native question",()=>{const f=fixture();f.stall();f.stall();f.tick();f.stall();assert.equal(f.questions.length,1);assert.equal(f.questions[0].owner,f.window);assert.equal(f.questions[0].options.defaultId,1);f.control.dispose();});
test("Keep waiting never kills or reloads the interface",async()=>{const f=fixture();f.stall();f.tick();f.answer(1);await flush();assert.deepEqual(f.contents.actions,[]);assert.equal(f.control.status().stalled,true);f.control.dispose();});
test("recovery is cancelled if the renderer becomes responsive while the question is open",async()=>{const f=fixture();f.stall();f.tick();const q=f.questions[0];f.contents.emit("responsive");assert.equal(q.options.signal.aborted,true);q.resolve({response:0});await flush();assert.deepEqual(f.contents.actions,[]);f.control.dispose();});
test("closed windows cancel recovery and remove listeners",async()=>{const f=fixture();f.stall();f.tick();f.window.emit("closed");f.answer(0);await flush();assert.deepEqual(f.contents.actions,[]);assert.equal(f.contents.listenerCount("before-input-event"),0);assert.equal(f.contents.listenerCount("unresponsive"),0);});
test("explicit Reload resets only the identified interface",async()=>{const f=fixture();f.stall();f.tick();f.answer(0);await flush();assert.deepEqual(f.contents.actions,["crash","reload"]);assert.deepEqual(f.other.actions,[]);assert.equal(f.questions.length,1);assert.equal(f.control.status().recovering,true);f.contents.emit("did-finish-load");assert.equal(f.control.status().stalled,false);f.control.dispose();});
test("an already crashed interface reloads without crashing another process",async()=>{const f=fixture();f.contents.crashed=true;f.contents.pid=0;f.contents.emit("render-process-gone",{}, {reason:"crashed"});f.tick();f.answer(0);await flush();assert.deepEqual(f.contents.actions,["reload"]);f.control.dispose();});
test("a shared renderer is protected even when Reload was chosen",async()=>{const f=fixture();f.other.pid=f.contents.pid;f.stall();f.tick();f.answer(0);await flush();assert.deepEqual(f.contents.actions,[]);assert.match(f.control.status().lastError,/shares this renderer/);f.control.dispose();});
test("navigation to another page is never reset by interface recovery",async()=>{const f=fixture();f.contents.url="https://example.com/";f.stall();f.tick();f.answer(0);await flush();assert.deepEqual(f.contents.actions,[]);assert.match(f.control.status().lastError,/changed its frontend/);f.control.dispose();});
test("a later independent freeze can recover after a successful reload",async()=>{const f=fixture();f.stall();f.tick();f.answer(0);await flush();f.contents.crashed=false;f.contents.emit("did-finish-load");f.stall();f.tick();assert.equal(f.questions.length,2);f.control.dispose();});
test("native Ctrl Shift R retries after Keep waiting",async()=>{const f=fixture();f.stall();f.tick();f.answer(1);await flush();let prevented=false;f.contents.emit("before-input-event",{preventDefault(){prevented=true;}},{type:"keyDown",key:"R",control:true,shift:true});assert.equal(prevented,true);assert.equal(f.questions.length,2);f.control.dispose();});
test("recovery shortcut leaves a healthy interface alone",()=>{const f=fixture();let prevented=false;f.contents.emit("before-input-event",{preventDefault(){prevented=true;}},{type:"keyDown",key:"R",control:true,shift:true});assert.equal(prevented,false);assert.equal(f.questions.length,0);f.control.dispose();});
test("a failed native question is recorded without any renderer reset",async()=>{const f=fixture();f.options.dialog.showMessageBox=async()=>{throw Error("native dialog failed")};f.stall();f.tick();await flush();assert.deepEqual(f.contents.actions,[]);assert.match(f.control.status().lastError,/native dialog failed/);f.control.dispose();});

test("accepted renderer recovery reattaches the same native surface exactly once",async()=>{const f=fixture();f.stall();f.tick();f.answer(0);await flush();f.contents.emit("responsive");f.contents.emit("did-finish-load");f.contents.emit("did-finish-load");assert.deepEqual(f.window.actions,["hide","show"]);assert.deepEqual(f.other.actions,[]);f.control.dispose();});
test("recovery does not reveal a hidden or minimized window",async()=>{for(const property of ["visible","minimized"]){const f=fixture();f.stall();f.tick();f.answer(0);await flush();f.window[property]=property==="minimized";f.contents.emit("did-finish-load");assert.deepEqual(f.window.actions,[]);f.control.dispose();}});
test("recovery preserves another window's focus",async()=>{const f=fixture();f.window.focused=false;f.stall();f.tick();f.answer(0);await flush();f.contents.emit("did-finish-load");assert.deepEqual(f.window.actions,["hide","showInactive"]);f.control.dispose();});
test("ordinary navigation never remaps the native window",()=>{const f=fixture();f.contents.emit("did-finish-load");assert.deepEqual(f.window.actions,[]);f.control.dispose();});
