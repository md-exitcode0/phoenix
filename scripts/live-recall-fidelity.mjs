// Bounded real-model recall acceptance. One submission; never resubmit on timeout.
import net from 'node:net';
import {mkdir, open, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {homedir} from 'node:os';
const output=process.argv[2];
if(!output?.startsWith('/'))throw Error('Expected absolute evidence directory');
await mkdir(output,{recursive:true});
const sessionId='recall-fidelity-'+Date.now();
const tokens=['ALPHA-é🦊-7319','BETA-漢字-2047','GAMMA-Ω-9863'];
const record={type:'ToolResult',tool_name:'qa_fixture',input:'Synthetic archive paging acceptance only',success:true,
  output:'Archive QA retrieval sentinel record.\nFIRST='+tokens[0]+'\n'+'neutral filler '.repeat(500)+'\nSECOND='+tokens[1]+'\n'+'neutral filler '.repeat(500)+'\nTHIRD='+tokens[2]+'\n'};
await writeFile(join(homedir(),'.phoenix','sessions',sessionId+'.archive.jsonl'),JSON.stringify(record)+'\n',{flag:'wx',mode:0o600});
const log=await open(join(output,'gateway-events.jsonl'),'wx',0o600);
const prompt='Read-only recall acceptance. A synthetic QA retrieval sentinel record exists in this session’s compacted archive. Use recall search to locate it, then exact line retrieval and every next_offset needed to recover the complete record. Report FIRST, SECOND, and THIRD exactly, preserving Unicode. Use only recall; no files, commands, browser, memory writes, todo tools, or delegation. Do not infer missing suffixes from search excerpts. This is fixture data, not personal memory.';
const turnId=sessionId+'-turn';
console.log(JSON.stringify({sessionId,turnId}));
const socket=net.createConnection(join(homedir(),'.phoenix','gateway.sock'));
socket.on('connect',()=>socket.write(JSON.stringify({Turn:{session_id:sessionId,turn_id:turnId,user_request:prompt,workspace:output,interaction_mode:'execute',permission_mode:'full_access',journal:true,delivery:'queue'}})+'\n'));
let pending='',terminal=false,final='',events=[];
for await(const chunk of socket){pending+=chunk;let end;while((end=pending.indexOf('\n'))>=0){
  const row=JSON.parse(pending.slice(0,end));pending=pending.slice(end+1);
  await log.write(JSON.stringify({at:new Date().toISOString(),value:row})+'\n');
  if(row.Story){events.push(row.Story);if(row.Story.kind==='tool')console.log(JSON.stringify(row.Story));}
  if(row.Error)throw Error(JSON.stringify(row.Error));
  if(row.Done){terminal=true;final=row.Done.final_markdown;socket.end();break;}
}if(terminal)break;}
if(!terminal)throw Error('No terminal receipt; inspect this session, do not resubmit');
const calls=events.filter(s=>s.kind==='tool');
const inputs=calls.map(s=>{try{return JSON.parse(s.target);}catch{return {};}});
const checks={exactUnicodeValues:tokens.every(t=>final.includes(t)),onlyRecall:calls.length>0&&calls.every(s=>s.tool==='recall'),noFailedTools:calls.every(s=>s.ok),performedSearch:inputs.some(i=>i.query&&i.line==null),pagedExactRead:[0,6000,12000].every(offset=>inputs.some(i=>i.line===1&&(i.offset??0)===offset))};
await log.write(JSON.stringify({at:new Date().toISOString(),value:{Checks:checks}})+'\n');
await log.close();console.log(JSON.stringify({checks,final}));
if(!Object.values(checks).every(Boolean))process.exitCode=1;
