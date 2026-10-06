// Small real-provider acceptance: the owner must receive two delegated results
// and publish one useful final. Uses only a disposable company and access copies.
import {mkdir, writeFile, open, readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {withAcceptanceGateway, once, messages} from './lib/acceptance-gateway.mjs';

const [binary, output, mode] = process.argv.slice(2);
if(mode && mode!=='--cancel-followup')throw Error('Unknown acceptance mode');
if (![binary, output].every(path => path?.startsWith('/'))) throw Error('Absolute binary and NEW output directory required');
await mkdir(output, {mode:0o700});
const workspace = join(output, 'work'); await mkdir(workspace);
await writeFile(join(output, 'binary.json'), JSON.stringify({binary,
  sha256:createHash('sha256').update(await readFile(binary)).digest('hex')}, null, 2), {flag:'wx'});
await withAcceptanceGateway({binary, output, workspace, accountPool:true, preserveState:true}, async ({socketPath,home}) => {
  await once(socketPath, {Onboarding:{action:'choose_company',choice:'founding_company'}});
  const {CompanyDirectory:{directory}} = await once(socketPath, {CompanyDirectory:{action:'status'}});
  const owner = directory.agents.find(a => a.agent_id === 'phoenix');
  const peers = ['coder','critic'].map(role => directory.agents.find(a => a.internal_role === role));
  if (!owner || peers.some(a => !a)) throw Error('Expected owner and coworkers in disposable roster');
  const name = 'Owner return acceptance ' + Date.now();
  const created = await once(socketPath, {CompanyDirectory:{action:'create_group',name,
    description:'Small isolated invoice review; test owner return and final visibility.',color:'#688c9e',
    icon_seed:'owner-return',members:[owner,...peers].map(a=>a.agent_id),settings:{read_full_transcript:true}}});
  const group = created.CompanyDirectory?.directory.groups.find(g=>g.name===name);
  if (!group) throw Error('Created group missing; no turn submitted');
  const prompt = [
    `@${owner.display_name} Coordinate this short invoice review. You own the final answer.`,
    `Delegate the arithmetic to ${peers[0].display_name} and the duplicate-record review to ${peers[1].display_name} using talk. Receive both answers before giving me one concise combined result. Coworkers should report back to you.`,
    'Dataset: invoice A: 3 units at $40; invoice B: 2 units at $75; refund R against A: 1 unit at $40; repeated export row B: 2 units at $75 with the same invoice ID B.',
    'Count each invoice ID once; subtract refunds once. Arithmetic reviewer: calculate the net revenue. Duplicate reviewer: identify the duplicate and explain its effect if counted twice. Return evidence, not an acknowledgement.',
    'Everything needed is above. This is read-only analysis, not a build task. No web, files, credentials, extra delegation, schedules or user questions. Keep this small; do not create an artifact.',
  ].join('\n');
  const {GroupActivationPreview:preview} = await once(socketPath,{GroupActivationPreview:{group_id:group.group_id,user_request:prompt}});
  const {execution_wave_display_names,active_display_names,...activation} = preview;
  const turnId = 'owner-return-' + Date.now();
  await writeFile(join(output,'submission.json'),JSON.stringify({group,owner,peers,activation,turnId,prompt},null,2),{flag:'wx'});
  const log = await open(join(output,'events.jsonl'),'wx',0o600);
  let terminal, canceled=false; const stories=[];
  const timer=setTimeout(async()=>{canceled=true;try{await once(socketPath,{Cancel:{session_id:group.canonical_session_id}});}catch(error){console.error(String(error));}},5*60*1000);
  console.log(JSON.stringify({phase:'submitted',groupId:group.group_id,sessionId:group.canonical_session_id,turnId}));
  try {
    for await (const value of messages(socketPath,{Turn:{session_id:group.canonical_session_id,turn_id:turnId,
      user_request:prompt,workspace,interaction_mode:'execute',permission_mode:'full_access',
      target_group:group.group_id,group_activation:activation,journal:true,delivery:'queue'}})) {
      await log.write(JSON.stringify({at:new Date().toISOString(),value})+'\n');
      if(value.Story){stories.push(value.Story);console.log(JSON.stringify(value.Story));}
      if(mode==='--cancel-followup'&&!canceled&&stories.filter(s=>s.kind==='handoff').length===2){
        canceled=true;
        const ack=await once(socketPath,{Cancel:{session_id:group.canonical_session_id}});
        await writeFile(join(output,'cancel-ack.json'),JSON.stringify(ack,null,2),{flag:'wx'});
      }
      if(value.Done||value.Error){terminal=value;break;}
    }
    if(!terminal)throw Error('Stream ended without terminal receipt; do not resubmit');
    if(mode==='--cancel-followup'){
      const nextPrompt=`@${owner.display_name} The previous invoice task was stopped. Answer only this new question: what is 2 + 2? Do not resume the invoice review, delegate, use tools, or write files.`;
      const {GroupActivationPreview:nextPreview}=await once(socketPath,{GroupActivationPreview:{group_id:group.group_id,user_request:nextPrompt}});
      const {execution_wave_display_names,active_display_names,...nextActivation}=nextPreview;
      const nextStories=[];let nextTerminal;
      for await(const value of messages(socketPath,{Turn:{session_id:group.canonical_session_id,turn_id:turnId+'-followup',
        user_request:nextPrompt,workspace,interaction_mode:'execute',permission_mode:'full_access',
        target_group:group.group_id,group_activation:nextActivation,journal:true,delivery:'queue'}})){
        await log.write(JSON.stringify({at:new Date().toISOString(),phase:'followup',value})+'\n');
        if(value.Story){nextStories.push(value.Story);console.log(JSON.stringify(value.Story));}
        if(value.Done||value.Error){nextTerminal=value;break;}
      }
      const nextFinal=nextTerminal?.Done?.final_markdown||'';
      const checks={stopAcknowledged:canceled&&!!terminal.Error,
        noStoppedTaskAnswer:!stories.some(s=>s.kind==='group_message'),
        followupCompleted:!!nextTerminal?.Done,
        noOldCoworkerWork:!nextStories.some(s=>s.kind==='usage'&&s.agent&&!s.agent.startsWith(owner.display_name)),
        noOldReturns:!nextStories.some(s=>s.kind==='return'||s.kind==='handoff'),
        oneNewAnswer:nextStories.filter(s=>s.kind==='group_message').length===1,
        newQuestionOnly:nextFinal.includes('4')&&!/invoice|\$230|\$150/.test(nextFinal)};
      await writeFile(join(output,'result.json'),JSON.stringify({terminal,canceled,nextTerminal,checks},null,2),{flag:'wx'});
      console.log(JSON.stringify({phase:'terminal',checks}));
      if(!Object.values(checks).every(Boolean))process.exitCode=1;
      return;
    }
    const session=JSON.parse(await readFile(join(home,'sessions',group.canonical_session_id+'.json'),'utf8'));
    await writeFile(join(output,'session.json'),JSON.stringify(session,null,2),{flag:'wx',mode:0o600});
    const handoffs=stories.filter(s=>s.kind==='handoff'),returns=stories.filter(s=>s.kind==='return');
    const contributions=stories.filter(s=>s.kind==='group_message');
    const final=terminal.Done?.final_markdown?.replace(/^\*\*orchestrator\*\*\s*/, '').trim()||'';
    const checks={
      cleanTerminal:!!terminal.Done&&!canceled,
      exactlyTwoAssignments:handoffs.length===2,
      bothCoworkersAssigned:peers.every(peer=>handoffs.some(s=>s.receiver===peer.internal_role)),
      exactlyTwoCorrelatedReturns:returns.length===2&&handoffs.every(h=>returns.filter(r=>r.reply_to===h.handoff_id&&r.status==='done').length===1),
      oneOwnerPublication:contributions.length===1&&contributions[0].agent_id===owner.agent_id,
      finalMatchesSinglePublication:contributions.length===1&&final===contributions[0].markdown?.trim(),
      finalUsesBothFindings:final.includes('$230')&&final.includes('$150')&&/duplicat/i.test(final),
      publicationAfterBothReturns:returns.length===2&&contributions.length===1&&returns.every(r=>r.event_sequence<contributions[0].event_sequence),
    };
    await writeFile(join(output,'result.json'),JSON.stringify({terminal,canceled,checks,stories:stories.length,
      note:'Small live success case only. Cancellation, restart and failed-provider branches require separate evidence.'},null,2),{flag:'wx'});
    console.log(JSON.stringify({phase:'terminal',terminal,canceled,checks}));
    if(!Object.values(checks).every(Boolean))process.exitCode=1;
  }finally{clearTimeout(timer);await log.close();}
});
