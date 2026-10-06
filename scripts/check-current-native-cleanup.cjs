'use strict';
// Execute the captured review launcher's actual module with inert CDP/HTTP.
// No connection to Phoenix, evaluation in a live page, IPC, files or models.
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const {EventEmitter}=require('node:events');
const crypto=require('node:crypto');
const file=process.argv[2]||path.join(__dirname,'../review-shell/launcher/current-native.cjs');
const source=fs.readFileSync(file,'utf8');
let passed=0;
function check(name,value){assert.equal(value,true,name);passed++;}
function fixture(mode='success'){
  const sockets=[],timers=new Map(),events=[];let nextTimer=1;
  const dirname=path.join('/tmp/phoenix-native-fixture','artifacts/a/b/c/launcher');
  const root=path.resolve(dirname,'../../../../..');
  const expected='file://'+path.join(root,'phoenix_agent/canvas-app/ui',mode==='services'?'native-services.html':'index.html').split(path.sep).map(encodeURIComponent).join('/');
  class FakeSocket extends EventEmitter{
    constructor(){super();this.sent=[];this.closeCount=0;sockets.push(this);queueMicrotask(()=>this.emit('open'));}
    send(raw){
      const message=JSON.parse(raw);this.sent.push(message);
      if(mode==='sendFailure'||this.throwSend)throw new Error('Fixture send failed');
      const health=message.method==='Runtime.evaluate'&&message.params.expression.includes('gateway_status');
      const subscriptions=message.method==='Runtime.evaluate'&&message.params.expression.startsWith('(async()=>');
      if(health&&mode==='healthTimeout')return;
      let response;
      if(health&&mode==='healthException')response={result:{exceptionDetails:{text:'Fixture health failed'}}};
      else if(message.method==='Runtime.addBinding'&&mode==='bindingError')response={error:{message:'Fixture binding failed'}};
      else if(subscriptions&&mode==='subscriptionsError')response={result:{exceptionDetails:{text:'Fixture subscription failed'}}};
      else response={result:message.method==='Runtime.evaluate'?{result:{value:health?{running:mode!=='notRunning',pid:1,ws_port:0}:true}}:{}};
      queueMicrotask(()=>this.emit('message',JSON.stringify({id:message.id,...response})));
    }
    close(){this.closeCount++;this.emit('close');}
  }
  const http={get(_url,onResponse){
    const request=new EventEmitter();request.setTimeout=()=>request;request.destroy=error=>request.emit('error',error);
    queueMicrotask(()=>{
      const response=new EventEmitter();onResponse(response);
      response.emit('data',JSON.stringify([{type:'page',url:expected+(mode==='foreign'?'.sibling':''),id:'fixture-page',webSocketDebuggerUrl:'ws://127.0.0.1:17442/devtools/page/fixture'}]));
      response.emit('end');
    });return request;
  }};
  const context=vm.createContext({module:{exports:{}},__dirname:dirname,
    require(name){if(name==='node:http')return http;if(name==='node:path')return path;if(name.endsWith('/ws'))return {WebSocket:FakeSocket};throw Error('Unexpected dependency: '+name);},
    setTimeout:(run,delay)=>{const id=nextTimer++;timers.set(id,{run,delay});return id;},clearTimeout:id=>timers.delete(id),
  });
  vm.runInContext(source,context);
  return {sockets,timers,events,start:()=>context.module.exports((name,payload)=>events.push({name,payload}))};
}
const settle=()=>new Promise(resolve=>setImmediate(resolve));
(async()=>{
  for(const mode of ['healthTimeout','healthException','notRunning','bindingError','subscriptionsError','sendFailure']){
    const f=fixture(mode),observed=f.start().then(value=>({value}),error=>({error:error.message}));
    await settle();
    if(mode==='healthTimeout'){
      check('startup timeout has exactly one bounded request',f.timers.size===1&&[...f.timers.values()][0].delay===20000);
      const [id,timer]=[...f.timers][0];f.timers.delete(id);timer.run();await settle();
    }
    check(mode+' closes the failed initialization connection',f.sockets[0].closeCount===1);
    check(mode+' rejects without retaining request timers',!!(await observed).error&&f.timers.size===0);
    if(mode==='healthTimeout')check('timeout acknowledges uncertain completion without retry',f.sockets[0].sent.length===1&&/unconfirmed; no retry/.test((await observed).error));
  }
  const service=fixture('services'),serviceClient=await service.start();
  check('exact native-services target initializes without a conversation page',serviceClient.health.running&&serviceClient.identity.targetId==='fixture-page');
  await serviceClient.close();
  const foreign=fixture('foreign'),foreignError=await foreign.start().then(()=>null,error=>error.message);
  check('similarly named native target is refused before connecting',/UNAVAILABLE/.test(foreignError)&&foreign.sockets.length===0);
  const f=fixture(),client=await f.start(),socket=f.sockets[0];
  check('healthy initialization keeps one live connection',f.sockets.length===1&&socket.closeCount===0&&f.timers.size===0&&client.identity.targetId==='fixture-page');
  socket.emit('message',JSON.stringify({method:'Runtime.bindingCalled',params:{name:'__phoenixMonoCodeEventV4',payload:JSON.stringify({name:'term-data',payload:{data:'fixture event'}})}}));
  check('healthy event forwarding remains usable',f.events.length===1&&f.events[0].payload.data==='fixture event');
  socket.throwSend=true;const failed=await client.invoke('session_context_get').then(()=>null,error=>error.message);
  check('later synchronous send failure removes its timer',failed==='Fixture send failed'&&f.timers.size===0&&socket.closeCount===0);
  socket.throwSend=false;await client.close();
  check('explicit close cleans subscriptions before closing once',socket.closeCount===1&&socket.sent.at(-1).params.expression.includes('splice(0)')&&f.timers.size===0);
  const sent=socket.sent.length,afterClose=await client.invoke('session_context_get').then(()=>null,error=>error.message);
  check('closed connection cannot send another native command',/DISCONNECTED/.test(afterClose)&&socket.sent.length===sent);
  console.log(JSON.stringify({passed,source_sha256:crypto.createHash('sha256').update(source).digest('hex'),scope:'Actual review launcher executed with inert HTTP/CDP; no running-app or model acceptance.'}));
})().catch(error=>{console.error(error.message);process.exitCode=1});
