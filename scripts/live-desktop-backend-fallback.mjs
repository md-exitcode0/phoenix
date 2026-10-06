// Force native startup to fail inside a disposable acceptance environment.
import {mkdir,writeFile,open} from 'node:fs/promises';
import {join} from 'node:path';
import {messages,once,withAcceptanceGateway} from './lib/acceptance-gateway.mjs';
const [binary,output]=process.argv.slice(2);
if(!binary?.startsWith('/')||!output?.startsWith('/'))throw Error('Absolute binary and new output required');
await mkdir(output,{mode:0o700});
await withAcceptanceGateway({binary,output,workspace:output,setup:async({home,env})=>{
  const bin=join(home,'failing-native-bin');await mkdir(bin);
  await writeFile(join(bin,'gnome-shell'),'#!/bin/sh\nexit 73\n',{mode:0o700,flag:'wx'});
  env.PATH=bin+':'+env.PATH;
  delete env.PHOENIX_DESKTOP_BACKEND;
}},async({socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const actor=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory.agents.find(a=>a.internal_role==='coder');
  const log=await open(join(output,'events.jsonl'),'wx',0o600);let terminal;
  const started=Date.now();
  const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:actor.canonical_session_id}}).catch(()=>{}),5*60*1000);
  try{
    for await(const value of messages(socketPath,{Turn:{session_id:actor.canonical_session_id,turn_id:'backend-fallback-'+Date.now(),
      user_request:'Read-only desktop acceptance: call computer_screenshot exactly once, inspect it and finish with one sentence describing the visible desktop. Use no other tools and modify nothing.',
      workspace:output,target_agent:actor.agent_id,interaction_mode:'execute',permission_mode:'full_access',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify(value)+'\n');if(value.Done||value.Error){terminal=value;break;}
    }
  }finally{clearTimeout(timer);await log.close();}
  const views=(await once(socketPath,'DesktopWorkspaces')).DesktopWorkspaces;
  if(views.length!==1)throw Error('Expected one fallback desktop');
  const frame=(await once(socketPath,{DesktopObservation:{scope_key:views[0].scope_key}})).DesktopObservation;
  const checks={completed:!!terminal?.Done,x11Fallback:views[0].backend==='x11',actualImage:frame.data_url?.startsWith('data:image/png;base64,')===true};
  await writeFile(join(output,'result.json'),JSON.stringify({checks,elapsed_ms:Date.now()-started,views,terminal},null,2),{flag:'wx'});
  console.log(JSON.stringify(checks));if(!Object.values(checks).every(Boolean))process.exitCode=1;
});
