// A real agent must inspect changing canvas pixels in an unopened browser pane.
import http from 'node:http';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {withAcceptanceGateway,once,messages} from './lib/acceptance-gateway.mjs';
import {acceptanceBrowser} from './lib/acceptance-browser.mjs';
const [binary,output]=process.argv.slice(2);
if(![binary,output].every(path=>path?.startsWith('/')))throw Error('Absolute binary and NEW output required');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
const server=http.createServer((_req,res)=>{
  res.setHeader('content-type','text/html; charset=utf-8');
  res.end(`<!doctype html><meta charset="utf-8"><title>Picture observation</title>
<style>body{margin:32px;font:20px system-ui;background:#faf9f5}canvas{display:block;border:1px solid #333;max-width:100%}button{font:inherit;margin-top:24px;padding:12px 18px}</style>
<h1>Picture observation</h1><canvas width="640" height="360" aria-label="Test picture"></canvas><button>Change panel</button>
<script>const c=document.querySelector('canvas').getContext('2d');function paint(next){c.fillStyle=next?'#ce5b3c':'#2f7064';c.fillRect(0,0,640,360);c.fillStyle='#fff';c.font='bold 52px sans-serif';c.fillText(next?'CORAL 83':'MOSS 47',75,202);}paint(false);document.querySelector('button').onclick=()=>paint(true);</script>`);
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
try{
  await withAcceptanceGateway({binary,output,workspace,authSource:'codex',preserveState:true,
    setup:args=>acceptanceBrowser({...args,output})},async({home,socketPath})=>{
    await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
    const actor=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory.agents.find(a=>a.agent_id==='coder');
    const prompt=`Open http://127.0.0.1:${server.address().port}/ in your browser. Read the code shown in the picture, click Change panel, and read the updated picture. Use browser screenshots for both observations; do not read the source or extract the code using JavaScript. Report the exact before and after codes.`;
    await writeFile(join(output,'prompt.txt'),prompt);
    const start=Date.now(),events=[];let terminal;
    try{
      for await(const frame of messages(socketPath,{Turn:{session_id:actor.canonical_session_id,turn_id:'pixel-observation-'+Date.now(),
        user_request:prompt,workspace,target_agent:actor.agent_id,permission_mode:'full_access',interaction_mode:'execute',journal:true,delivery:'queue'}},
        {signal:AbortSignal.timeout(180000)})){
        events.push({elapsedMs:Date.now()-start,frame});
        if(frame.Done||frame.Error){terminal=frame;break;}
      }
      const session=JSON.parse(await readFile(join(home,'sessions',actor.canonical_session_id+'.json')));
      const receipts=session.messages.filter(row=>row.type==='ToolResult'&&row.tool_name!=='response_validation');
      const captures=receipts.filter(row=>row.tool_name==='browser_screenshot'&&row.success);
      const answer=terminal?.Done?.final_markdown||'';
      const checks={completed:terminal?.Done?.completion==='completed',bothCodes:answer.includes('MOSS 47')&&answer.includes('CORAL 83'),
        twoNativeCaptures:captures.length===2,noCaptureFailure:!receipts.some(row=>row.tool_name==='browser_screenshot'&&!row.success),
        noSourceExtraction:!receipts.some(row=>['read','bash','browser_execute_js','browser_evaluate','web_fetch'].includes(row.tool_name))};
      for(let index=0;index<captures.length;index++){
        const path=captures[index].output.split('\n').find(line=>line.startsWith('Screenshot saved: '))?.slice('Screenshot saved: '.length);
        if(path)await writeFile(join(output,`picture-${index+1}.png`),await readFile(path));
      }
      await writeFile(join(output,'events.json'),JSON.stringify(events,null,2));
      const result={checks,elapsedMs:Date.now()-start,answer,toolCalls:receipts.length,failedCalls:receipts.filter(row=>!row.success).length,captures};
      await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));
      console.log(JSON.stringify({checks,elapsedMs:result.elapsedMs,answer,toolCalls:result.toolCalls}));
      if(Object.values(checks).some(value=>!value))process.exitCode=1;
    }finally{if(!terminal)await once(socketPath,{Cancel:{session_id:actor.canonical_session_id}}).catch(()=>{});}
  });
}finally{await new Promise(resolve=>server.close(resolve));}
