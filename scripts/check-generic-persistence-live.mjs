// Isolated behavioral smoke check: recover a stale source route and finish an artifact.
import assert from 'node:assert/strict';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {withAcceptanceGateway,messages,once} from './lib/acceptance-gateway.mjs';
const [binary,output]=process.argv.slice(2);
if(!binary?.startsWith('/')||!output?.startsWith('/'))throw Error('Absolute binary and output required');
await mkdir(output,{recursive:false});const workspace=join(output,'work');await mkdir(workspace);
await writeFile(join(workspace,'source-map.json'),JSON.stringify({preferred:'old/export.csv',mirror:'retired/export.csv',archive:'archive/current.csv'}));
await mkdir(join(workspace,'archive'));await writeFile(join(workspace,'archive/current.csv'),'item,quantity,status\napples,17,confirmed\npears,25,confirmed\nunknown,,pending\n');
await withAcceptanceGateway({binary,output,workspace},async({socketPath})=>{
 const session='generic-persistence-'+Date.now(),events=[];
 const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:session}}).catch(()=>{}),180000);
 let terminal;
 try{
  for await(const event of messages(socketPath,{Turn:{session_id:session,user_request:'Produce result.json in this workspace with keys confirmed_quantity (total quantity of confirmed items), pending_count (number of pending items), and pending_quantity (null if not known). Use source-map.json to locate the source. The preferred export and its mirror were reported unavailable. Complete this small local task using the available evidence, preserve uncertainty about pending quantities, and check the saved result. Do not use network tools or contact anyone.',target_agent:null,workspace,interaction_mode:'execute',permission_mode:'full_access',journal:true}})){
   events.push(event);if(event.Done||event.Error){terminal=event;break;}
  }
 }finally{clearTimeout(timer);await writeFile(join(output,'events.json'),JSON.stringify(events,null,2));}
 if(!terminal?.Done)throw Error('Task did not complete: '+JSON.stringify(terminal));
 const result=JSON.parse(await readFile(join(workspace,'result.json'),'utf8'));
 await writeFile(join(output,'result.json'),JSON.stringify({terminal,artifact:result,limits:'A single isolated smoke check; not an A/B comparison or proof of universal persistence.'},null,2));
 if(!terminal?.Done)throw Error('Task did not complete');
 assert.equal(result.confirmed_quantity,42);assert.equal(result.pending_count,1);assert.equal(result.pending_quantity,null);
 console.log(JSON.stringify({artifact:result,terminal:'done'}));
});
