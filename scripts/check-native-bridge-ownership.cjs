'use strict';
// Executes production request/connection functions with inert HTTP/WebSocket
// adapters. No native app, authentication token, profile, IPC or model call.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {EventEmitter} = require('node:events');
const crypto = require('node:crypto');
const source = fs.readFileSync(process.argv[2] || path.join(__dirname, '../canvas-app/chromium-shell/main.cjs'), 'utf8');
const relay = fs.readFileSync(process.argv[3] || path.join(__dirname, '../canvas-app/ui/relay.html'), 'utf8');
const invocation = source.slice(source.indexOf('function rejectBridgeRequests('), source.indexOf('function bridgeAuthorized('));
const server = source.slice(source.indexOf('function startBridgeServer('), source.indexOf('function createSurface('));
assert(invocation.includes('async function invokeRust') && server.includes('webSockets.on("connection"'), 'Production boundaries changed; inspect extraction');

let passed = 0;
function fixture() {
  const timers = new Map(), events = [];let nextTimer = 1, socketServer;
  class FakeSocket extends EventEmitter {
    constructor() { super(); this.readyState = 1; this.sent = []; }
    send(raw, callback) {
      if (this.throwSend) throw new Error('Send failed synchronously');
      if (this.callbackFailure) { callback?.(new Error('Send callback failed')); return; }
      this.sent.push(JSON.parse(raw));callback?.();
    }
    close() { this.readyState = 2; } // Deliberately delayed close notification.
    reply(value) { this.emit('message', JSON.stringify(value)); }
  }
  class FakeServer extends EventEmitter {
    constructor() { super(); socketServer = this; }
  }
  const context = vm.createContext({
    WebSocket:{OPEN:1},WebSocketServer:FakeServer,BRIDGE_TOKEN:'isolated-fixture-only',BRIDGE_PORT:17443,SELFTEST:false,
    http:{createServer:()=>({on(){},listen(_port,_host,done){done?.()}})},
    process:{versions:{chrome:'fixture'},stderr:{write(){}}},bootLog(){},
    emit:(name,payload)=>events.push({name,payload}),
    setTimeout:(fn)=>{const id=nextTimer++;timers.set(id,fn);return id},clearTimeout:id=>timers.delete(id),
  });
  vm.runInContext('let rustSocket=null,bridgeServer=null,nextBridgeRequest=1;const bridgeRequests=new Map();\n'+invocation+'\n'+server+'\nstartBridgeServer();globalThis.test={invokeRust,pending:()=>bridgeRequests.size,current:()=>rustSocket};',context);
  return {FakeSocket,events,timers,connect(socket){socketServer.emit('connection',socket)},...context.test};
}
async function begin(f, socket, command='session_context_get') {
  const promise = f.invokeRust(command,{sessionId:'isolated-session',owner:{kind:'agent',id:'phoenix'}});
  const observed = promise.then(value=>({value}),error=>({error:error.message}));
  await Promise.resolve();return {observed,id:socket.sent.findLast(m=>m.type==='invoke')?.id};
}
function check(name, value) { assert.equal(value,true,name);passed++; }

function relayFixture() {
  const sockets=[],timers=[],operations=[],listeners=new Map();
  class FakeSocket {
    static OPEN=1;
    constructor() { this.readyState=1;this.sent=[];sockets.push(this); }
    send(raw) { this.sent.push(JSON.parse(raw)); }
    receive(value) { return this.onmessage({data:JSON.stringify(value)}); }
    disconnect() { this.readyState=3;this.onclose(); }
  }
  const context=vm.createContext({
    URLSearchParams,WebSocket:FakeSocket,location:{search:'?port=17443&token=isolated-fixture-token-123456'},
    setTimeout:(run,delay)=>timers.push({run,delay}),
    window:{__TAURI__:{core:{invoke(command,args){
      return new Promise((resolve,reject)=>operations.push({command,args,resolve,reject}));
    }},event:{listen(name,callback){listeners.set(name,callback);return Promise.resolve(()=>listeners.delete(name));}}}},
  });
  const script=relay.match(/<script>([\s\S]*?)<\/script>/)?.[1];
  assert(script?.includes('window.__TAURI__.core.invoke'), 'Production relay boundary changed; inspect extraction');
  vm.runInContext(script,context);
  return {sockets,timers,operations,listeners,reconnect(){assert.equal(timers.length,1);timers.shift().run();return sockets.at(-1);}};
}

(async()=>{
  const f=fixture(),old=new f.FakeSocket(),fresh=new f.FakeSocket();f.connect(old);
  const first=await begin(f,old);f.connect(fresh);
  check('replacement retires the old request immediately',f.pending()===0);
  check('replacement rejects old receipt without replay', /unconfirmed/.test((await first.observed).error)&&fresh.sent.every(m=>m.type!=='invoke'));
  const second=await begin(f,fresh);old.emit('close');
  check('late old close retains fresh request',f.pending()===1&&f.current()===fresh);
  old.reply({type:'result',id:second.id,ok:true,value:'foreign'});
  check('old result cannot resolve fresh request',f.pending()===1);
  old.reply({type:'event',name:'term-data',payload:{id:1,data:'stale'}});
  check('old event ignored',f.events.length===0);
  fresh.reply({type:'event',name:'term-data',payload:{id:2,data:'current'}});
  check('current event retained',f.events.length===1&&f.events[0].payload.data==='current');
  fresh.reply({type:'result',id:second.id,ok:true,value:['current history']});
  check('fresh native reply resolves',JSON.stringify((await second.observed).value)==='["current history"]'&&f.pending()===0&&f.timers.size===0);
  const disconnected=await begin(f,fresh);fresh.emit('close');
  check('current disconnect rejects own pending work',/unconfirmed/.test((await disconnected.observed).error)&&f.pending()===0&&f.current()===null&&f.timers.size===0);
  for(const mode of ['throwSend','callbackFailure']){
    const a=fixture(),s=new a.FakeSocket();a.connect(s);s[mode]=true;const r=await begin(a,s);
    check(mode+' removes timer and request',!!(await r.observed).error&&a.pending()===0&&a.timers.size===0);
  }
  const a=fixture(),s=new a.FakeSocket();a.connect(s);const timed=await begin(a,s);
  const [timer,fire]=[...a.timers][0];a.timers.delete(timer);fire();s.reply({type:'result',id:timed.id,ok:true,value:'late'});
  check('late result cannot replace timeout',/timed out/.test((await timed.observed).error)&&a.pending()===0);
  const failed=await begin(a,s);s.reply({type:'result',id:failed.id,ok:false,error:'Native read refused'});
  check('native rejection preserved without resend',(await failed.observed).error==='Native read refused'&&s.sent.filter(m=>m.type==='invoke').length===2&&a.pending()===0);
  const relayTest=relayFixture(),oldRelay=relayTest.sockets[0];
  const oldResult=oldRelay.receive({type:'invoke',id:1,command:'session_context_get',args:{sessionId:'old'}});
  oldRelay.disconnect();const currentRelay=relayTest.reconnect();
  const currentResult=currentRelay.receive({type:'invoke',id:1,command:'session_context_get',args:{sessionId:'current'}});
  relayTest.operations[0].resolve('stale history');await oldResult;
  check('relay never forwards old result through replacement with reused ID',currentRelay.sent.length===0);
  relayTest.operations[1].resolve('current history');await currentResult;
  check('relay delivers current native reply once',currentRelay.sent.length===1&&currentRelay.sent[0].value==='current history');
  await oldRelay.receive({type:'invoke',id:2,command:'stale-command',args:{}});
  check('obsolete relay messages cannot invoke native commands',relayTest.operations.length===2);
  oldRelay.onclose();check('obsolete close cannot schedule another reconnect',relayTest.timers.length===0);
  relayTest.listeners.get('term-data')({payload:{data:'current event'}});
  check('relay sends accepted native events through current connection',currentRelay.sent.at(-1).type==='event'&&currentRelay.sent.at(-1).payload.data==='current event');
  await currentRelay.receive({type:'invoke',id:1.5,command:'invalid-id'});
  await currentRelay.onmessage({data:'invalid JSON'});
  check('relay rejects malformed invocation envelopes',relayTest.operations.length===2);
  const lateError=currentRelay.receive({type:'invoke',id:3,command:'session_context_get',args:{}});
  currentRelay.disconnect();const thirdRelay=relayTest.reconnect();
  relayTest.operations[2].reject(new Error('old native failure'));await lateError;
  check('relay never forwards old error through replacement',thirdRelay.sent.length===0);
  const nativeError=thirdRelay.receive({type:'invoke',id:4,command:'session_context_get',args:{}});
  relayTest.operations[3].reject(new Error('current native failure'));await nativeError;
  check('relay preserves current native failure without retry',thirdRelay.sent.length===1&&thirdRelay.sent[0].ok===false&&thirdRelay.sent[0].error==='current native failure'&&relayTest.operations.length===4);
  console.log(JSON.stringify({passed,source_sha256:crypto.createHash('sha256').update(source).digest('hex'),relay_sha256:crypto.createHash('sha256').update(relay).digest('hex'),scope:'Actual production functions and relay script executed with inert transport; no native IPC/app/model acceptance.'}));
})().catch(error=>{console.error(error.message);process.exitCode=1});
