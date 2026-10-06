// Run the two prepared strategies sequentially; allow reversing order for replication.
import {spawn} from 'node:child_process';
import {mkdir,readFile,writeFile,open} from 'node:fs/promises';
import {join,resolve} from 'node:path';
const [binary,blender,prior,prepared,output,orderArg='notes,summary']=process.argv.slice(2);
if(![binary,blender,prior,prepared,output].every(p=>p?.startsWith('/')))throw Error('Five absolute paths required');
const order=orderArg.split(',');
if(order.length!==2||new Set(order).size!==2||order.some(s=>!['notes','summary'].includes(s)))throw Error('Order must contain notes and summary once each');
await mkdir(output,{mode:0o700});
await writeFile(join(output,'design.json'),JSON.stringify({prior,prepared,order,model:'gpt-5.6-sol',reasoning_effort:'medium',
  comparison:'Actual UI-only repair resumed from the same rejected artifact and transcript; builder context strategy differs. Same full archive, recent tail, company history, tool capabilities, reviewer history and request. Preparation cost is retained separately. Sequential order can influence caching and timing; repeat in reverse order before drawing a broad conclusion.'},null,2),{flag:'wx'});
const results=[];
let interrupted=false;
for(const [index,strategy] of order.entries()) {
  const destination=join(output,'run-'+String(index+1));
  const log=await open(join(output,'run-'+String(index+1)+'.log'),'wx',0o600);
  const child=spawn(process.execPath,[resolve('scripts/live-continuity-revision.mjs'),binary,blender,prior,destination,prepared,strategy],{stdio:['ignore','pipe','pipe']});
  const stop=()=>{interrupted=true;child.kill('SIGTERM');};
  process.once('SIGINT',stop);process.once('SIGTERM',stop);
  const capture=async stream=>{for await(const chunk of stream)await log.write(chunk);};
  let status;
  try{[status]=await Promise.all([new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));}),capture(child.stdout),capture(child.stderr)]);}
  finally{process.removeListener('SIGINT',stop);process.removeListener('SIGTERM',stop);await log.close();}
  if(interrupted)throw Error('Comparison interrupted; inspect the existing run before resuming');
  const result=JSON.parse(await readFile(join(destination,'result.json'),'utf8'));
  results.push({strategy,...status,destination,result});
  await writeFile(join(output,'pair-results.json'),JSON.stringify(results,null,2));
  console.log(JSON.stringify({strategy,...status,elapsed_ms:result.elapsed_ms,tool_calls:result.tool_calls,failed_tools:result.failed_tools,checks:result.checks}));
  if(!result.checks.sourceUnchanged)throw Error('Original artifacts changed; pair cannot continue fairly');
  if(status.code!==0)process.exitCode=1;
}
