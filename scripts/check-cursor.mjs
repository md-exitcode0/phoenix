import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {paintCursor, movementDuration, ARROW} from '../desktop/gnome-extension/phoenix-cursor@phoenix.dev/cursor-art.mjs';
import {validateWindowActions, parseKeyCombo, keypadKeycode, KEYSYMS} from '../desktop/gnome-extension/phoenix-cursor@phoenix.dev/cursor-input.mjs';

assert.deepEqual(ARROW[0], [0, 0]);
assert(ARROW.every(([x,y]) => x + 10 + 1.6 < 38 && y + 10 + 1.6 < 42));
assert.equal(movementDuration(1500), 166);
assert.equal(movementDuration(1500, 900), 900);
assert.equal(movementDuration(1e6), 180);
const invalidCombos=['ctrl+typo','ctrl++a','a+b','ctrl+control+a','constructor','ctrl+toString','a+','\u0001','\ud800'];
for(const combo of invalidCombos)assert.throws(()=>parseKeyCombo(combo));
assert.deepEqual(parseKeyCombo('ctrl+plus'),{mods:[0xffe3],main:43});
assert.deepEqual(parseKeyCombo('super'),{mods:[],main:0xffeb});
assert.deepEqual(parseKeyCombo('Ctrl + Shift + A'),{mods:[0xffe3,0xffe1],main:65});
assert.equal(parseKeyCombo('😀').main,0x0101f600);
let positions = [];
const cr = new Proxy({}, {get: (_, name) => (...args) => positions.push([name, ...args])});
paintCursor(cr);
assert.deepEqual(positions[0], ['translate', 10, 10]);
assert.deepEqual(positions[1], ['moveTo', 0, 0]);
assert(!positions.some(([name]) => name === 'arc'), 'no obscuring halo');

// Exercise the actual extension methods with compositor boundaries stubbed.
// This is not a claim that Node can validate GNOME Shell API compatibility.
const source = await readFile(new URL('../desktop/gnome-extension/phoenix-cursor@phoenix.dev/extension.js', import.meta.url), 'utf8');
let timers = 0;
const activeTimers = new Map();
const Clutter = {BUTTON_PRIMARY:1,ButtonState:{PRESSED:1,RELEASED:0},KeyState:{PRESSED:1,RELEASED:0},AnimationMode:{EASE_OUT_QUAD:1,EASE_IN_OUT_CUBIC:2}};
const GLib = {PRIORITY_DEFAULT:0, timeout_add: (_,ms,fn) => {activeTimers.set(++timers,{ms,fn});return timers;}, source_remove:id=>activeTimers.delete(id)};
GLib.Variant={new:(_,value)=>value};
GLib.uuid_string_random=()=> 'fixture-unique-id';
let now=0;
GLib.get_monotonic_time=()=>now*1000;
let captureCallback, closedStreams=0, openedStreams=0, captureStarts=0;
const fakeGlobal={get_pointer:()=>[55,66],display:{get_focus_window:()=>null}};
let publishedCaptures=0, discardedCaptures=0;
const fakeGio={FileCopyFlags:{NONE:0},FileCreateFlags:{NONE:0,PRIVATE:1},File:{new_for_path:()=>({create:()=>{
 openedStreams++;return {close:()=>closedStreams++};
},move:()=>publishedCaptures++,delete:()=>discardedCaptures++})}};
const fakeShell={Screenshot:class {
 screenshot_window(_frame,_cursor,_stream,callback){captureStarts++;captureCallback=callback;}
 screenshot_window_finish(){return [true,null];}
}};
const fakeMain={activateWindow:win=>{fakeGlobal.display.focus_window=win;}};
const Cursor = new Function('Extension','Clutter','GLib','paintCursor','movementDuration','global','Date','validateWindowActions','Gio','Shell','Main','parseKeyCombo','KEYSYMS','keypadKeycode',
    source.replace(/^import .*;\n/gm, '').replace('export default class', 'return class'))(
    class {}, Clutter, GLib, paintCursor, movementDuration,fakeGlobal,{now:()=>now},validateWindowActions,fakeGio,fakeShell,fakeMain,parseKeyCombo,KEYSYMS,keypadKeycode);
const cursor = new Cursor();
cursor._timeouts = new Set();
let pending;
cursor._cursor = {x:0,y:0,opacity:0,remove_transition(){},remove_all_transitions(){},destroy(){},ease(options){pending=options;}};
let arrivals = 0;
cursor._animateTo(1510,10,0,()=>arrivals++);
assert.equal(arrivals,0,'input continuation must wait for arrival');
assert.equal(pending.duration,166);
assert.equal(pending.x,1500);
assert.equal(pending.y,0);
assert.equal(timers,2,'idle fade and one deadline, not a movement trail timer');
cursor._cursor.x = pending.x;
cursor._cursor.y = pending.y;
pending.onComplete();
assert.equal(arrivals,1);
assert.deepEqual(cursor._cursorCenter(),[1510,10]);
const replies = () => ({events:[],return_value(v){this.events.push(['ok',v]);},return_dbus_error(name,reason){this.events.push([name,reason]);}});
let moving = replies(), busy = replies();
cursor.MoveToAsync([100,100,50],moving);
let obsolete = pending.onComplete;
cursor.MoveToAsync([200,200,50],busy);
assert.equal(busy.events[0][0],'dev.phoenix.Cursor.Busy');
assert.equal(moving.events.length,0);
const blockedClick = replies(); cursor.ClickAsync(['left'], blockedClick);
assert.equal(blockedClick.events[0][0], 'dev.phoenix.Cursor.Busy');
cursor.Hide();
obsolete();
assert.equal(moving.events.length,1,'cancellation and stale completion must not reply twice');
assert.equal(moving.events[0][0],'dev.phoenix.Cursor.Canceled');

let buttons=[],points=[];
cursor._now=()=>1;
cursor._pointerTo=(x,y)=>points.push([x,y]);
cursor._spawnRipple=()=>{};
cursor._vpointer={notify_button:(timestamp,button,state)=>{
 assert.equal(timestamp,GLib.get_monotonic_time(),'virtual pointer events use microseconds, not the window-focus clock');
 buttons.push([button,state]);
},notify_relative_motion:(timestamp,dx,dy)=>{
 assert.equal(timestamp,GLib.get_monotonic_time());
 const [x,y]=points.at(-1)??fakeGlobal.get_pointer();points.push([x+dx,y+dy]);
}};
cursor._restorePointer=true;
for(const cancel of [false,true]){
 buttons=[];points=[];
 const drag=replies();
 cursor.DragAsync([250,270,100],drag);
 const completed=pending.onComplete;
 assert.equal([...activeTimers.values()].filter(t=>t.ms===30).length,1);
 if(cancel) cursor.Hide(); else completed();
 completed();
 assert.equal(drag.events.length,1);
 assert.equal(drag.events[0][0],cancel?'dev.phoenix.Cursor.Canceled':'ok');
 assert.deepEqual(buttons,[[1,1],[1,0]],'drag always releases its button once');
 assert.deepEqual(points.at(-1),[55,66]);
 assert.equal([...activeTimers.values()].filter(t=>t.ms===30||t.ms===120).length,0,'no drag loop or delayed restoration survives settlement');
}
for (const drag of [false,true]) {
 const stalled=replies();buttons=[];points=[];
 if(drag) cursor.DragAsync([250,270,600],stalled);
 else cursor.MoveToAsync([250,270,600],stalled);
 const late=pending.onComplete;
 const id=cursor._motion.watchdog;
 const timer=activeTimers.get(id);
 assert.equal(timer.ms,1600,'deadline follows the requested duration');
 // Model GLib removing its one-shot source when the callback returns.
 activeTimers.delete(id);timer.fn();late();
 assert.equal(stalled.events.length,1);
 assert.match(stalled.events[0][1],/deadline/);
 assert(!cursor._motion);
 if(drag){assert.deepEqual(buttons,[[1,1],[1,0]]);assert.deepEqual(points.at(-1),[55,66]);}
 assert.equal([...activeTimers.values()].filter(t=>t.ms===30||t.ms===1600).length,0);
}
points=[];
const timerCount=timers;
const failedClick = replies();
cursor._withPointerAt(7,8,()=>{throw new Error('input failed');}, failedClick);
assert.equal(failedClick.events.length,0);
assert.deepEqual(points,[[7,8]]);
assert.equal(timers,timerCount+1,'failed clicks still schedule restoration');
const restoreId=timers;
activeTimers.get(restoreId).fn();activeTimers.delete(restoreId);
assert.deepEqual(points.at(-1),[55,66]);
assert.match(failedClick.events[0][1],/input failed/);
assert(!cursor._pointerAction);

// Before-control: the former helper returned while this callback could still
// overwrite a new operation's position. Compositor timing is controlled here.
let oldRestore;
const oldPointer = (x,y,action) => { points.push([x,y]); try { action(); }
 finally { oldRestore = () => points.push([55,66]); } };
points=[];oldPointer(7,8,()=>{});points.push([90,91]);oldRestore();
assert.deepEqual(points.at(-1),[55,66],'before: stale callback steals the next position');
for (const method of ['ClickAsync','DoubleClickAsync','ScrollAsync']) {
 for (const cancel of [false,true]) {
  points=[];
  const reply=replies();cursor[method](method==='ScrollAsync'?[0,0]:['left'],reply);
  assert.equal(reply.events.length,0,'no completion before restoration');
  const stale = [...activeTimers.values()].find(t=>t.ms===120).fn;
  const rival=replies();cursor.DragAsync([4,5,100],rival);
  assert.equal(rival.events[0][0],'dev.phoenix.Cursor.Busy');
  assert.throws(()=>cursor._directKey('a'),/busy/);
  assert.throws(()=>cursor.FocusWindow(1),/busy/);
  if(cancel) cursor.Hide(); else fire(120);
  assert.equal(reply.events.length,1);
  assert.equal(reply.events[0][0],cancel?'dev.phoenix.Cursor.Canceled':'ok');
  assert.deepEqual(points.at(-1),[55,66]);
  assert(!cursor._pointerAction);
  const next=replies();cursor.ClickAsync(['left'],next);
  const count=points.length;stale();
  assert.equal(points.length,count,'obsolete restore cannot move the next action');
  assert.equal(next.events.length,0);
  fire(120);assert.equal(next.events.length,1);
 }
}
console.log('CURSOR_POINTER_OWNERSHIP_OK: before-control reproduces late warp; replies now wait for restore, competitors rejected, stale callbacks fenced');
const notify=cursor._vpointer.notify_button;
buttons=[];
cursor._vpointer.notify_button=(_,button,state)=>{buttons.push([button,state]);if(state===1)throw new Error('uncertain press');};
assert.throws(()=>cursor._pressRelease(1),/uncertain press/);
assert.deepEqual(buttons,[[1,1],[1,0]],'uncertain press still attempts release');
cursor._vpointer.notify_button=notify;
const realEase=cursor._cursor.ease;
cursor._cursor.ease=()=>{throw new Error('compositor refused animation');};
const refused=replies();
cursor.MoveToAsync([1,1,1],refused);
assert.equal(refused.events.length,1);
assert.equal(refused.events[0][0],'dev.phoenix.Cursor.Canceled');
buttons=[];
const refusedDrag=replies();
cursor.DragAsync([1,1,1],refusedDrag);
assert.deepEqual(buttons,[[1,1],[1,0]]);
assert.equal(refusedDrag.events.length,1);
assert(!cursor._motion);
cursor._cursor.ease=realEase;
let focusCalls=0,typed=[];
cursor._windowById=()=>({win:{focus(){focusCalls++;},raise(){},get_buffer_rect:()=>({x:0,y:0})}});
cursor._userIdleMs=()=>1000;
for(const invalid of [null, {type:'move',x:-1,y:0}, {type:'move',x:1,y:2,button:'left'}, {type:'move',x:1}, {type:'drag'}, {type:'click',x:'4',y:2}, {type:'click',x:1,y:2,button:'typo'},
 {type:'click',x:1.5,y:2}, {type:'wait',ms:-1}, {type:'wait',ms:10001}, {type:'scroll',x:1,y:2,dy:31},
 {type:'type',text:{}}, {type:'key',combo:''}, {type:'click',x:1,y:2,clicks:2},
 ...invalidCombos.map(combo=>({type:'key',combo}))]) {
 const reply=replies(),before=focusCalls;
 cursor.WindowBatchAsync([42,JSON.stringify([{type:'type',text:'must not run'},invalid])],reply);
 assert.equal(reply.events.length,1);
 const result=JSON.parse(reply.events[0][1][0]);
 assert.equal(result.ok,false);assert.equal(result.executed,0);
 assert.equal(focusCalls,before);assert(!cursor._batch);
}
assert.throws(()=>validateWindowActions(Array.from({length:26},()=>({type:'wait',ms:0}))));
assert.equal(validateWindowActions([{type:'click',x:0,y:0},{type:'scroll',x:1,y:1,dy:-30},{type:'wait',ms:10000}]).length,3);
const doubleClick={type:'double_click',x:123,y:456,button:'left'};
assert.deepEqual(validateWindowActions([doubleClick]),[{type:'click',x:123,y:456,button:'left',double:true}]);
assert.equal(doubleClick.type,'double_click','validation preserves caller input');
for(const action of [{...doubleClick,x:-1},{...doubleClick,button:'invalid'},{...doubleClick,double:false},{...doubleClick,clicks:2}])assert.throws(()=>validateWindowActions([action]));

console.log('CURSOR_PREFLIGHT_OK: malformed batches rejected before any focus/input, no silent coercion or skipped success');
cursor._typeText=text=>typed.push(text);
function fire(ms){const entry=[...activeTimers].find(([,timer])=>timer.ms===ms);assert(entry,`missing ${ms}ms timer`);activeTimers.delete(entry[0]);entry[1].fn();return entry[1].fn;}
function fireGlide(){const entry=[...activeTimers].find(([,timer])=>timer.ms>=60&&timer.ms<=180);assert(entry,'missing quick cursor-glide timer');activeTimers.delete(entry[0]);entry[1].fn();return entry[1].ms;}
const beforeDragWindow=cursor._windowById, beforeShow=cursor._showCursor;
let dragFocus=true;
cursor._windowById=()=>({win:{focus(){},raise(){},has_focus:()=>dragFocus,get_buffer_rect:()=>({x:100,y:200,width:500,height:400})}});
cursor._cursor.set_position=(x,y)=>{cursor._cursor.x=x;cursor._cursor.y=y;};
cursor._showCursor=()=>{};
for (const [x, valid] of [[20,true],[500,false]]) {
 buttons=[];points=[];
 const reply=replies();
 cursor.WindowBatchAsync([42,JSON.stringify([{type:'move',x,y:30}])],reply);
 fire(80);
 if(valid){assert.notDeepEqual(points.at(-1),[120,230],'window move must not teleport before its visible glide finishes');const duration=fireGlide();assert(duration>=60&&duration<=180);assert.deepEqual(points.at(-1),[120,230]);fire(60);}
 assert.deepEqual(buttons,[],'hover never presses a mouse button');
 assert.deepEqual(points.at(-1),[55,66],'hover restores user pointer');
 const result=JSON.parse(reply.events[0][1][0]);
 assert.equal(result.ok,valid);assert.equal(result.completed_actions,valid?1:0);
}
console.log('CURSOR_WINDOW_MOVE_OK: window coordinates, no click, bounds rejection, pointer restore');
buttons=[];points=[];
const smoothClick=replies();
cursor.WindowBatchAsync([42,JSON.stringify([{type:'click',x:40,y:50,button:'left'}])],smoothClick);
fire(80);
assert.deepEqual(buttons,[],'window click waits for the cursor glide before pressing');
const clickDuration=fireGlide();assert(clickDuration>=60&&clickDuration<=180);
assert.deepEqual(points.at(-1),[140,250]);
assert.deepEqual(buttons,[[1,1],[1,0]],'click is delivered only after arrival');
fire(60);
assert.deepEqual(points.at(-1),[55,66],'window click restores the user pointer after the batch');
assert.equal(JSON.parse(smoothClick.events[0][1][0]).ok,true);
console.log('CURSOR_WINDOW_CLICK_GLIDE_OK: managed clicks animate briefly before input and still restore pointer ownership');
for (const mode of ['complete','cancel','focus-loss','cancel-before-press']) {
 buttons=[];points=[];dragFocus=true;
 const reply=replies();
 cursor.WindowBatchAsync([42,JSON.stringify([{type:'drag',from_x:20,from_y:30,to_x:200,to_y:100,duration_ms:100}])],reply);
 fire(80);
 assert.deepEqual(buttons,[],'initial pointer movement precedes the press');
 if(mode==='cancel-before-press'){
   const stale=[...activeTimers.values()].find(t=>t.ms===16).fn;
   cursor.Hide();stale();
   assert.deepEqual(buttons,[],'canceled preparation never presses a button');
   assert.equal(reply.events.length,1);assert(!cursor._batch);
   assert.deepEqual(points.at(-1),[55,66]);
   continue;
 }
 fire(16);
 const stale=[...activeTimers.values()].find(t=>t.ms===16).fn;
 assert.deepEqual(buttons,[[1,1]]);
 if(mode==='cancel')cursor.Hide();
 else if(mode==='focus-loss'){dragFocus=false;fire(16);}
 else {now+=50;fire(16);assert.deepEqual(points.at(-1),[210,265]);now+=50;fire(16);fire(60);}
 stale();
 assert.deepEqual(buttons,[[1,1],[1,0]],'release exactly once on every exit');
 assert.deepEqual(points.at(-1),[55,66],'restore user pointer after release');
 assert.equal(reply.events.length,1);
 const result=JSON.parse(reply.events[0][1][0]);
 assert.equal(result.ok,mode==='complete');
 assert.equal(result.completed_actions,mode==='complete'?1:0);
 assert(!cursor._batch);
}
cursor._windowById=beforeDragWindow;cursor._showCursor=beforeShow;
console.log('CURSOR_WINDOW_DRAG_OK: window offsets, intermediate motion, release, cancellation, focus loss, and stale callback fencing');
for(const cancel of [false,true]){
 const batch=replies();typed=[];
 cursor.WindowBatchAsync([42,JSON.stringify([{type:'wait',ms:10},{type:'type',text:'owned input'}])],batch);
 const competing=replies();cursor.MoveToAsync([1,1,10],competing);
 assert.equal(competing.events[0][0],'dev.phoenix.Cursor.Busy');
 assert.throws(()=>cursor._directKey('a'),/busy/);
 fire(80);
 const stale=[...activeTimers.values()].find(t=>t.ms===10).fn;
 if(cancel){cursor.Hide();stale();assert.deepEqual(typed,[]);}
 else {fire(10);assert.deepEqual(typed,['owned input']);fire(60);}
 assert.equal(batch.events.length,1);
 assert.equal(JSON.parse(batch.events[0][1][0]).ok,!cancel);
 assert(!cursor._batch);
}
const originalWindowLookup=cursor._windowById;
let targetFocused=true;
cursor._windowById=()=>({win:{focus(){},raise(){},has_focus:()=>targetFocused,get_buffer_rect:()=>({x:0,y:0})}});
const focusLost=replies();typed=[];
cursor.WindowBatchAsync([42,JSON.stringify([{type:'wait',ms:10},{type:'type',text:'must not escape'}])],focusLost);
fire(80);targetFocused=false;fire(10);
assert.deepEqual(typed,[]);
assert.equal(JSON.parse(focusLost.events[0][1][0]).ok,false);
assert.match(JSON.stringify(focusLost.events),/lost keyboard focus/);
assert(!cursor._batch);
cursor._windowById=originalWindowLookup;
console.log('CURSOR_FOCUS_LOSS_OK: delayed actions cancel instead of reaching another window');
const focusHandlers=new Map();let focusHandlerId=0;
fakeGlobal.display.connect=(_signal,handler)=>{focusHandlers.set(++focusHandlerId,handler);return focusHandlerId;};
fakeGlobal.display.disconnect=id=>{assert(focusHandlers.delete(id));};
for(const signalCancel of [false,true]) {
 const observed=replies();typed=[];
 cursor.WindowBatchAsync([42,JSON.stringify([{type:'wait',ms:10},{type:'type',text:'only while focused'}])],observed);
 fire(80);assert.equal(focusHandlers.size,1);
 const lateWait=[...activeTimers.values()].find(timer=>timer.ms===10).fn;
 if(signalCancel){[...focusHandlers.values()][0]();lateWait();assert.deepEqual(typed,[]);}
 else {fire(10);fire(60);assert.deepEqual(typed,['only while focused']);}
 assert.equal(focusHandlers.size,0,'focus watcher disconnected on every completion path');
 assert.equal(observed.events.length,1);
 assert.equal(JSON.parse(observed.events[0][1][0]).ok,!signalCancel);
 assert(!cursor._batch);
}
for(const trailingWait of [false,true]) {
 const transitioned=replies();typed=[];
 const actions=[{type:'type',text:'delivered before transition'}];
 if(trailingWait)actions.push({type:'wait',ms:1200});
 cursor.WindowBatchAsync([42,JSON.stringify(actions)],transitioned);
 fire(80);
 if(trailingWait)fire(60);
 const stale=[...activeTimers.values()].filter(timer=>timer.ms===(trailingWait?1200:60)).map(timer=>timer.fn);
 [...focusHandlers.values()][0]();
 for(const callback of stale)callback();
 const receipt=JSON.parse(transitioned.events[0][1][0]);
 assert.equal(receipt.ok,true);
 assert.equal(receipt.completed_actions,1);
 assert.equal(receipt.focus_changed,true);
 assert.equal(receipt.shortened_waits,trailingWait?1:0);
 assert.equal(transitioned.events.length,1);
 assert.deepEqual(typed,['delivered before transition']);
 assert.equal(focusHandlers.size,0);
 assert(!cursor._batch);
}
const explicitCancel=replies();
cursor.WindowBatchAsync([42,JSON.stringify([{type:'type',text:'last input'}])],explicitCancel);
fire(80);cursor.Hide();
assert.equal(JSON.parse(explicitCancel.events[0][1][0]).ok,false,'explicit cancellation must never become a successful focus transition');
delete fakeGlobal.display.connect;delete fakeGlobal.display.disconnect;
console.log('CURSOR_TERMINAL_TRANSITION_OK: completed input succeeds without replay; shortened waits are explicit and cancellation stays strict');
console.log('CURSOR_FOCUS_SIGNAL_OK: immediate cancellation clears timer and watcher; stale wait cannot type');
cursor._userIdleMs=()=>0;
const beforeFocus=replies(),focusBefore=focusCalls;
cursor.WindowBatchAsync([42,JSON.stringify([{type:'type',text:'must not type'}])],beforeFocus);
const stalePoll=[...activeTimers.values()].find(t=>t.ms===150).fn;
cursor.Hide();stalePoll();
assert.equal(focusCalls,focusBefore);
assert.equal(beforeFocus.events.length,1);
assert(!cursor._batch);
console.log('CURSOR_BATCH_OK: exclusive input ownership, normal completion, wait cancellation and pre-focus cancellation fence stale callbacks');
for(const capture of [false,true]){
 const busyUser=replies(),before=focusCalls;
 if(capture) cursor.CaptureWindowAsync([42,'unused-no-file-written.png'],busyUser);
 else cursor.WindowBatchAsync([42,JSON.stringify([{type:'type',text:'must not interrupt'}])],busyUser);
 now+=5100;fire(150);
 assert.equal(focusCalls,before);
 assert.equal(busyUser.events.length,1);
 const result=JSON.parse(busyUser.events[0][1][0]);
 assert.equal(result.ok,false);
 assert.match(JSON.stringify(result),/Desktop busy/);
 assert(!cursor._batch);
}
delete cursor._userIdleMs;
assert.equal(cursor._userIdleMs(),0,'unavailable idle monitor must not count as idle');
console.log('CURSOR_USER_PRESENCE_OK: sustained typing blocks batch/capture after deadline; unknown presence fails closed');
cursor._userIdleMs=()=>1000;
const captureWindow={get_buffer_rect:()=>({x:0,y:0,width:100,height:80}),
 get_frame_rect:()=>({x:0,y:0,width:100,height:80}),get_title:()=> 'fixture',get_wm_class:()=> 'fixture'};
cursor._windowById=()=>({win:captureWindow});
for(const mode of ['success','before-paint','in-flight','timeout','wrong-focus']) {
 const result=replies(), startsBefore=captureStarts, opensBefore=openedStreams, closesBefore=closedStreams;
 const publishedBefore=publishedCaptures, discardedBefore=discardedCaptures;
 cursor.CaptureWindowAsync([42,'no-real-file.png'],result);
 const paint=[...activeTimers.values()].find(t=>t.ms===250).fn;
 const competing=replies();cursor.ClickAsync(['left'],competing);
 assert.equal(competing.events[0][0],'dev.phoenix.Cursor.Busy');
 if(mode==='before-paint') {
  cursor.Hide();paint();
  assert.equal(captureStarts,startsBefore);assert.equal(openedStreams,opensBefore);
 } else {
  if(mode==='wrong-focus')fakeGlobal.display.focus_window=null;
  fire(250);
  if(mode!=='wrong-focus') {
   assert.equal(captureStarts,startsBefore+1);
   if(mode==='in-flight')cursor.Hide();
   if(mode==='timeout')fire(10000);
   if(mode==='in-flight'||mode==='timeout') {
    assert.equal(result.events.length,1);assert(cursor._batch,'native capture retains ownership while draining');
    const rival=replies();cursor.MoveToAsync([1,1,10],rival);
    assert.equal(rival.events[0][0],'dev.phoenix.Cursor.Busy');
   }
   captureCallback(null,{});
   assert.equal(closedStreams,closesBefore+1);
  }
 }
 assert.equal(result.events.length,1,'capture replies exactly once');
 assert.equal(JSON.parse(result.events[0][1][0]).ok,mode==='success');
 assert.equal(publishedCaptures-publishedBefore,mode==='success'?1:0);
 assert.equal(discardedCaptures-discardedBefore,(mode==='in-flight'||mode==='timeout')?1:0);
 assert(!cursor._batch,'capture ownership released after pre-start cancellation or native drain');
 assert(![...activeTimers.values()].some(t=>t.ms===250||t.ms===10000));
}
console.log('CURSOR_CAPTURE_OWNERSHIP_OK: paint cancellation, native drain quarantine, timeout, focus validation, success and stream cleanup');
const showCursor=cursor._showCursor;
cursor._showCursor=()=>{};
const ctrl=0xffe3,shift=0xffe1,key=97;
const normalKeys=[[ctrl,1],[shift,1],[key,1],[key,0],[shift,0],[ctrl,0]];
for(const failAt of [-1,0,1,2,3,4,5]) {
 const emitted=[];
 cursor._vkeyboard={notify_keyval:(_,code,state)=>{
  emitted.push([code,state]);if(emitted.length-1===failAt)throw new Error('native keyboard failure');
 }};
 if(failAt===-1)cursor._directKey('ctrl+shift+a');
 else assert.throws(()=>cursor._directKey('ctrl+shift+a'),/native keyboard failure/);
 const pressed=emitted.filter(([,state])=>state===1).map(([code])=>code);
 assert.deepEqual(emitted.filter(([,state])=>state===0).map(([code])=>code),pressed.reverse(),'every attempted press gets a release attempt, even after cleanup failure');
 if(failAt===-1)assert.deepEqual(emitted,normalKeys);
}
const failedKeys=[];
cursor._vkeyboard={notify_keyval:(_,code,state)=>{
 failedKeys.push([code,state]);if(code===key&&state===1)throw new Error('original press failure');
 if(state===0)throw new Error('cleanup failure');
}};
assert.throws(()=>cursor._directKey('ctrl+shift+a'),/original press failure/);
assert.deepEqual(failedKeys,normalKeys,'all releases attempted despite multiple errors');
const committedText=[];
const keyEventsBeforeUnicode=failedKeys.length;
fakeMain.inputMethod={currentFocus:{},commit:text=>committedText.push(text)};
Cursor.prototype._typeText.call(cursor,'Café — 日本語 😀');
assert.deepEqual(committedText,['Café — 日本語 😀']);
assert.equal(failedKeys.length,keyEventsBeforeUnicode,'Unicode commits do not generate unmapped raw keyvals');
fakeMain.inputMethod.currentFocus=null;
assert.throws(()=>Cursor.prototype._typeText.call(cursor,'ASCII prefix 日本語'),/requires a focused text-input target/);
assert.equal(failedKeys.length,keyEventsBeforeUnicode,'unsupported target must not enter an ASCII prefix before failing');
assert.equal(committedText.length,1);
delete fakeMain.inputMethod;
console.log('CURSOR_UNICODE_OK: exact text-input commit; unsupported target fails without partial input');
cursor._showCursor=showCursor;
console.log('CURSOR_KEY_RELEASE_OK: press/cleanup failures unwind all attempted keys and preserve the original error');
const shutdown=replies();
cursor.MoveToAsync([10,10,100],shutdown);
obsolete=pending.onComplete;
cursor.disable();obsolete();
assert.equal(shutdown.events.length,1);
assert.equal(shutdown.events[0][0],'dev.phoenix.Cursor.Canceled');
assert.equal(activeTimers.size,0);
console.log('CURSOR_CANCELLATION_OK: busy admission, hide/disable replies, stale completion fencing, drag release and timer cleanup');
console.log('CURSOR_CHECK_OK: geometry bounds, exact hotspot, arrival ordering, explicit duration, zero movement trail timers');
console.log('1500px automatic move: old schedule 900ms; new schedule 166ms (not an end-to-end latency measurement).');
