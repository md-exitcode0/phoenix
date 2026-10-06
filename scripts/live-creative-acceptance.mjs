// Plain user briefs through the actual Phoenix runtime. No construction,
// coaching, or visual verdict is supplied by this observer during a run.
import {mkdir,open,readFile,writeFile,readdir,copyFile,stat,cp} from 'node:fs/promises';
import {createReadStream} from 'node:fs';
import {join,relative} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,messages,once} from './lib/acceptance-gateway.mjs';
import {acceptanceBrowser} from './lib/acceptance-browser.mjs';
import {collectDesktopEvidence} from './collect-desktop-evidence.mjs';
import {peerResult} from './lib/acceptance-evidence.mjs';
import {creativeScenarios,creativeScenario,creativeArtifactChecks} from './lib/creative-scenarios.mjs';
import {acceptanceAuthOptions} from './lib/acceptance-account.mjs';

const [binary,output,scenario,...options]=process.argv.slice(2);
const minutesArg=options[0]&&!options[0].startsWith('--')?options.shift():'20';
const minutes=Number(minutesArg);
const resumeOptions=options.filter(value=>value.startsWith('--resume-from='));
if(resumeOptions.length>1)throw Error('Choose one resume source');
const resumeOption=resumeOptions[0];
if(resumeOption&&!resumeOption.startsWith('--resume-from=/'))throw Error('Resume option requires an absolute evidence directory');
const resumedFrom=resumeOption?.slice('--resume-from='.length);
const auth=acceptanceAuthOptions(options.filter(value=>!value.startsWith('--resume-from=')));
if(![binary,output].every(p=>p?.startsWith('/'))||!Object.hasOwn(creativeScenarios,scenario)||!Number.isInteger(minutes)||minutes<5||minutes>30)
  throw Error(`Usage: ABSOLUTE_BINARY NEW_ABSOLUTE_OUTPUT ${Object.keys(creativeScenarios).join('|')} [5–30 minutes] [--resume-from=/EVIDENCE] [--provider-pool | --profile=ID | --codex-login]`);
const recipe=creativeScenario(scenario),isWebsite=recipe.kind==='website';
await mkdir(output,{mode:0o700});
let prior;
if(resumedFrom){
  const [execution,result,setup,submission]=await Promise.all(['execution','result','setup','submission'].map(name=>readFile(join(resumedFrom,name+'.json'),'utf8').then(JSON.parse)));
  if(execution.gateway?.code!==0||!execution.credentialRemoved||!execution.durableState||execution.error||setup.scenario!==scenario||!result.terminal)
    throw Error('Resume requires a terminal matching run, clean stopped gateway and preserved state');
  prior={execution,result,setup,submission};
}
const workspace=prior?.setup.workspace|| (prior?join(resumedFrom,'work'):join(output,'work'));
if(!prior){
  await mkdir(workspace);
  const init=spawnSync('git',['init','--quiet',workspace],{encoding:'utf8'});
  if(init.status!==0)throw Error('Could not establish the isolated repository boundary');
}else{
  // Continue the actual working files/paths after preserving an immutable
  // checkpoint. Old result hashes still describe that earlier checkpoint.
  await cp(workspace,join(output,'before'),{recursive:true,errorOnExist:true,force:false,
    filter:path=>!relative(workspace,path).split('/').some(part=>['.git','node_modules','.cache'].includes(part))});
}
if(recipe.reference&&!prior){
  const source=fileURLToPath(new URL('../'+recipe.reference,import.meta.url));
  await copyFile(source,join(workspace,'banana-reference.jpg'));
}
const originalPrompt=prior?.setup.originalPrompt||prior?.setup.prompt||recipe.prompt;
const prompt=prior?`${isWebsite?'@Iris ':''}Continue the existing task to completion using its saved work and original requirements.`:originalPrompt;
const historyTurns=[...(prior?.setup.historyTurns||[]),...(prior?[{directory:resumedFrom,turnId:prior.submission.turnId}]:[])];
const historicalStories=await Promise.all(historyTurns.map(async source=>({turnId:source.turnId,
  stories:(await readFile(join(source.directory,'events.jsonl'),'utf8')).trim().split('\n').map(line=>JSON.parse(line).frame?.Story).filter(Boolean)})));
await writeFile(join(output,'prompt.txt'),prompt,{flag:'wx'});
async function hashFile(path){const hash=createHash('sha256');for await(const chunk of createReadStream(path))hash.update(chunk);return hash.digest('hex');}
await writeFile(join(output,'setup.json'),JSON.stringify({scenario,prompt,originalPrompt,workspace,resumedFrom:resumedFrom||null,historyTurns,initialFiles:await readdir(workspace),
  actingModel:'gpt-5.6-sol',actingEffort:'medium',nativeVision:true,...auth,
  benchmarkMode:prior?'saved-task-continuation':'fresh-creation',nativeGuiRequired:false,
  observerCoaching:false,observerAuthoredConstruction:false,deadlineMinutes:minutes,binarySha256:await hashFile(binary)},null,2));

await withAcceptanceGateway({binary,output,workspace,...auth,preserveState:true,
  setup:async args=>{
    if(prior)for(const name of ['company','sessions','cas']){
      const source=join(prior.execution.durableState,name);
      if(await stat(source).catch(error=>{if(error.code==='ENOENT')return null;throw error;}))await cp(source,join(args.home,name),{recursive:true,errorOnExist:true,force:false});
    }
    return acceptanceBrowser({...args,output});
  }},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const directory=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory;
  const actor=directory.agents.find(a=>a.agent_id===recipe.actor);
  if(!actor)throw Error('Expected owner missing; no turn submitted');
  let sessionId=actor.canonical_session_id,group,activation;
  if(isWebsite){
    if(prior)group=directory.groups.find(g=>g.group_id===prior.submission.group.group_id);
    else{
      const name=recipe.name+' acceptance '+Date.now();
      const created=await once(socketPath,{CompanyDirectory:{action:'create_group',name,
        description:'Fictional website acceptance',members:['frontend','researcher','coder'],
        color:'#688c9e',icon_seed:'tideline',settings:{read_full_transcript:true}}});
      group=created.CompanyDirectory.directory.groups.find(g=>g.name===name);
    }
    if(!group)throw Error('Expected group missing; no turn submitted');
    sessionId=group.canonical_session_id;
    const {execution_wave_display_names,active_display_names,...selected}=(await once(socketPath,{GroupActivationPreview:{group_id:group.group_id,user_request:prompt}})).GroupActivationPreview;
    activation=selected;
  }
  const turnId=`creative-${scenario}-${Date.now()}`;
  await writeFile(join(output,'submission.json'),JSON.stringify({actor,group,activation,sessionId,turnId},null,2));
  const log=await open(join(output,'events.jsonl'),'wx',0o600),started=Date.now(),stories=[];
  let terminal,canceled=false,observationError,terminalElapsedMs;
  const cancel=async()=>{if(canceled)return;canceled=true;await once(socketPath,{Cancel:{session_id:sessionId}}).catch(()=>{});};
  const timer=setTimeout(cancel,minutes*60_000);
  console.log(JSON.stringify({phase:'submitted',scenario,home,sessionId,turnId}));
  try{
    try{
      for await(const frame of messages(socketPath,{Turn:{session_id:sessionId,turn_id:turnId,user_request:prompt,
        workspace,permission_mode:'full_access',interaction_mode:'execute',journal:true,delivery:'queue',
        ...(group?{target_group:group.group_id,group_activation:activation}:{target_agent:actor.agent_id})}},
        {signal:AbortSignal.timeout((minutes*60+30)*1000)})){
        await log.write(JSON.stringify({elapsed_ms:Date.now()-started,frame})+'\n');
        if(frame.Story){stories.push(frame.Story);if(['tool','status','answer','warning','handoff','return','group_message','compaction'].includes(frame.Story.kind))
          console.log(JSON.stringify({elapsed_ms:Date.now()-started,...frame.Story}));}
        if(frame.Done||frame.Error){terminal=frame;terminalElapsedMs=Date.now()-started;clearTimeout(timer);break;}
      }
    }catch(error){observationError=String(error.message);await cancel();}
    clearTimeout(timer);
    const artifactFiles=[];
    async function walk(dir){for(const entry of await readdir(dir,{withFileTypes:true})){
      if(['.git','node_modules','.cache'].includes(entry.name))continue;
      const path=join(dir,entry.name);if(entry.isDirectory())await walk(path);else if(entry.isFile())artifactFiles.push({path:relative(workspace,path),bytes:(await stat(path)).size,sha256:await hashFile(path)});
    }}
    // Without a terminal acknowledgement the writer may still be active.
    // Retain the files, but never certify hashes of an unconfirmed running job.
    if(terminal)await walk(workspace);
    await writeFile(join(output,'artifacts.json'),JSON.stringify(artifactFiles,null,2));
    const toolEvents=stories.filter(s=>s.kind==='tool');
    const handoffs=stories.filter(s=>s.kind==='handoff'),returns=stories.filter(s=>s.kind==='return');
    const contributions=['researcher','coder'].map(peer=>peerResult(stories,turnId,actor.agent_id,peer)
      ||historicalStories.map(source=>peerResult(source.stories,source.turnId,actor.agent_id,peer)).find(Boolean));
    const publications=stories.filter(s=>s.kind==='group_message'&&s.agent_id===actor.agent_id&&s.execution?.turn_id===turnId);
    const checks={cleanCompletion:terminal?.Done?.completion==='completed'&&!canceled&&!observationError,
      ...creativeArtifactChecks(recipe,artifactFiles),
      ...(isWebsite?{
        researcherContributed:Boolean(contributions[0]),
        engineeringContributed:Boolean(contributions[1]),
        oneOwnerPublication:publications.length===1,
        ownerPublishedAfterContributions:publications.length===1&&contributions.every(row=>row&&(row.execution.turn_id!==turnId||row.event_sequence<publications[0].event_sequence))
      }:{})};
    const result={scenario,checks,terminal,canceled,observationError:observationError||null,elapsedMs:terminalElapsedMs??Date.now()-started,artifactSnapshotStable:Boolean(terminal),
      toolEvents:toolEvents.length,failedToolEvents:toolEvents.filter(s=>s.ok===false).length,handoffs,returns,
      visualAcceptance:'Pending independent inspection of the actual saved output; these checks do not establish appearance or usability.'};
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));
    await collectDesktopEvidence(home,join(output,'screens'));
    console.log(JSON.stringify({phase:'terminal',scenario,checks,elapsedMs:result.elapsedMs,toolEvents:result.toolEvents,failedToolEvents:result.failedToolEvents}));
    if(!Object.values(checks).every(Boolean))process.exitCode=1;
  }finally{clearTimeout(timer);await log.close();}
});
