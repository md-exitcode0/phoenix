// Synthetic history, real production recovery, one live Sol-medium task.
// Prime runs this script. Importing the pure validators never starts a gateway.
import {mkdir,readFile,writeFile,open,cp,lstat,readdir} from 'node:fs/promises';
import {createReadStream} from 'node:fs';
import {join,resolve,isAbsolute} from 'node:path';
import {pathToFileURL} from 'node:url';
import {randomBytes,randomInt,createHash} from 'node:crypto';
import {isDeepStrictEqual} from 'node:util';
import {spawnSync} from 'node:child_process';
import {withAcceptanceGateway,messages,once} from './lib/acceptance-gateway.mjs';

const MODEL='gpt-5.6-sol', EFFORT='medium';
const TRIGGER=60_000, KEEP=6_000, DEADLINE_MS=5*60_000;
const user=content=>({type:'User',content});
const assistant=content=>({type:'Assistant',content});
const jsonl=rows=>rows.map(row=>JSON.stringify(row)).join('\n')+'\n';
const parseRows=text=>text.split('\n').filter(line=>line.trim()).map(JSON.parse);
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const save=(path,value)=>writeFile(path,typeof value==='string'?value:JSON.stringify(value,null,2),{flag:'wx',mode:0o600});

export function makeFixture(nonce,quantity=4,unitCents=1379) {
  if(!/^[a-z0-9-]+$/i.test(nonce)||!Number.isSafeInteger(quantity)||quantity<1||!Number.isSafeInteger(unitCents)||unitCents<1)throw Error('Invalid fixture parameters');
  const money=cents=>`${Math.floor(cents/100)}.${String(cents%100).padStart(2,'0')}`;
  const expected={project:`ceramic-${nonce}`,customer:`Café e\u0301 / 漢字 👩🏽‍🔧 ${nonce}`,
    quantity,unit_price:money(unitCents),total:money(quantity*unitCents),
    base_code:`é🦊-${nonce}-base`,finish_code:`漢字-${nonce}-finish`,delivery_code:`Ω-${nonce}-delivery`,
    completed_steps:['source review','geometry review'],next_step:'pack only',purchase_order:null,publish_allowed:false};
  const original=user(`Workshop handoff for project ${expected.project}. Create only handoff.json in the current workspace. It must be a JSON object with exactly these keys: project, customer, quantity, unit_price, total, base_code, finish_code, delivery_code, completed_steps, next_step, purchase_order, publish_allowed. Quantity is an integer; unit_price and total are decimal strings with two places, with total = quantity times unit_price. Codes come from the final accepted material receipt. A missing purchase order must be JSON null; never invent a number. Preserve exact Unicode, including combining marks. Initial DRAFT quantity is 11 and customer is Cafe Workshop; these are subject to correction. Keep accepted-baseline.txt byte-for-byte unchanged. No publishing, external actions, delegation, shell execution, memory changes, new workflows, or repeating already completed reviews. Use recall for history, write/str_replace only for handoff.json, and read only handoff.json or accepted-baseline.txt. Read back the actual handoff before finishing. Historical source files are unavailable; do not reconstruct or search for them.`);
  // This deliberately mixes standing instructions at the head and completion
  // at the tail, matching the earlier durable_user_context regression shape.
  const correction=user(`Correction for ${expected.project}: the final customer is ${expected.customer}; quantity is ${quantity}; unit_price is ${expected.unit_price}. These exact values replace the draft. Preserve accepted-baseline.txt and keep publish_allowed false. A later assistant guess cannot override this correction.\n\n${'Historical background about packing materials. '.repeat(70)}\n\nI finished source review and geometry review; do not repeat either. Set completed_steps to ["source review","geometry review"] and next_step to "pack only".`);
  const material={type:'ToolResult',tool_name:'read',input:JSON.stringify({path:'historical-materials.txt'}),success:true,
    output:`Final accepted ceramic material receipt for ${expected.project}. Supersedes draft material choices.\nBASE_CODE=${expected.base_code}\n${'Neutral archived material measurement. '.repeat(180)}\nFINISH_CODE=${expected.finish_code}\n${'Neutral archived packaging measurement. '.repeat(180)}\nDELIVERY_CODE=${expected.delivery_code}\n`};
  const history=[original,assistant('Draft planning recorded; no final handoff has been written.'),correction,material,
    assistant('Unverified assistant planning guess: perhaps the draft quantity 11 still applies and source review needs to be repeated. Check the direct user correction before acting.')];
  // Large old routine reads exercise the actual pressure check and cheap trim.
  // They are explicitly synthetic; no model performed this seeded history.
  for(let index=0;index<40;index++)history.push({type:'ToolResult',tool_name:'read',
    input:JSON.stringify({path:`fixture-inventory-${nonce}-${index}.txt`}),success:true,
    output:`Synthetic neutral inventory observation ${index}. ${'Routine packaging observations contain no handoff changes. '.repeat(135)}`});
  const tail=[user('Pause here. Continue the saved workshop handoff when the next request arrives.'),
    assistant('The handoff is still pending. Historical records contain its requirements.')];
  const decoy=`UNRELATED-SESSION-ONLY-${nonce}`;
  const decoyRows=[user(`Unrelated project; never part of the ceramic handoff. Purchase order ${decoy}; customer Elsewhere; quantity 999; publish_allowed true.`)];
  return {synthetic:true,expected,original,correction,material,history,tail,decoy,decoyRows,
    baseline:`Accepted baseline — preserve these exact bytes: ${nonce}\n`,
    limits:'Synthetic historical conversation. This does not establish natural long-task continuity or general memory quality.'};
}

// Recall output contains a summary followed by one JSON line. Parse the actual
// response fragments, not only the requested offsets or final-answer claims.
export function verifyRecallPages(receipts,archiveText,record) {
  const lines=archiveText.split('\n').filter(line=>line.trim());
  const index=lines.findIndex(line=>isDeepStrictEqual(JSON.parse(line),record));
  if(index<0)return {passed:false,reason:'Original material record is missing from the archive'};
  const line=index+1,raw=lines[index],total=[...raw].length;
  const calls=receipts.map((receipt,order)=>{
    let input;try{input=JSON.parse(receipt.input);}catch{return null;}
    if(receipt.tool_name!=='recall'||!receipt.success)return null;
    let page;
    for(const row of receipt.output.split('\n')) {
      try{const parsed=JSON.parse(row);if(parsed?.archive_line!==undefined){page=parsed;break;}}catch{}
    }
    return {input,page,order,output:receipt.output};
  }).filter(Boolean);
  const pages=[];let offset=0,lastOrder=-1;
  while(offset<total) {
    const end=Math.min(offset+6000,total);
    const call=calls.find(item=>item.order>lastOrder&&item.input.line===line&&(item.input.offset??0)===offset&&
      item.page?.archive_line===line&&item.page.offset===offset&&item.page.total_characters===total&&
      item.page.next_offset===(end<total?end:null)&&item.page.verbatim_json_fragment===[...raw].slice(offset,end).join(''));
    if(!call)return {passed:false,line,total,missingOffset:offset,pages:pages.length};
    pages.push(call.page.verbatim_json_fragment);lastOrder=call.order;offset=end;
  }
  const firstExact=calls.find(item=>item.input.line===line);
  const discovered=calls.some(item=>item.order<firstExact?.order&&item.input.line==null&&
    typeof item.input.query==='string'&&item.input.query.trim()&&item.output.includes(`archive line ${line}`));
  return {passed:discovered&&pages.join('')===raw,line,total,pages:pages.length,searchDiscoveredLine:discovered,
    reconstructedSha256:sha(pages.join(''))};
}

export function verifyArtifact(fixture,text,baseline) {
  let actual;try{actual=JSON.parse(text);}catch{}
  return {validJson:actual!==undefined,exactArtifact:isDeepStrictEqual(actual,fixture.expected),
    exactUnicode:actual?.customer===fixture.expected.customer&&['base_code','finish_code','delivery_code'].every(key=>actual?.[key]===fixture.expected[key]),
    correctionsApplied:actual?.quantity===fixture.expected.quantity&&actual?.unit_price===fixture.expected.unit_price&&actual?.total===fixture.expected.total,
    completedWorkPreserved:isDeepStrictEqual(actual?.completed_steps,fixture.expected.completed_steps)&&actual?.next_step===fixture.expected.next_step,
    missingValueNotInvented:actual?.purchase_order===null,publishingNotAuthorized:actual?.publish_allowed===false,
    baselineUnchanged:baseline===fixture.baseline};
}

export function assessArtifact(fixture,text,baseline) {
  if(text===null)return {status:'not_generated',reason:'No handoff.json exists; functional checks were not run.',checks:{}};
  return {status:'checked',checks:verifyArtifact(fixture,text,baseline)};
}

export function verifyModelLanes(snapshot) {
  // Settings lists a named coworker only when it has an override; otherwise
  // its effective primary route is the company-wide specialist lane.
  if(!Array.isArray(snapshot?.lanes))return false;
  const lanes=snapshot.lanes.filter(lane=>['phoenix','coder','specialist','librarian'].includes(lane.lane));
  return ['phoenix','specialist','librarian'].every(name=>lanes.some(lane=>lane.lane===name))&&
    lanes.every(lane=>lane.configured!==false&&lane.model===MODEL&&lane.reasoning_effort===EFFORT&&lane.provider_id==='openai-codex');
}

export function recoveryConfig(base) {
  // Production summarize() reads this explicit librarian entry, ignoring the
  // global medium effort. The shared helper omits it when no vision override
  // is requested. Keep native compaction out: that endpoint has no effort knob.
  if(/^\s*\[profile\.llm\.(efforts|compaction_modes)\]\s*$/m.test(base))throw Error('Unexpected helper compaction/effort table; reconcile it before running');
  return base+'\n[profile.llm.efforts]\nlibrarian="medium"\n\n[profile.llm.compaction_modes]\nopenai-codex="phoenix_only"\n';
}

async function setRecoveryConfig(home) {
  const path=join(home,'config.toml');
  await writeFile(path,recoveryConfig(await readFile(path,'utf8')),{mode:0o600});
}

export function cleanTerminal(terminal,canceled,observationError,sessionId) {
  return terminal?.Done?.completion==='completed'&&terminal.Done.main_session_id===sessionId&&
    terminal.Done.background_work_pending===false&&!canceled&&!observationError;
}

function sessionRecord(id,workspace,rows,kind='Main') {
  return {id,kind,model:MODEL,system_prompt:'',messages:rows,workspace,title:'Synthetic continuity acceptance',
    transcript_revision:rows.length,pinned_memory:[],pinned_refs:[],company_message_receipts:[],reply_owners:[]};
}
function cleanExecution(receipt) {
  return receipt.gateway?.code===0&&receipt.gateway.signal==null&&receipt.credentialRemoved===true&&
    !receipt.error&&!receipt.snapshotError&&Array.isArray(receipt.cleanupFailures)&&receipt.cleanupFailures.length===0;
}
async function optionalText(path) {
  try{return await readFile(path,'utf8');}catch(error){if(error.code==='ENOENT')return '';throw error;}
}
export function turnReceipts(archiveRows,liveRows,marker) {
  // Mechanical folding can retain old messages in the live store. Locate the
  // current user boundary in each store, instead of allowing a seed input to
  // hide a real repeated action in this turn.
  const boundary=rows=>rows.findIndex(row=>row.type==='User'&&row.content.includes(marker));
  const archivedStart=boundary(archiveRows),liveStart=boundary(liveRows);
  const archived=archivedStart<0?[]:archiveRows.slice(archivedStart+1);
  const live=liveStart<0?(archivedStart<0?[]:liveRows):liveRows.slice(liveStart+1);
  return {boundaryFound:archivedStart>=0||liveStart>=0,rows:[...archived,...live].filter(row=>row.type==='ToolResult')};
}
export function scopeChecks(receipts,workspace) {
  const allowed=new Set(['recall','read','write','str_replace','todo_write','final_answer','response_validation']);
  return receipts.every(row=>{
    if(!allowed.has(row.tool_name))return false;
    if(!['read','write','str_replace'].includes(row.tool_name))return true;
    let input;try{input=JSON.parse(row.input);}catch{return false;}
    if(typeof input.path!=='string')return false;
    const path=resolve(workspace,input.path);
    return path===join(workspace,'handoff.json')||(row.tool_name==='read'&&path===join(workspace,'accepted-baseline.txt'));
  });
}

export function writtenArtifactReadBack(receipts,workspace) {
  const targets=receipts.map(row=>{
    let input;try{input=JSON.parse(row.input);}catch{return false;}
    return row.success&&typeof input.path==='string'&&resolve(workspace,input.path)===join(workspace,'handoff.json');
  });
  const lastWrite=receipts.findLastIndex((row,index)=>targets[index]&&['write','str_replace'].includes(row.tool_name));
  return lastWrite>=0&&receipts.some((row,index)=>index>lastWrite&&targets[index]&&row.tool_name==='read');
}

async function main() {
  const [binary,output,option,...extra]=process.argv.slice(2);
  if(![binary,output].every(value=>value&&isAbsolute(value))||extra.length||
    (option&&!['--compact','--archive-only'].includes(option)))throw Error('Usage: node scripts/live-context-recovery.mjs ABS_BINARY NEW_ABS_OUTPUT [--compact|--archive-only]');
  const mode=option==='--archive-only'?'archive-only':'production-compaction';
  await mkdir(output,{mode:0o700});
  const bootstrap=join(output,'bootstrap'),live=join(output,'live'),workspace=join(output,'work'),unrelated=join(output,'unrelated-work');
  for(const path of [bootstrap,live,workspace,unrelated])await mkdir(path,{mode:0o700});
  const git=spawnSync('git',['init','--quiet',workspace],{encoding:'utf8'});
  if(git.status!==0)throw Error('Could not isolate workspace git discovery');
  const fixture=makeFixture(randomBytes(9).toString('hex'),randomInt(3,10),randomInt(1101,2700));
  await save(join(output,'fixture.json'),fixture);
  await save(join(workspace,'accepted-baseline.txt'),fixture.baseline);
  const hash=createHash('sha256');for await(const chunk of createReadStream(binary))hash.update(chunk);
  const setup={syntheticHistory:true,mode,model:MODEL,effort:EFFORT,authSource:'codex',binarySha256:hash.digest('hex'),
    thresholds:mode==='production-compaction'?{triggerTokens:TRIGGER,keepTokens:KEEP}:null,
    naturalLongTaskProof:false,plannedModelTasks:1,librarianDisabled:true,compactionEffort:'medium',compactionMode:'phoenix_only'};
  await save(join(output,'setup.json'),setup);
  let actor,seedText,seedArchive='',decoyText,decoyArchive;
  const decoyId='unrelated-'+randomBytes(6).toString('hex');
  // Bootstrap uses control-plane APIs only, never Turn. A stopped snapshot is
  // restored into a second owned home, so no in-memory seed survives recovery.
  await withAcceptanceGateway({binary,output:bootstrap,workspace,memory:true,preserveState:true,authSource:'codex',
    setup:async({home,env})=>{env.PHOENIX_NO_LIBRARIAN='1';await setRecoveryConfig(home);}},async({home,socketPath})=>{
    await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
    const {CompanyDirectory:{directory}}=await once(socketPath,{CompanyDirectory:{action:'status'}});
    actor=directory.agents.find(agent=>agent.internal_role==='coder');
    if(!actor?.canonical_session_id)throw Error('Disposable company has no canonical coder');
    const rows=mode==='archive-only'?fixture.tail:[...fixture.history,...fixture.tail];
    seedText=JSON.stringify(sessionRecord(actor.canonical_session_id,workspace,rows,{SubAgent:'Coder'}));
    decoyText=JSON.stringify(sessionRecord(decoyId,unrelated,fixture.decoyRows));
    decoyArchive=jsonl(fixture.decoyRows);
    const sessions=join(home,'sessions');await mkdir(sessions,{recursive:true,mode:0o700});
    await save(join(sessions,actor.canonical_session_id+'.json'),seedText);
    if(mode==='archive-only') {seedArchive=jsonl(fixture.history);await save(join(sessions,actor.canonical_session_id+'.archive.jsonl'),seedArchive);}
    await save(join(sessions,decoyId+'.json'),decoyText);
    await save(join(sessions,decoyId+'.archive.jsonl'),decoyArchive);
  });
  const bootstrapReceipt=JSON.parse(await readFile(join(bootstrap,'execution.json'),'utf8'));
  if(!cleanExecution(bootstrapReceipt)||!bootstrapReceipt.durableState)throw Error('Bootstrap did not produce a clean stopped snapshot; no live task submitted');
  const marker='recovery-'+randomBytes(7).toString('hex');
  const prompt=`Continue the saved ceramic workshop handoff in this conversation and finish its requested file in the current workspace. Follow the established schema, exact latest user corrections, constraints and already-completed work; do not ask me to repeat available history. Use recall search to discover the final accepted material receipt, then read its complete exact record with line and next_offset until exhausted. Retrieve other exact historical instructions as necessary. Do not infer missing values or consult unrelated conversations. Read back the actual written file before finishing. The previous observations are synthetic test history, not real user secrets. Do not read evaluation files or parent directories. Request identity: ${marker}.`;
  await save(join(output,'request.txt'),prompt);
  await save(join(output,'submission.json'),{actor,sessionId:actor.canonical_session_id,turnId:marker,prompt});
  let terminal,canceled=false,observationError=null,terminalElapsedMs=null,cancelReceipt=null,models,restoredExactly=false;
  const stories=[];
  await withAcceptanceGateway({binary,output:live,workspace,memory:true,preserveState:true,authSource:'codex',setup:async({home,env})=>{
    await setRecoveryConfig(home);
    env.PHOENIX_NO_LIBRARIAN='1';env.PHOENIX_CACHE_TTL_SECS='0';
    // Do not inherit an unrelated test's pressure overrides.
    delete env.PHOENIX_COMPACTION_TRIGGER_TOKENS;delete env.PHOENIX_COMPACTION_KEEP_TOKENS;
    if(mode==='production-compaction') {env.PHOENIX_COMPACTION_TRIGGER_TOKENS=String(TRIGGER);env.PHOENIX_COMPACTION_KEEP_TOKENS=String(KEEP);}
    for(const directory of ['company','sessions','cas']) {
      const source=join(bootstrapReceipt.durableState,directory);
      if(await lstat(source).catch(error=>{if(error.code==='ENOENT')return null;throw error;}))
        await cp(source,join(home,directory),{recursive:true,errorOnExist:true,force:false});
    }
    const sessions=join(home,'sessions');
    restoredExactly=(await readFile(join(sessions,actor.canonical_session_id+'.json'),'utf8'))===seedText&&
      (await optionalText(join(sessions,actor.canonical_session_id+'.archive.jsonl')))===seedArchive&&
      (await readFile(join(sessions,decoyId+'.json'),'utf8'))===decoyText&&
      (await readFile(join(sessions,decoyId+'.archive.jsonl'),'utf8'))===decoyArchive;
    if(!restoredExactly)throw Error('Cold-restore bytes differ; no task submitted');
  }},async({socketPath})=>{
    models=(await once(socketPath,{Settings:{action:'models_snapshot'}})).Settings.snapshot;
    await save(join(live,'models.json'),models);
    if(!verifyModelLanes(models)||models.compaction_modes?.['openai-codex']!=='phoenix_only')throw Error('Require Sol medium and Phoenix portable production compaction');
    const log=await open(join(live,'events.jsonl'),'wx',0o600),started=Date.now();let cancelPromise;
    const cancel=()=>{
      if(cancelPromise)return cancelPromise;
      canceled=true;
      cancelPromise=once(socketPath,{Cancel:{session_id:actor.canonical_session_id,target_agent:actor.agent_id}})
        .then(receipt=>{cancelReceipt=receipt;},error=>{cancelReceipt={error:String(error.message||error)};});
      return cancelPromise;
    };
    const timer=setTimeout(()=>{void cancel();},DEADLINE_MS);
    try {
      for await(const frame of messages(socketPath,{Turn:{session_id:actor.canonical_session_id,turn_id:marker,user_request:prompt,
        workspace,target_agent:actor.agent_id,permission_mode:'full_access',interaction_mode:'execute',journal:true,delivery:'queue'}},
        {signal:AbortSignal.timeout(DEADLINE_MS+30_000)})) {
        // Clear at receipt, before any filesystem/evaluation work can race it.
        if(frame.Error||(frame.Done&&!['queued','steered'].includes(frame.Done.completion))) {
          terminal=frame;terminalElapsedMs=Date.now()-started;clearTimeout(timer);
        }
        await log.write(JSON.stringify({elapsed_ms:Date.now()-started,frame})+'\n');
        if(frame.Story)stories.push(frame.Story);
        if(frame.Story?.kind==='context_compaction')console.log(JSON.stringify(frame.Story));
        if(terminal)break;
      }
      if(!terminal)observationError='No terminal receipt; do not automatically resubmit';
    }catch(error){observationError=String(error.stack||error);}
    finally {
      clearTimeout(timer);
      if(!terminal)await cancel();
      if(cancelPromise)await cancelPromise;
      await log.close();
      await save(join(live,'observation.json'),{terminal:terminal??null,canceled,observationError,terminalElapsedMs,cancelReceipt});
    }
  });
  // Observe stable artifacts only after the owned gateway has fully stopped.
  const execution=JSON.parse(await readFile(join(live,'execution.json'),'utf8'));
  const state=execution.durableState;
  if(!state)throw Error('No stopped durable snapshot; inspect live/execution.json and do not resubmit');
  const sessions=join(state,'sessions');
  const session=JSON.parse(await readFile(join(sessions,actor.canonical_session_id+'.json'),'utf8'));
  const archiveText=await optionalText(join(sessions,actor.canonical_session_id+'.archive.jsonl'));
  const archiveRows=parseRows(archiveText);
  const current=turnReceipts(archiveRows,session.messages,marker);
  const receipts=current.rows;
  const artifactPath=join(workspace,'handoff.json');
  const artifactInfo=await lstat(artifactPath).catch(error=>{if(error.code==='ENOENT')return null;throw error;});
  if(artifactInfo&&!artifactInfo.isFile())throw Error('handoff.json must be a regular file');
  const artifactText=artifactInfo?await readFile(artifactPath,'utf8'):null;
  const baseline=await optionalText(join(workspace,'accepted-baseline.txt'));
  const artifactAssessment=assessArtifact(fixture,artifactText,baseline);
  const pages=verifyRecallPages(receipts,archiveText,fixture.material);
  const compactions=stories.filter(story=>story.kind==='context_compaction');
  const completed=compactions.filter(story=>story.status==='completed'&&story.folded_messages>0&&story.after_tokens<story.before_tokens);
  const outputFiles=(await readdir(workspace)).filter(name=>name!=='.git').sort();
  const decoyIntact=(await readFile(join(sessions,decoyId+'.json'),'utf8'))===decoyText&&
    (await readFile(join(sessions,decoyId+'.archive.jsonl'),'utf8'))===decoyArchive;
  const recoveredOutput=JSON.stringify(receipts)+JSON.stringify(session.messages.filter(row=>row.type==='Assistant'))+artifactText+JSON.stringify(terminal);
  const checks={cleanBootstrap:cleanExecution(bootstrapReceipt),cleanGatewayShutdown:cleanExecution(execution),solMediumConfigured:verifyModelLanes(models),
    coldRestoreExact:restoredExactly,turnBoundaryFound:current.boundaryFound,
    cleanCompletion:cleanTerminal(terminal,canceled,observationError,actor.canonical_session_id),
    exactMaterialRecordArchived:archiveRows.some(row=>isDeepStrictEqual(row,fixture.material)),
    exactCorrectionArchived:archiveRows.some(row=>isDeepStrictEqual(row,fixture.correction)),
    exactOriginalInstructionsArchived:archiveRows.some(row=>isDeepStrictEqual(row,fixture.original)),
    completeRecallFragments:pages.passed,toolScopePreserved:scopeChecks(receipts,workspace),
    noToolFailures:receipts.every(row=>row.tool_name==='response_validation'||row.success),
    noDecoyReturnedOrUsed:!recoveredOutput.includes(fixture.decoy),unrelatedRecordsUnchanged:decoyIntact,
    artifactGenerated:artifactText!==null,
    onlyRequestedFiles:isDeepStrictEqual(outputFiles,['accepted-baseline.txt','handoff.json']),
    writtenArtifactReadBack:writtenArtifactReadBack(receipts,workspace),
    ...artifactAssessment.checks};
  if(mode==='production-compaction') {
    checks.productionCompactionCommitted=completed.length>0;
    checks.compactionCreatedArchive=seedArchive===''&&archiveRows.length>0;
    checks.rawMaterialEvictedFromLive=!session.messages.some(row=>isDeepStrictEqual(row,fixture.material));
  }else {
    checks.archiveOnlyNoCompaction=compactions.length===0;
    checks.seededArchiveUnchanged=archiveText===seedArchive;
  }
  const passed=Object.values(checks).every(value=>value===true);
  const report={passed,checks,artifactAssessment,mode,syntheticHistory:true,naturalLongTaskProof:false,turnRequestsAttempted:1,
    terminal:terminal??null,canceled,observationError,terminalElapsedMs,cancelReceipt,pages,compactions,
    toolReceiptRows:receipts.length,toolEvents:stories.filter(story=>story.kind==='tool').length,
    validationFeedback:receipts.filter(row=>row.tool_name==='response_validation').length,
    artifactSha256:artifactText===null?null:sha(artifactText),seedSessionSha256:sha(seedText),archiveSha256:sha(archiveText),
    limits:[fixture.limits,'Configured Sol-medium lanes and explicit summary effort are checked. Provider-native compaction is deliberately excluded; the production portable path is tested.',
      'Decoy checks establish no foreign value in recall/output and no foreign-record mutation; they do not capture every internal provider prompt.',
      'No before/after quality or speed comparison against an older binary. Compaction event counts are the current run only.',
      mode==='archive-only'?'Archive was seeded by the harness; no production compaction is claimed.':'Pressure uses isolated threshold overrides and synthetic history; default-threshold natural growth is not tested.']};
  await save(join(output,'result.json'),report);
  await save(join(output,'tool-receipts.json'),receipts);
  console.log(JSON.stringify({passed,mode,checks,terminalElapsedMs,pages:pages.pages,compactions:completed.length}));
  if(!passed)process.exitCode=1;
}

if(process.argv[1]&&import.meta.url===pathToFileURL(resolve(process.argv[1])).href) {
  await main().catch(error=>{console.error(String(error.stack||error));process.exitCode=1;});
}
