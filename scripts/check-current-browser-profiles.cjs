'use strict';
// Run the installed adapter with inert IPC/native/gateway boundaries. No
// browser, profile, authentication, user data or provider request is opened.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const {EventEmitter}=require('node:events');
const root=path.join(__dirname,'..'),source=fs.readFileSync(path.join(root,'review-shell/launcher/backend-current.cjs'),'utf8');
const handlers=new Map(),requests=[],nativeCalls=[],agents=['phoenix','frontend'].map(agent_id=>({agent_id,browser_profile_id:`agent-${agent_id}`,lifecycle:'active',kind:'responsibility_owner'}));
const net={createConnection(){const socket=new EventEmitter();socket.setTimeout=()=>{};socket.destroy=()=>{};socket.write=raw=>{
 const request=JSON.parse(raw);requests.push(request);
 const kind=typeof request==='string'?request:Object.keys(request)[0],body=request[kind];
 const reply=kind==='CompanyDirectory'?{CompanyDirectory:{directory:{agents,groups:[]}}}:kind==='BrowserSurface'?{BrowserSurface:{instance:body.instance,supported:true,attached:body.action==='open',tabs:[{id:body.instance+'-tab',active:true,url:'https://example.invalid'}]}}:{BrowserInteraction:{instance:body.instance,ok:true}};
 queueMicrotask(()=>socket.emit('data',JSON.stringify(reply)+'\n'));
 };queueMicrotask(()=>socket.emit('connect'));return socket;}};
const window={webContents:{id:1,on(){},isDestroyed(){return false;},send(){}}},diagnostics={rendererErrors:[]};
const context=vm.createContext({module:{exports:{}},process:{getuid:()=>1000},setTimeout,clearTimeout,Buffer,
 require(name){
  if(name==='node:fs')return {lstatSync:()=>({isSocket:()=>true,uid:1000})};
  if(name==='node:net')return net;
  if(name==='./current-native.cjs')return async()=>({identity:{fixture:true},health:{pid:1},async invoke(command,args){nativeCalls.push({command,args});if(command==='browser_surface_set_bounds')return {supported:true};if(command==='channels_command'&&args.action==='list')return {connections:[{config:{id:'saved',enabled:true},status:{saved_login:true}},{config:{id:'running',enabled:true},status:{saved_login:true,running:true}},{config:{id:'disabled',enabled:false},status:{saved_login:true}},{config:{id:'unsigned',enabled:true},status:{saved_login:false}}]};if(command==='channels_command'&&args.action==='start')return {ok:true};throw Error('Unexpected native call');},async close(){}});
  if(name==='./turn-receipts.cjs')return ()=>({});
  if(name.startsWith('./'))return require(path.join(root,'review-shell/launcher',name));
  return require(name);
 }});
vm.runInContext(source,context);
let passed=0;const check=(label,value)=>{assert.equal(value,true,label);passed++;};
(async()=>{
 const adapter=await context.module.exports({ipcMain:{handle:(name,fn)=>handlers.set(name,fn),on(){}},config:{profilePath:'/fixture'},appURL:()=>true,diagnostics,persistDiagnostics(){},getWindow:()=>window});
 const event={sender:window.webContents,senderFrame:{url:'phoenix-review://monocode/'}};
 for(const instance of ['agent-phoenix','agent-frontend']){
  for(const action of ['open','status','close']){
   const reply=await adapter.rpc({BrowserSurface:{instance,action}});
   check(`${instance} ${action} reaches canonical gateway`,reply.BrowserSurface.instance===instance&&requests.at(-1).BrowserSurface?.action===action);
  }
  const reply=await adapter.rpc({BrowserInteract:{instance,browser_action:{action:'navigate',url:'https://example.invalid'}}});
  check(`${instance} navigation keeps original profile`,reply.BrowserInteraction.instance===instance&&requests.at(-1).BrowserInteract?.instance===instance);
  const status=await handlers.get('phoenix-isolated-invoke')(event,'browser_surface_status',{instance});
  check(`${instance} native status reads the same profile`,status.instance===instance&&status.tabs[0].id===instance+'-tab');
  const attached=await handlers.get('phoenix-isolated-invoke')(event,'browser_surface_attach',{instance,rect:{x:0,y:0,width:100,height:100}});
  check(`${instance} uses shared frame fallback`,attached.supported===false&&attached.embedded===false);
  await adapter.rpc({BrowserInteract:{instance,browser_action:{action:'resize',width:700,height:900}}});
  check(`${instance} physical private view follows pane size`,nativeCalls.at(-1).args.instance===instance&&nativeCalls.at(-1).args.rect.height===900);
 }
 const invoke=(args)=>handlers.get('phoenix-isolated-invoke')(event,'channels_command',args);
 check('existing saved channel may reconnect',!!(await invoke({action:'start',id:'saved'})).ok&&nativeCalls.at(-1).args.id==='saved');
 check('actual UI null optional fields do not block saved reconnect',!!(await invoke({action:'start',id:'saved',config:null,token:null,value:null,kind:null,offset:null,limit:null,snapshot:null})).ok&&Object.keys(nativeCalls.at(-1).args).length===2);
 const before=nativeCalls.filter(c=>c.args.action==='start').length;
 check('already running channel is not restarted',!!(await invoke({action:'start',id:'running'})).already_running&&nativeCalls.filter(c=>c.args.action==='start').length===before);
 for(const args of [{action:'start',id:'unknown'},{action:'start',id:'disabled'},{action:'start',id:'unsigned'},{action:'start',id:'saved',token:'fixture-secret'},{action:'start',id:'saved',config:{enabled:true}}]){await assert.rejects(()=>invoke(args));passed++;}
 await assert.rejects(()=>adapter.rpc({BrowserSurface:{instance:'agent-unknown',action:'open'}}));passed++;
 await assert.rejects(()=>handlers.get('phoenix-isolated-invoke')(event,'browser_surface_attach',{instance:'agent-unknown'}));passed++;
 await adapter.close();
 const uiSource=fs.readFileSync(path.join(root,'review-shell/ui/conversation.js'),'utf8');
 const transport=uiSource.slice(uiSource.indexOf('  function queueBrowserTransport('),uiSource.indexOf('  function browserSurfaceRect('));
 const calls=[],state={browserOwnerId:'agent-phoenix',browserBoundKey:'phoenix',browserSurfaceGeneration:1,item:{id:'phoenix'},browserActionQueue:Promise.resolve()};
 const uiContext=vm.createContext({state,window:{PhoenixIsolatedBackend:{current:true}},ui:{async invoke(command,args){calls.push({command,args});return {ok:true};}},conversationKeyOf:item=>item.id,browserControlRpc(){throw Error('Current transport must use its typed native boundary');}});
 vm.runInContext(transport,uiContext);
 await vm.runInContext('queueBrowserTransport({action:"resize",width:705,height:240})',uiContext);
 check('actual UI narrow resize reaches typed current browser boundary',calls.length===1&&calls[0].args.request.BrowserInteract.instance==='agent-phoenix'&&calls[0].args.request.BrowserInteract.browser_action.height===240);
 const delayed=vm.runInContext('queueBrowserTransport({action:"resize",width:705,height:240})',uiContext);state.browserOwnerId='agent-frontend';state.item={id:'frontend'};state.browserBoundKey='frontend';state.browserSurfaceGeneration++;
 await assert.rejects(delayed,/superseded/);check('delayed UI resize cannot cross selection',calls.length===1);
 const main=fs.readFileSync(path.join(root,'review-shell/launcher/electron-main.cjs'),'utf8');
 check('current launch does not create an ephemeral profile',main.includes("if(!config.currentBackend)config.nativeBrowser=await require('./backend-browser.cjs')"));
 console.log(JSON.stringify({passed,scope:'Actual current adapter with inert gateway/IPC; current root and coworker profile ownership, preserved close contract, no private data/model calls.'}));
})().catch(error=>{console.error(error.stack);process.exitCode=1});
