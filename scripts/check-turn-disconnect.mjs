import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const source=readFileSync(new URL('../canvas-app/ui/conversation.js',import.meta.url),'utf8');
const start=source.indexOf('    socket.onclose=()=>{\n      if(state.turnSocket!==socket');
assert.ok(start>=0);
const handler=source.slice(start,source.indexOf('\n    };',start)+7);
function run({current=true,selected=true}={}){
 const socket={},events=[],finishes=[],token={};
 const state={turnSocket:current?socket:null};
 vm.runInNewContext(handler,{socket,state,turnToken:token,request:{turnId:'owned-turn'},selectionIsCurrent:()=>selected,targetAgent:()=>null,renderStory:event=>events.push(event),finishTurn:(...args)=>{finishes.push(args);state.turnSocket=null;}});
 socket.onclose();socket.onclose();
 return {events,finishes,socket,token};
}
const lost=run();
assert.equal(lost.events.length,1,'one interruption receipt, even if close is delivered twice');
assert.equal(lost.events[0].turn_id,'owned-turn');
assert.match(lost.events[0].text,/before completion was confirmed/);
assert.equal(lost.finishes.length,1);
assert.equal(lost.finishes[0][1],true,'retain unfinished work');
assert.equal(run({current:false}).events.length,0,'normal completion or deliberate stop must not show an interruption');
assert.equal(run({selected:false}).events.length,0,'late close cannot alter another conversation');
console.log('PASS: unexpected disconnect is visible once; completed/stopped/stale turns remain quiet');
