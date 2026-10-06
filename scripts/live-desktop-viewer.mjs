// Real gateway + Sol-medium screenshot, with a read-only local viewer adapter.
import http from 'node:http';
import {mkdir,readFile,writeFile,open} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {messages,once,withAcceptanceGateway,sleep} from './lib/acceptance-gateway.mjs';
const [binary,output]=process.argv.slice(2);
if(!binary?.startsWith('/')||!output?.startsWith('/'))throw Error('Absolute binary and new evidence directory required');
await mkdir(output,{mode:0o700});
const ui=resolve(fileURLToPath(new URL('../canvas-app/ui/',import.meta.url)));
await withAcceptanceGateway({binary,output,workspace:output,setup:async({env})=>{env.PHOENIX_DESKTOP_BACKEND='gnome';}},async({socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const directory=await once(socketPath,{CompanyDirectory:{action:'status'}});
  const actor=directory.CompanyDirectory.directory.agents.find(a=>a.internal_role==='coder');
  const log=await open(join(output,'events.jsonl'),'wx',0o600);
  let terminal;
  try{
    for await(const value of messages(socketPath,{Turn:{session_id:actor.canonical_session_id,
      turn_id:'viewer-smoke-'+Date.now(),user_request:'Read-only desktop viewer acceptance. Call computer_screenshot exactly once, inspect the returned image, then final_answer with one sentence describing the visible desktop. Use no other tools. Do not modify files or contact other agents.',
      workspace:output,interaction_mode:'execute',permission_mode:'full_access',target_agent:actor.agent_id,journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify(value)+'\n');
      if(value.Done||value.Error){terminal=value;break;}
    }
  }finally{await log.close();}
  if(!terminal||terminal.Error)throw Error('Screenshot task did not complete: '+JSON.stringify(terminal));
  const views=(await once(socketPath,'DesktopWorkspaces')).DesktopWorkspaces;
  if(views.length!==1||views[0].backend!=='gnome')throw Error('Expected one actual native workspace');
  const frame=(await once(socketPath,{DesktopObservation:{scope_key:views[0].scope_key}})).DesktopObservation;
  if(!frame.data_url?.startsWith('data:image/png;base64,'))throw Error('No actual screenshot delivered');
  const unchanged=(await once(socketPath,{DesktopObservation:{scope_key:views[0].scope_key,after_ms:frame.captured_at_ms}})).DesktopObservation;
  if(unchanged.data_url!==null)throw Error('Unchanged observation retransmitted pixels');
  await writeFile(join(output,'observation.png'),Buffer.from(frame.data_url.split(',')[1],'base64'),{flag:'wx'});
  await writeFile(join(output,'gateway-checks.json'),JSON.stringify({views,width:frame.width,height:frame.height,
    captured_at_ms:frame.captured_at_ms,unchangedOmitted:true,terminal},null,2),{flag:'wx'});
  const html=`<!doctype html><html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Live Phoenix desktop viewer</title><link rel="stylesheet" href="/desktop-viewer.css"><style>:root{--ink:#eee9e4;--ink-soft:#bbb3ab;--ink-faint:#a29b94;--surface:#252321;--bg:#1e1c1a;--line:#403c37;font:14px system-ui;background:var(--bg);color:var(--ink)}body{margin:24px}header{margin-bottom:16px}main{height:calc(100vh - 100px);border:1px solid var(--line);border-radius:12px;overflow:hidden}.desktop-workspace-toolbar{display:flex;align-items:center;height:42px}.inspection-desktop-panel{height:calc(100% - 42px)}</style><header>Live gateway connection · Latest agent observation</header><main><div class="desktop-workspace-toolbar"><select id="desktopWorkspacePicker" aria-label="Agent desktop"></select><span id="desktopWorkspaceStatus" class="desktop-workspace-status" role="status"></span></div><section class="inspection-desktop-panel"><div class="desktop-observation-stage"><img id="desktopObservationImage" alt="" hidden><p id="desktopObservationEmpty" class="desktop-observation-empty">Connecting…</p></div><footer id="desktopObservationCaption" class="desktop-observation-caption">Latest observation</footer></section></main><script src="/desktop-viewer.js"></script><script>window.PhoenixDesktopViewer.update({visible:true,context:'live-gateway',rpc:async(request,_timeout,signal)=>{const response=await fetch('/rpc',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(request),signal});const value=await response.json();if(value.Error)throw Error(value.Error.message);return value;}});</script></html>`;
  let requests=0;
  const server=http.createServer(async(req,res)=>{
    try{
      if(req.method==='GET'&&req.url==='/'){res.setHeader('Content-Type','text/html');res.end(html);return;}
      if(req.method==='GET'&&['/desktop-viewer.js','/desktop-viewer.css'].includes(req.url)){
        res.setHeader('Content-Type',req.url.endsWith('.js')?'text/javascript':'text/css');res.end(await readFile(join(ui,req.url.slice(1))));return;
      }
      if(req.method==='POST'&&req.url==='/rpc'){
        let body='';for await(const chunk of req){body+=chunk;if(body.length>4096)throw Error('Request too large');}
        const request=JSON.parse(body);
        if(request!=='DesktopWorkspaces'&&!(request?.DesktopObservation&&Object.keys(request).length===1))throw Error('Only observation reads are allowed');
        requests++;res.setHeader('Content-Type','application/json');res.end(JSON.stringify(await once(socketPath,request)));return;
      }
      res.writeHead(404);res.end();
    }catch(error){res.writeHead(400,{'Content-Type':'application/json'});res.end(JSON.stringify({Error:{message:String(error.message)}}));}
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const url='http://127.0.0.1:'+server.address().port+'/';
  await writeFile(join(output,'viewer.json'),JSON.stringify({url,scope:views[0].scope_key},null,2),{flag:'wx'});
  console.log(JSON.stringify({phase:'viewer-ready',url,output}));
  try{await sleep(120000);}finally{server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}
  await writeFile(join(output,'viewer-requests.json'),JSON.stringify({requests}),{flag:'wx'});
});
