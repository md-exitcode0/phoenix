// Actual Phoenix, ordinary requirements, independent black-box checks after Done.
// No observer research links, implementation hints, or iterative coaching.
import {mkdir,open,readFile,writeFile} from 'node:fs/promises';
import {createReadStream} from 'node:fs';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway,messages,once} from './lib/acceptance-gateway.mjs';
import {evaluateCsvTool} from './lib/csv-acceptance.mjs';

const [binary,output,option]=process.argv.slice(2);
if(option&&option!=='--codex-login')throw Error('Supported option: --codex-login');
if(![binary,output].every(value=>value?.startsWith('/')))throw Error('Absolute binary and NEW output paths required');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
// Stop ordinary git discovery at the disposable workspace, not the user's
// enclosing project. This creates no commit and changes no parent repository.
const git=spawnSync('git',['init','--quiet',workspace],{encoding:'utf8'});
if(git.status!==0)throw Error('Could not initialize the disposable workspace repository');
const prompt='Build a small Python command-line tool, totals.py, that I can run as python3 totals.py input.csv output.json. Read UTF-8 CSV with category and amount columns and produce a JSON object mapping each category to its exact total as a two-decimal string. This is for ordinary money amounts (up to 12 digits before the decimal, at most two decimal places), including refunds. Quoted categories, commas, embedded newlines and Unicode should work. Reject missing or duplicate columns, extra or missing row fields, blank categories, malformed CSV, non-finite amounts and invalid money amounts with a clear error and a nonzero exit. On any failure keep the previous output untouched. Header-only input should produce an empty object. Include a brief usage README. Finish the working tool.';
await writeFile(join(output,'prompt.txt'),prompt,{flag:'wx'});
const binaryHash=createHash('sha256');for await(const chunk of createReadStream(binary))binaryHash.update(chunk);
await writeFile(join(output,'setup.json'),JSON.stringify({prompt,actingModel:'gpt-5.6-sol',actingEffort:'medium',observerCoaching:false,initialFiles:['.git/'],authSource:option?'codex':'phoenix',binarySha256:binaryHash.digest('hex')},null,2),{flag:'wx'});

await withAcceptanceGateway({binary,output,workspace,accountPool:!option,authSource:option?'codex':'phoenix',preserveState:true},async({home,socketPath})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const {CompanyDirectory:{directory}}=await once(socketPath,{CompanyDirectory:{action:'status'}});
  const actor=directory.agents.find(agent=>agent.internal_role==='coder');
  if(!actor?.canonical_session_id)throw Error('Disposable coder unavailable');
  const sessionId=actor.canonical_session_id,turnId='generic-coding-'+Date.now();
  await writeFile(join(output,'submission.json'),JSON.stringify({actor,sessionId,turnId},null,2));
  const log=await open(join(output,'events.jsonl'),'wx',0o600);
  const started=Date.now();let terminal,canceled=false,terminalElapsedMs;
  const timer=setTimeout(async()=>{canceled=true;try{await once(socketPath,{Cancel:{session_id:sessionId,target_agent:actor.agent_id}})}catch(error){console.error(String(error))}},12*60_000);
  try{
    for await(const frame of messages(socketPath,{Turn:{session_id:sessionId,turn_id:turnId,user_request:prompt,workspace,target_agent:actor.agent_id,permission_mode:'full_access',interaction_mode:'execute',journal:true,delivery:'queue'}},{signal:AbortSignal.timeout(12*60_000+30_000)})){
      await log.write(JSON.stringify({elapsed_ms:Date.now()-started,frame})+'\n');
      if(frame.Story&&['tool','status','answer','warning'].includes(frame.Story.kind))console.log(JSON.stringify({elapsed_ms:Date.now()-started,...frame.Story}));
      if(frame.Done||frame.Error){terminal=frame;terminalElapsedMs=Date.now()-started;clearTimeout(timer);break;}
    }
    if(!terminal)throw Error('No terminal receipt; inspect the existing turn, never resubmit automatically');
    const session=JSON.parse(await readFile(join(home,'sessions',sessionId+'.json'),'utf8'));
    await writeFile(join(output,'session.json'),JSON.stringify(session,null,2),{mode:0o600});
    const evaluation=await evaluateCsvTool(workspace,join(output,'independent-checks'));
    const {checks}=evaluation;
    const receipts=session.messages.filter(message=>message.type==='ToolResult'&&message.tool_name!=='response_validation');
    const validation=session.messages.filter(message=>message.type==='ToolResult'&&message.tool_name==='response_validation');
    // Done means the transport ended, not that the user outcome succeeded.
    const finalText=(terminal.Done?.final_markdown||'').replace(/^\*\*\[[^\]]+\]\*\*\s*/,'').replace(/^Workflow completion is unverified:[^\n]*\n\s*/,'');
    const runtimeFailure=/^(?:The provider became unavailable|This agent could not complete|The agent could not complete|Phoenix stopped this agent|I could not advance because)/i.test(finalText);
    const cleanCompletion=Boolean(terminal.Done)&&terminal.Done.completion==='completed'&&!canceled&&!runtimeFailure;
    const result={terminal,canceled,cleanCompletion,runtimeFailure,elapsedMs:terminalElapsedMs,...evaluation,toolCalls:receipts.length,failedCalls:receipts.filter(x=>!x.success).length,validationFeedback:validation.length,scope:'One ordinary coding task through the real Phoenix runtime; observer inputs are independent of builder tests. Artifact correctness and clean run completion are separate. Missing deliverables and failed positive controls cannot pass negative cases. No visual/audio or whole-product acceptance claim.'};
    await writeFile(join(output,'result.json'),JSON.stringify(result,null,2),{flag:'wx'});
    console.log(JSON.stringify({phase:'independent-evaluation',passed:result.passed,total:result.total,elapsedMs:result.elapsedMs}));
    if(!cleanCompletion||checks.some(x=>!x.passed))process.exitCode=1;
  }finally{clearTimeout(timer);await log.close();}
});
