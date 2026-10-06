// Minimal real-provider check: synthetic text, no tool work or account mutation.
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {withAcceptanceGateway,messages,once} from './lib/acceptance-gateway.mjs';
import {acceptanceAuthOptions} from './lib/acceptance-account.mjs';
const [binary,output,...flags]=process.argv.slice(2);
if(!binary?.startsWith('/')||!output?.startsWith('/'))throw Error('Absolute binary and new output directory required');
const auth=acceptanceAuthOptions(flags);
await mkdir(output,{mode:0o700});
await withAcceptanceGateway({binary,output,workspace:output,...auth},async({socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const directory=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory;
  const actor=directory.agents.find(a=>a.internal_role==='coder');
  const started=Date.now();let terminal;let tools=0;let timedOut=false;
  try{
    for await(const event of messages(socketPath,{Turn:{session_id:actor.canonical_session_id,
      turn_id:'sol-auth-'+Date.now(),user_request:'Read-only synthetic authorization check. Do not call tools or access any resources. Reply with exactly PHOENIX_SOL_READY.',
      workspace:output,interaction_mode:'execute',permission_mode:'full_access',target_agent:actor.agent_id,journal:true,delivery:'queue'}},{signal:AbortSignal.timeout(90_000)})){
      if(event.Story?.kind==='tool')tools++;
      if(event.Done||event.Error){terminal=event;break;}
    }
  }catch(error){
    if(error.name!=='TimeoutError')throw error;
    timedOut=true;
    await once(socketPath,{Cancel:{session_id:actor.canonical_session_id,target_agent:actor.agent_id}}).catch(()=>{});
  }
  const reply=terminal?.Done?.final_markdown||terminal?.Error?.message||'';
  const classification=timedOut?'timeout':/401|unauthorized|sign.in again|login expired|authentication rejected/i.test(reply)?'authentication':/429|usage[_ -]limit|rate.limit/i.test(reply)?'rate_limit':terminal?.Done?.completion==='completed'&&reply.includes('PHOENIX_SOL_READY')?'ready':'unexpected_result';
  const result={model:'gpt-5.6-sol',reasoning:'medium',classification,tool_calls:tools,elapsed_ms:Date.now()-started,terminal_received:Boolean(terminal),completion:terminal?.Done?.completion??null,providerPool:auth.accountPool,...auth};
  await writeFile(join(output,'result.json'),JSON.stringify(result,null,2),{flag:'wx'});
  console.log(JSON.stringify(result));
  if(classification!=='ready'||tools)process.exitCode=1;
});
