// Real Sol-medium screen reading and grounding, with no separate vision model.
import {mkdir,writeFile,readFile,copyFile,open} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {randomBytes,createHash} from 'node:crypto';
import {withAcceptanceGateway,once,messages} from './lib/acceptance-gateway.mjs';
const [binary,output]=process.argv.slice(2);
if(![binary,output].every(p=>p?.startsWith('/')))throw Error('Absolute binary and fresh output directory required');
await mkdir(output,{mode:0o700});
const workspace=join(output,'work');await mkdir(workspace);
const code=randomBytes(4).toString('hex').toUpperCase();
await writeFile(join(output,'binary.json'),JSON.stringify({binary,sha256:createHash('sha256').update(await readFile(binary)).digest('hex'),model:'gpt-5.6-sol',effort:'medium'},null,2),{flag:'wx'});
await withAcceptanceGateway({binary,output,workspace,setup:async({env})=>{
  env.PHOENIX_DESKTOP_BACKEND='gnome';env.PHOENIX_PERCEPTION_CODE=code;
  const apps=join(env.XDG_DATA_HOME,'applications');await mkdir(apps,{recursive:true});
  const fixture=resolve('scripts/native-perception-fixture.py');
  if(/[\r\n"`$\\]/.test(fixture))throw Error('Unsupported fixture path');
  await writeFile(join(apps,'phoenix-perception-check.desktop'),`[Desktop Entry]\nType=Application\nName=Phoenix Perception Check\nExec=/usr/bin/python3 "${fixture}"\nTerminal=false\n`,{flag:'wx'});
}},async({socketPath,home})=>{
  await once(socketPath,{Onboarding:{action:'choose_company',choice:'founding_company'}});
  const directory=(await once(socketPath,{CompanyDirectory:{action:'status'}})).CompanyDirectory.directory;
  const actor=directory.agents.find(a=>a.internal_role==='coder');
  if(!actor?.canonical_session_id)throw Error('No coder conversation');
  const models=(await once(socketPath,{Settings:{action:'models_snapshot'}})).Settings.snapshot;
  await writeFile(join(output,'models.json'),JSON.stringify(models,null,2));
  for(const lane of models.lanes.filter(lane=>['phoenix','specialist','coder'].includes(lane.lane)&&lane.configured!==false)) {
    if(lane.model!=='gpt-5.6-sol'||lane.reasoning_effort!=='medium')throw Error('Acceptance lane is not Sol medium');
  }
  const prompt='Open Phoenix Perception Check in your private desktop. Use computer_read_text to read its verification code. On a separate next round, use computer_locate to find the Confirm sample button. Inspect both tools’ images before answering. Return the exact visible verification code and the button center in screen pixels. Do not click the button. Allowed tools: computer_open, computer_list_windows, computer_focus_window, computer_wait, computer_read_text, computer_locate, final_answer. Do not use other tools, filesystem inspection, shell, Python, APIs, delegation, or a browser. Do not guess the code or claim grounding before seeing pixels. No user questions.';
  const sessionId=actor.canonical_session_id,turnId='native-perception-'+Date.now();
  await writeFile(join(output,'submission.json'),JSON.stringify({sessionId,turnId,prompt},null,2),{flag:'wx'});
  const log=await open(join(output,'events.jsonl'),'wx');let terminal;
  const timer=setTimeout(()=>once(socketPath,{Cancel:{session_id:sessionId,target_agent:actor.agent_id}}).catch(()=>{}),5*60*1000);
  try {
    for await(const value of messages(socketPath,{Turn:{session_id:sessionId,turn_id:turnId,user_request:prompt,workspace,target_agent:actor.agent_id,interaction_mode:'execute',permission_mode:'full_access',journal:true,delivery:'queue'}})){
      await log.write(JSON.stringify(value)+'\n');
      if(value.Story&&['tool','answer'].includes(value.Story.kind))console.log(JSON.stringify(value.Story));
      if(value.Done||value.Error){terminal=value;break;}
    }
  } finally {clearTimeout(timer);await log.close();}
  if(!terminal)throw Error('Missing terminal receipt; inspect before retrying');
  const sessionPath=join(home,'sessions',sessionId+'.json');
  const session=JSON.parse(await readFile(sessionPath,'utf8'));
  await copyFile(sessionPath,join(output,'session.json'));
  const toolResults=session.messages.filter(m=>m.type==='ToolResult');
  await mkdir(join(output,'screens'));
  for(const [index,result] of toolResults.entries()) {
    const path=result.output.split('\n').find(line=>line.startsWith('Screenshot saved: '))?.slice('Screenshot saved: '.length);
    if(path?.startsWith(home+'/'))await copyFile(path,join(output,'screens',`${index}-${result.tool_name}.png`));
  }
  const allowed=new Set(['computer_open','computer_list_windows','computer_focus_window','computer_wait','computer_read_text','computer_locate','final_answer']);
  const assistant=session.messages.filter(m=>m.type==='Assistant').map(m=>m.content||'').join('\n');
  const checks={terminal:!!terminal.Done,codeRead:assistant.includes(code),allowedToolsOnly:toolResults.every(m=>allowed.has(m.tool_name)),
    textImage:toolResults.some(m=>m.tool_name==='computer_read_text'&&m.success&&m.output.includes('Native screen reading:')),
    locateImage:toolResults.some(m=>m.tool_name==='computer_locate'&&m.success&&m.output.includes('Native visual grounding:')),
    noMissingVision:!toolResults.some(m=>m.output.includes('no vision model configured'))};
  await writeFile(join(output,'result.json'),JSON.stringify({checks,terminal,expectedCode:code,limits:'Button coordinates require independent screenshot inspection.'},null,2));
  console.log(JSON.stringify({checks,output}));
  if(!Object.values(checks).every(Boolean))process.exitCode=1;
});
