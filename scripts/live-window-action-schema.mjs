// Model-generated native window batches, verified by the actual GTK app state.
import {mkdir,readFile,writeFile,open} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {spawn} from 'node:child_process';
import {messages,once,withAcceptanceGateway} from './lib/acceptance-gateway.mjs';
const [binary,output,closeMode]=process.argv.slice(2);
if(closeMode!==undefined&&closeMode!=='close')throw Error('Optional mode must be close');
if(!binary?.startsWith('/')||!output?.startsWith('/'))throw Error('Absolute binary and new evidence directory required');
await mkdir(output,{mode:0o700});
const fixture=resolve('scripts/cursor-native-input-fixture.mjs');
if(/[\r\n"`$\\]/.test(fixture))throw Error('Unsupported launcher path');
await withAcceptanceGateway({binary,output,workspace:output,setup:async({env})=>{
  delete env.PHOENIX_DESKTOP_BACKEND; // Exercise the production automatic default.
  const apps=join(env.XDG_DATA_HOME,'applications');await mkdir(apps,{recursive:true});
  await writeFile(join(apps,'phoenix-native-acceptance.desktop'),`[Desktop Entry]\nType=Application\nName=Phoenix Native Acceptance\nExec=gjs -m "${fixture}"\nTerminal=false\n`,{flag:'wx'});
}},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const actor=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory.agents.find(a=>a.internal_role==='coder');
  let request='Native window input acceptance. Open Phoenix Native Acceptance. Use computer_list_windows and computer_capture_window to identify and inspect its window. Then use computer_window_act with window-relative coordinates for all interaction: replace the text entry with exactly “Café Phoenix 42”, click Native click target exactly once, and scroll the document down by three wheel steps then up by one wheel step. Batch these known actions and capture the result. Use only computer_open, computer_list_windows, computer_capture_window, computer_window_act and final_answer. Do not use scripts, bash, accessibility, file tools, other agents, or extra applications. Finish with what you actually observed. The harness verifies the app state independently.';
  if(closeMode==='close')request+=' Finally close this test window using computer_window_act with Alt+F4 followed by a 1200ms wait and capture:true. Confirm it closed using computer_list_windows. Do not retry a closed target.';
  await writeFile(join(output,'request.txt'),request,{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx',0o600),stories=[],started=Date.now();let terminal;
  const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:actor.canonical_session_id}}).catch(()=>{}),5*60*1000);
  try{
    for await(const value of messages(socketPath,{Turn:{session_id:actor.canonical_session_id,turn_id:'window-batch-'+Date.now(),user_request:request,
      workspace:output,target_agent:actor.agent_id,interaction_mode:'execute',permission_mode:'full_access',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify(value)+'\n');
      if(value.Story){stories.push(value.Story);if(value.Story.kind==='tool')console.log(JSON.stringify(value.Story));}
      if(value.Done||value.Error){terminal=value;break;}
    }
  }finally{clearTimeout(timer);await log.close();}
  const views=(await once(socketPath,'DesktopWorkspaces')).DesktopWorkspaces;
  if(views.length!==1)throw Error('Expected exactly one scoped desktop');
  const state=JSON.parse(await readFile(join(home,'desktops',views[0].scope_key,'input-state.json'),'utf8'));
  const calls=stories.filter(s=>s.kind==='tool');
  const scroll=state.scrollEvents.reduce((sum,e)=>({dx:sum.dx+e.dx,dy:sum.dy+e.dy}),{dx:0,dy:0});
  const checks={nativeDefault:views[0].backend==='gnome',terminal:!!terminal?.Done,exactText:state.text==='Café Phoenix 42',oneClick:state.clicks===1,
    scrollDown:state.scrollEvents.some(e=>e.dy>0),scrollUp:state.scrollEvents.some(e=>e.dy<0),netScroll:scroll.dx===0&&scroll.dy===2,
    windowBatchUsed:calls.some(c=>c.tool==='computer_window_act'),noToolFailures:calls.every(c=>c.ok),
    onlyRequestedTools:calls.every(c=>['computer_open','computer_list_windows','computer_capture_window','computer_window_act','final_answer'].includes(c.tool))};
  if(closeMode==='close') {
    const descriptor=JSON.parse(await readFile(join(home,'desktops',views[0].scope_key,'runtime','phoenix-desktop.json'),'utf8'));
    if(descriptor.scope_key!==views[0].scope_key)throw Error('Native descriptor scope mismatch');
    const result=await new Promise((resolve,reject)=>{
      const child=spawn('gdbus',['call','--address',descriptor.session_bus,'--timeout','3','--dest','dev.phoenix.Cursor','--object-path','/dev/phoenix/Cursor','--method','dev.phoenix.Cursor.ListWindows'],{stdio:['ignore','pipe','pipe']});
      let stdout='',stderr='';child.stdout.on('data',chunk=>stdout+=chunk);child.stderr.on('data',chunk=>stderr+=chunk);
      child.once('error',reject);child.once('exit',code=>resolve({code,stdout,stderr}));
    });
    if(result.code!==0)throw Error('Independent window inspection failed: '+result.stderr);
    const listing=JSON.parse(result.stdout.slice(result.stdout.indexOf('{'),result.stdout.lastIndexOf('}')+1));
    checks.windowActuallyClosed=!listing.windows.some(window=>window.title==='Phoenix native input acceptance');
  }
  const frame=(await once(socketPath,{DesktopObservation:{scope_key:views[0].scope_key}})).DesktopObservation;
  if(frame.data_url)await writeFile(join(output,'final-window.png'),Buffer.from(frame.data_url.split(',')[1],'base64'),{flag:'wx'});
  await writeFile(join(output,'result.json'),JSON.stringify({checks,elapsed_ms:Date.now()-started,toolCalls:calls.length,state,terminal},null,2),{flag:'wx'});
  console.log(JSON.stringify({checks,toolCalls:calls.length}));
  if(!Object.values(checks).every(Boolean))process.exitCode=1;
});
