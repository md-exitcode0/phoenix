import {spawn} from 'node:child_process';
import {mkdir,readFile,writeFile,open} from 'node:fs/promises';
import {join} from 'node:path';
import {withAcceptanceGateway} from './lib/acceptance-gateway.mjs';
const [binary,probe,source,questions,output,prepared]=process.argv.slice(2);
if(![binary,probe,source,output].every(p=>p?.startsWith('/'))||!(questions==='--prepare-only'||questions?.startsWith('/')))throw Error('Absolute binary, probe, source, output and question path (or --prepare-only) required');
await mkdir(output,{mode:0o700});
await withAcceptanceGateway({binary,output,workspace:output,memory:true,setup:async({home,env})=>{
  env.PHOENIX_NO_LIBRARIAN='1';
  const path=join(home,'config.toml');
  await writeFile(path,(await readFile(path,'utf8'))+'\n[profile.llm.efforts]\nlibrarian="medium"\n');
}},async({env})=>{
  const log=await open(join(output,'probe.log'),'wx',0o600);
  const child=spawn(probe,[source,questions,join(output,'comparison'),...(prepared?[prepared]:[])],{env,stdio:['ignore','pipe','pipe']});
  const stop=()=>child.kill('SIGTERM');
  process.once('SIGINT',stop);process.once('SIGTERM',stop);
  const timer=setTimeout(stop,10*60*1000);
  const capture=async stream=>{for await(const chunk of stream)await log.write(chunk);};
  try{
    const [status]=await Promise.all([new Promise((resolve,reject)=>{
      child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));
    }),capture(child.stdout),capture(child.stderr)]);
    await writeFile(join(output,'probe-exit.json'),JSON.stringify(status),{flag:'wx'});
    console.log(JSON.stringify(status));if(status.code!==0)process.exitCode=1;
  }finally{clearTimeout(timer);process.removeListener('SIGINT',stop);process.removeListener('SIGTERM',stop);await log.close();}
});
