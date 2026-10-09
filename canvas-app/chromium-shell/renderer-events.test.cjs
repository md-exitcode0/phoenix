"use strict";
const test = require("node:test"), assert = require("node:assert/strict");
const { send } = require("./renderer-events.cjs");
function fixture() {
  const contents = { destroyed:false, crashed:false, loading:false, pid:42, messages:[],
    isDestroyed(){return this.destroyed}, isCrashed(){return this.crashed},
    isLoadingMainFrame(){return this.loading}, getOSProcessId(){return this.pid},
    send(...args){this.messages.push(args)} };
  return contents;
}
test("healthy interface receives the exact event once", () => {
  const c=fixture(), payload={instance:"agent-school_coach",targetId:"owned-tab"};
  assert.equal(send(c,"browser-location",payload),true);
  assert.deepEqual(c.messages,[["phoenix:event","browser-location",payload]]);
});
test("crash, missing process, destroyed window and navigation cannot receive events", () => {
  assert.equal(send(null,"update",{}),false);
  for(const [key,value] of [["destroyed",true],["crashed",true],["loading",true],["pid",0]]) {
    const c=fixture();c[key]=value;
    for(let n=0;n<100;n++)assert.equal(send(c,"update",{n}),false);
    assert.deepEqual(c.messages,[]);
  }
});
test("a frame disposed between check and send is harmless", () => {
  const c=fixture();c.send=()=>{throw Error("Render frame was disposed before WebFrameMain could be accessed")};
  assert.equal(send(c,"browser-location",{}),false);
});
test("disposal during a health check is harmless too", () => {
  const c=fixture();c.isLoadingMainFrame=()=>{throw Error("Object has been destroyed")};
  assert.equal(send(c,"browser-location",{}),false);
});
test("recovered interface receives new events without replaying stale ones", () => {
  const c=fixture();c.crashed=true;send(c,"old",{});c.crashed=false;c.loading=true;send(c,"during-load",{});
  c.loading=false;c.pid=43;assert.equal(send(c,"new",{}),true);
  assert.deepEqual(c.messages,[["phoenix:event","new",{}]]);
});
test("unrelated send failures stay observable", () => {
  const c=fixture();c.send=()=>{throw Error("Invalid event payload")};
  assert.throws(()=>send(c,"update",{}),/Invalid event payload/);
});
