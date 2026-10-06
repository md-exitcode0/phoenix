// Discovery and exact retrieval using production recall, private fixture data,
// and the same Sol-medium runtime used by the application tests.
import {mkdir,writeFile,readFile,open} from 'node:fs/promises';
import {join} from 'node:path';
import {randomBytes,randomInt} from 'node:crypto';
import {withAcceptanceGateway,messages,once} from './lib/acceptance-gateway.mjs';
const [binary,output]=process.argv.slice(2);
if(!binary?.startsWith('/')||!output?.startsWith('/'))throw Error('Absolute binary and NEW evidence directory required');
await mkdir(output,{mode:0o700});
await withAcceptanceGateway({binary,output,workspace:output,memory:true,
  setup:async({env})=>{env.PHOENIX_NO_LIBRARIAN='1';}},async({home,socketPath})=>{
  const sessionId='recall-discovery-'+Date.now();
  const values=['é🦊-'+randomBytes(5).toString('hex'),'漢字-'+randomBytes(5).toString('hex'),'Ω-'+randomBytes(5).toString('hex')];
  const rows=Array.from({length:47},(_,i)=>({type:'ToolResult',tool_name:'fixture_observation',
    input:'Unrelated workshop record '+i,success:true,output:'Inventory item '+i+' uses standard packaging. '.repeat(30)}));
  const targetIndex=randomInt(5,42);
  rows.splice(targetIndex,0,{type:'ToolResult',tool_name:'fixture_observation',success:true,
    input:'Ceramic holder accepted design record',output:'Ceramic holder final accepted design. Supersedes earlier draft.\nBASE_CODE='+values[0]+'\n'+
      'Neutral archived measurement detail. '.repeat(240)+'\nFINISH_CODE='+values[1]+'\n'+
      'Neutral archived packaging detail. '.repeat(240)+'\nDELIVERY_CODE='+values[2]+'\n'});
  const sessions=join(home,'sessions');await mkdir(sessions,{recursive:true,mode:0o700});
  const archive=join(sessions,sessionId+'.archive.jsonl');const original=rows.map(r=>JSON.stringify(r)).join('\n')+'\n';
  await writeFile(archive,original,{flag:'wx',mode:0o600});
  await writeFile(join(output,'expected.json'),JSON.stringify({targetLine:targetIndex+1,values},null,2),{flag:'wx',mode:0o600});
  const request='Read-only archived design recall. Find the final accepted Ceramic holder design record in this session’s archive. Use recall search first; the record’s line number is unknown. Retrieve its complete exact record, following next_offset until exhausted. Report BASE_CODE, FINISH_CODE and DELIVERY_CODE exactly, preserving Unicode. Use only recall and final_answer. Do not read files, execute commands, modify memory, delegate, or infer missing suffixes from snippets. These are synthetic workshop fixture values, not user secrets.';
  const log=await open(join(output,'events.jsonl'),'wx',0o600);const started=Date.now();let terminal;
  const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:sessionId}}).catch(()=>{}),180000);
  try{
    for await(const value of messages(socketPath,{Turn:{session_id:sessionId,turn_id:sessionId+'-turn',user_request:request,
      workspace:output,interaction_mode:'execute',permission_mode:'full_access',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify({at:Date.now(),value})+'\n');
      if(value.Done||value.Error){terminal=value;break;}
    }
  }finally{clearTimeout(timer);await log.close();}
  if(!terminal)throw Error('No terminal receipt; inspect this exact session before any retry');
  const session=JSON.parse(await readFile(join(sessions,sessionId+'.json'),'utf8'));
  const calls=session.messages.filter(m=>m.type==='ToolResult');
  const inputs=calls.filter(c=>c.tool_name==='recall').map(c=>JSON.parse(c.input));
  const exact=inputs.filter(i=>i.line===targetIndex+1);
  const target=JSON.stringify(rows[targetIndex]);
  const offsets=Array.from({length:Math.ceil([...target].length/6000)},(_,i)=>i*6000);
  const final=terminal.Done?.final_markdown||'';
  const checks={terminalSuccess:!!terminal.Done,searchBeforeExact:inputs[0]?.query?.length>0&&inputs[0]?.line==null,
    discoveredCorrectLine:exact.length>0,allPages:offsets.every(offset=>exact.some(i=>(i.offset||0)===offset)),
    exactValues:values.every(v=>final.includes(v)),onlyRecall:calls.length>0&&calls.every(c=>c.tool_name==='recall'),
    noToolFailures:calls.every(c=>c.success),archiveUnchanged:(await readFile(archive,'utf8'))===original};
  const report={checks,elapsed_ms:Date.now()-started,tool_calls:calls.length,inputs,terminal};
  await writeFile(join(output,'result.json'),JSON.stringify(report,null,2),{flag:'wx'});
  console.log(JSON.stringify({checks,elapsed_ms:report.elapsed_ms,tool_calls:report.tool_calls}));
  if(!Object.values(checks).every(Boolean))process.exitCode=1;
});
