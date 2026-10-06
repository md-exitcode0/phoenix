import {isolatedTestHome} from './lib/isolated-test-home.mjs';
// Real provider-backed group acceptance. No restart or automatic resubmission.
import net from 'node:net';
import {mkdir, open} from 'node:fs/promises';
import {homedir} from 'node:os';
import {join} from 'node:path';
const phoenixHome=isolatedTestHome();
const [output,source,blender,existingGroupId]=process.argv.slice(2);
if(![output,source,blender].every(path=>path?.startsWith('/')))throw new Error('Expected absolute paths');
await mkdir(output,{recursive:true});
const log=await open(join(output,'gateway-events.jsonl'),'wx',0o600);
const record=async value=>log.write(JSON.stringify({at:new Date().toISOString(),value})+'\n');
async function* request(body) {
  const socket=net.createConnection(join(phoenixHome,'gateway.sock'));
  socket.on('connect',()=>socket.write(JSON.stringify(body)+'\n'));
  let pending='';
  try {for await(const chunk of socket) {
    pending+=chunk;let split;
    while((split=pending.indexOf('\n'))>=0) {
      const line=pending.slice(0,split);pending=pending.slice(split+1);
      if(line.trim())yield JSON.parse(line);
    }
  }} finally {socket.destroy();}
}
async function once(body) {
  for await(const value of request(body)) {
    await record(value);
    if(value.Error)throw new Error(JSON.stringify(value.Error));
    return value;
  }
  throw new Error('No response; inspect saved receipts before retrying.');
}
const name='Native review acceptance '+Date.now();
const created=await once({CompanyDirectory:existingGroupId?{action:'status'}:{action:'create_group',name,
  description:'Read-only live acceptance: geometry inspection, verifier audit, evidence reconciliation.',
  color:'#688c9e',icon_seed:'native-review',members:['researcher','coder','critic'],settings:{read_full_transcript:true}}});
const group=created.CompanyDirectory?.directory.groups.find(group=>existingGroupId?group.group_id===existingGroupId:group.name===name);
if(!group)throw new Error('Created group missing from response');
console.log(JSON.stringify({groupId:group.group_id,sessionId:group.canonical_session_id}));
const prompt=[
  '@Theo and @Leo work independently in parallel; finally @Remy reconcile their saved contributions.',
  'Real-world offline artifact acceptance. Source directory: '+source+'. Blender executable: '+blender+'.',
  'The original task requires a polished headphone stand with a rounded base 140 x 110 x 12 mm, total product height 240 mm, curved padded saddle 75 mm wide, a recessed cable channel, graphite/cork/brass materials, and physically connected parts with no floating geometry. Required files are headphone-stand.blend, preview.png (1200 x 1000), and verification.json.',
  'This is READ-ONLY. Do not change, create, delete or regenerate any artifact, install anything, browse websites, access credentials, contact external services, or ask the user questions. Read only the three artifacts and build_headphone_stand.py and validate_blend.py in the source directory. Read-only Blender subprocess inspection using --background and --python-expr is allowed. Do not rerender. Do not start peers or create goals/routines. Each person performs only the assigned responsibility and publishes one substantive group contribution with evidence, not an acknowledgement.',
  'Theo: independently reopen the saved blend and inspect actual evaluated geometry for overall dimensions, saddle contact/support, and cable recess. Evaluate physical contact rather than trusting bounding-box intersection. State measurements and failures; do not edit a verifier.',
  'Leo: inspect the validation code and verification.json. Check whether each claimed pass proves the original requirement or substitutes another metric. You own the code/rubric audit, not geometric measurement. Give concrete code-level evidence without repeating the measurement task.',
  'Remy: read both saved contributions, reconcile disagreement, and issue a concise final pass/fail decision tied to the unchanged original requirements. You may read the same artifacts to resolve a specific disputed claim. Do not trust a green summary over underlying evidence. No repairs in this review task.',
].join('\n');
const response=await once({GroupActivationPreview:{group_id:group.group_id,user_request:prompt}});
const preview=response.GroupActivationPreview;
const actual=preview?.execution_dependencies?.map(edge=>edge.prerequisite+'>'+edge.dependent).sort();
if(JSON.stringify(actual)!==JSON.stringify(['coder>critic','researcher>critic']))throw new Error('Unexpected dependency preview: '+JSON.stringify(preview));
console.log(JSON.stringify({preview}));
const {execution_wave_display_names,active_display_names,...activation}=preview;
const turnId='group-native-review-'+Date.now();
await record({Submission:{turnId,groupId:group.group_id,sessionId:group.canonical_session_id}});
let terminal=false,terminalError=false;
const stories=[];
for await(const value of request({Turn:{session_id:group.canonical_session_id,turn_id:turnId,user_request:prompt,
  interaction_mode:'execute',permission_mode:'full_access',workspace:source,journal:true,target_agent:null,
  target_group:group.group_id,group_activation:activation,delivery:'queue'}})) {
  await record(value);
  if(value.Story){stories.push(value.Story);console.log(JSON.stringify(value.Story));}
  if(value.Done||value.Error){console.log(JSON.stringify(value));terminal=true;terminalError=!!value.Error;break;}
}
await log.close();
if(!terminal)throw new Error('No terminal receipt; inspect the saved group before retrying.');
const participants=['researcher','coder','critic'];
const start=id=>stories.findIndex(s=>s.kind==='group_member_status'&&s.agent_id===id&&s.state==='working');
const contribution=id=>stories.findIndex(s=>s.kind==='group_message'&&s.agent_id===id);
const checks={
  terminalSuccess:!terminalError,
  oneContributionEach:participants.every(id=>stories.filter(s=>s.kind==='group_message'&&s.agent_id===id&&s.markdown?.trim()).length===1),
  actualStarts:participants.every(id=>start(id)>=0),
  independentBranchesOverlap:Math.max(start('researcher'),start('coder'))<Math.min(contribution('researcher'),contribution('coder'))&&start('researcher')>=0&&start('coder')>=0,
  joinStartsAfterBothContributions:start('critic')>Math.max(contribution('researcher'),contribution('coder'))&&contribution('researcher')>=0&&contribution('coder')>=0,
  noDuplicateRelayAnswer:!stories.some(s=>s.kind==='answer'&&s.markdown?.trim()),
};
console.log(JSON.stringify({checks,passed:Object.values(checks).every(Boolean),note:'These checks measure scheduling and publication, not correctness of the reviewers conclusions.'}));
if(!Object.values(checks).every(Boolean))process.exitCode=1;
