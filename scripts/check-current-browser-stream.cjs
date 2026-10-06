'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const {EventEmitter}=require('node:events');
const source=fs.readFileSync(path.join(__dirname,'../review-shell/launcher/current-browser-stream.cjs'),'utf8');
let passed=0;const check=(label,value)=>{assert.equal(value,true,label);passed++;};
function fixture(){
 const sockets=[],events=[],intervals=new Map();let alive=true,tab='owned-1',nextTimer=0,wait=null,viewport=null;
 class Socket extends EventEmitter{
  static OPEN=1;
  constructor(url){super();this.url=url;this.readyState=1;this.sent=[];this.closed=0;sockets.push(this);queueMicrotask(()=>this.emit('open'));}
  send(raw){const message=JSON.parse(raw);this.sent.push(message);const result=message.method==='Page.getLayoutMetrics'?{cssVisualViewport:{clientWidth:800,clientHeight:600}}:message.method==='Page.captureScreenshot'?{data:'fixture-initial-capture'}:{};queueMicrotask(()=>this.emit('message',JSON.stringify({id:message.id,result})));}
  close(){this.closed++;this.readyState=3;this.emit('close');}
  frame(data='fixture-jpeg'){this.emit('message',JSON.stringify({method:'Page.screencastFrame',params:{data,sessionId:7,metadata:{deviceWidth:800,deviceHeight:600}}}));}
 }
 const http={get(_url,receive){const req=new EventEmitter();req.setTimeout=()=>{};req.destroy=e=>req.emit('error',e);queueMicrotask(()=>{const res=new EventEmitter();receive(res);res.emit('data',JSON.stringify([{id:tab,type:'page',webSocketDebuggerUrl:'ws://127.0.0.1:17442/devtools/page/'+tab}]));res.emit('end');});return req;}};
 const context=vm.createContext({module:{exports:{}},setTimeout,clearTimeout,setInterval:fn=>{const id=++nextTimer;intervals.set(id,fn);return id;},clearInterval:id=>intervals.delete(id),
  require(name){if(name==='node:http')return http;if(name==='node:path')return path;if(name.endsWith('/ws'))return {WebSocket:Socket};throw Error(name);},__dirname:'/fixture/launcher'});
 vm.runInContext(source,context);
 return {sockets,events,intervals,setTab(id){tab=id},setViewport(value){viewport=value},stop(){alive=false},delayStatus(){wait={};wait.promise=new Promise(r=>wait.resolve=r);return ()=>wait.resolve({tabs:[{id:tab,active:true,url:'https://example.invalid'}]});},
  start:()=>context.module.exports({instance:'agent-phoenix',getViewport:()=>viewport,native:{async invoke(command,args){assert.equal(command,'browser_surface_status');assert.equal(args.instance,'agent-phoenix');return wait?wait.promise:{tabs:[{id:tab,active:true,url:'https://example.invalid'}]};}},emit:(...event)=>events.push(event),isCurrent:()=>alive})};
}
(async()=>{
 const f=fixture(),controller=await f.start();
 check('one exact owned native tab opened',f.sockets.length===1&&f.sockets[0].url.endsWith('/owned-1'));
 check('only display/viewport commands sent',f.sockets[0].sent.every(m=>['Page.enable','Page.startScreencast','Emulation.clearDeviceMetricsOverride','Page.getLayoutMetrics','Page.captureScreenshot'].includes(m.method)));
 check('idle tab receives a genuine initial capture',JSON.parse(f.events.at(-1)[1]).BrowserFrame.data==='fixture-initial-capture');
 f.setViewport({width:700,height:240});await [...f.intervals.values()][0]();
 check('narrow pane resizes real page instead of shrinking a tall image',f.sockets[0].sent.at(-1).method==='Emulation.setDeviceMetricsOverride'&&f.sockets[0].sent.at(-1).params.height===240&&f.sockets.length===1);
 f.sockets[0].frame();const frame=JSON.parse(f.events.at(-1)[1]).BrowserFrame;
 check('real frame retains original owner',frame.instance==='agent-phoenix'&&frame.data==='fixture-jpeg'&&frame.w===800);
 f.setTab('owned-2');await [...f.intervals.values()][0]();
 check('tab switch retires old display once',f.sockets[0].closed===1&&f.sockets[0].sent.at(-1).method==='Page.stopScreencast');
 check('replacement uses exact new target',f.sockets.length===2&&f.sockets[1].url.endsWith('/owned-2'));
 const count=f.events.length;f.sockets[0].frame();check('late old-tab frames ignored',f.events.length===count);
 controller.close();controller.close();check('teardown stops once and removes timer',f.sockets[1].closed===1&&f.intervals.size===0);
 f.sockets[1].frame();check('closed lane cannot publish',f.events.length===count);
 const delayed=fixture(),resume=delayed.delayStatus(),starting=delayed.start();delayed.stop();resume();const retired=await starting;
 check('delayed status after conversation switch opens nothing',delayed.sockets.length===0&&delayed.events.length===0&&delayed.intervals.size===0);retired.close();
 console.log(JSON.stringify({passed,scope:'Actual stream module with inert native/CDP; no user profile, network, cookies or model calls.'}));
})().catch(error=>{console.error(error.stack);process.exitCode=1});
