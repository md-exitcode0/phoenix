// Fresh Phoenix reviewer receives the original brief and actual artifact pixels.
// No suggested defects, construction instructions or observer verdict.
import {mkdir,copyFile,readFile,writeFile} from 'node:fs/promises';
import {join,extname} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,once,messages} from './lib/acceptance-gateway.mjs';
const [binary,candidate,reference,briefFile,output]=process.argv.slice(2);
if(![binary,candidate,reference,briefFile,output].every(path=>path?.startsWith('/')))throw Error('Absolute binary, candidate, reference, original brief and NEW output paths required');
await mkdir(output,{mode:0o700});const workspace=join(output,'work');await mkdir(workspace);
const files=[['candidate'+extname(candidate),candidate],['reference'+extname(reference),reference]];
const hashes={};
for(const [name,source] of files){await copyFile(source,join(workspace,name));hashes[name]=createHash('sha256').update(await readFile(source)).digest('hex');}
const original=await readFile(briefFile,'utf8');
const prompt=`Review the visible finish of ${files[0][0]} against ${files[1][0]} and the original user request below. This is an image-only review package, not the builder's workspace; other deliverables are outside this review and their absence here is not evidence they were not delivered. Inspect the actual images together. Say whether the original requested visual finish is met and identify the strongest visible defects. Do not edit the files. Original request: ${original}`;
await writeFile(join(output,'prompt.txt'),prompt);
await writeFile(join(output,'provenance.json'),JSON.stringify({candidate,reference,briefFile,hashes,actingModel:'gpt-5.6-sol',effort:'medium'},null,2));
await withAcceptanceGateway({binary,output,workspace,authSource:'codex',preserveState:true},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const actor=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory.agents.find(a=>a.agent_id==='critic');
  if(!actor)throw Error('Reviewer unavailable');
  const events=[],start=Date.now();let terminal;
  try{
    for await(const frame of messages(socketPath,{Turn:{session_id:actor.canonical_session_id,turn_id:'artifact-review-'+Date.now(),
      user_request:prompt,workspace,target_agent:actor.agent_id,permission_mode:'workspace',interaction_mode:'execute',journal:true,delivery:'queue'}},
      {signal:AbortSignal.timeout(180000)})){
      events.push(frame);if(frame.Done||frame.Error){terminal=frame;break;}
    }
    const saved=JSON.parse(await readFile(join(home,'sessions',actor.canonical_session_id+'.json')));
    const inspections=saved.messages.filter(row=>row.type==='ToolResult'&&row.tool_name==='image_analyze');
    const unchanged=(await Promise.all(Object.entries(hashes).map(async([name,hash])=>createHash('sha256').update(await readFile(join(workspace,name))).digest('hex')===hash))).every(Boolean);
    const nativeComparison=inspections.some(row=>row.success&&row.output.includes('Image pixels attached')&&row.output.includes('2 = reference'));
    const result={elapsedMs:Date.now()-start,terminal,answer:terminal?.Done?.final_markdown,
      checks:{cleanCompletion:terminal?.Done?.completion==='completed',nativeComparison,sourcesUnchanged:unchanged},inspections,
      scope:'Independent model critique; it does not create or repair the artifact and its judgment remains reviewable.'};
    await writeFile(join(output,'events.json'),JSON.stringify(events,null,2));
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));
    console.log(JSON.stringify({elapsedMs:result.elapsedMs,checks:result.checks,answer:result.answer}));
    if(Object.values(result.checks).some(value=>!value))process.exitCode=1;
  }finally{if(!terminal)await once(socketPath,{Cancel:{session_id:actor.canonical_session_id}}).catch(()=>{});}
});
