import {isolatedTestHome} from './lib/isolated-test-home.mjs';
// Focused live-agent source-evidence acceptance; never resubmits on timeout.
import net from 'node:net';
import {mkdir,open,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {homedir} from 'node:os';
const phoenixHome=isolatedTestHome();
const [output,source]=process.argv.slice(2);
if(![output,source].every(p=>p?.startsWith('/')))throw Error('Expected absolute directories');
await mkdir(output,{recursive:true});
const log=await open(join(output,'gateway-events.jsonl'),'wx',0o600);
const record=async value=>log.write(JSON.stringify({at:new Date().toISOString(),value})+'\n');
async function* request(body){
  const socket=net.createConnection(join(phoenixHome,'gateway.sock'));
  socket.on('connect',()=>socket.write(JSON.stringify(body)+'\n'));
  let pending='';
  try{for await(const chunk of socket){pending+=chunk;let split;while((split=pending.indexOf('\n'))>=0){const line=pending.slice(0,split);pending=pending.slice(split+1);if(line.trim())yield JSON.parse(line);}}}finally{socket.destroy();}
}
async function once(body){for await(const value of request(body)){if(value.Error)throw Error(JSON.stringify(value.Error));return value;}throw Error('No response; inspect receipts before retry');}
const name='Citation audit acceptance '+Date.now();
const created=await once({CompanyDirectory:{action:'create_group',name,description:'Read-only exact source citation acceptance.',color:'#688c9e',icon_seed:'citation-audit',members:['coder','critic'],settings:{read_full_transcript:true}}});
const group=created.CompanyDirectory.directory.groups.find(g=>g.name===name);
const prompt=[
  '@Leo audit the verifier first, then @Remy independently validate the source citations and the conclusion.',
  'Read-only offline review. Source directory: '+source+'. Read only validate_blend.py. No file changes, network, credentials, questions, goals, routines or extra peer delegation.',
  'Leo: identify the exact expression for the height requirement and whether it uses all product bounds or a narrower datum. Identify where the global bounds are reported and where the final boolean selects the height check. Quote the actual expressions and cite exact file:line locations, not estimated line numbers. Use read with line_numbers=true to inspect and cite source in one call. Do not count display/header lines as source lines. Keep the report under 220 words.',
  'Remy: verify Leo’s specific quotations and line references directly against the file using one numbered read, then give a concise corrected or confirmed verdict with the exact references. Never repeat an unverified line number. Do not rerun geometry or claim the model was repaired.',
].join('\n');
const {GroupActivationPreview:preview}=await once({GroupActivationPreview:{group_id:group.group_id,user_request:prompt}});
await record({Preview:preview});
if(JSON.stringify(preview.execution_dependencies)!==JSON.stringify([{prerequisite:'coder',dependent:'critic'}]))throw Error('Unexpected activation');
const {execution_wave_display_names,active_display_names,...activation}=preview;
const turn_id='citation-audit-'+Date.now();
await record({Submission:{groupId:group.group_id,sessionId:group.canonical_session_id,turn_id,prompt}});
console.log(JSON.stringify({groupId:group.group_id,sessionId:group.canonical_session_id,turn_id}));
const stories=[];let terminal=false;
for await(const value of request({Turn:{session_id:group.canonical_session_id,turn_id,user_request:prompt,workspace:source,interaction_mode:'execute',permission_mode:'full_access',journal:true,target_group:group.group_id,group_activation:activation,delivery:'queue'}})){
  await record(value);
  if(value.Story){stories.push(value.Story);if(['group_message','group_member_status'].includes(value.Story.kind))console.log(JSON.stringify(value.Story));}
  if(value.Error)throw Error(JSON.stringify(value.Error));
  if(value.Done){terminal=true;break;}
}
if(!terminal)throw Error('No terminal receipt; do not resubmit');
const lines=(await readFile(join(source,'validate_blend.py'),'utf8')).split('\n');
const expected=["'overall_product_bounds_mm':","'design_datum_height_mm':","verification['measured_checks']['design_datum_height_mm']['pass']"].map(text=>lines.findIndex(line=>line.includes(text))+1);
if(expected.some(n=>n<=0))throw Error('Missing reference expression in source');
const contributions=stories.filter(s=>s.kind==='group_message');
const checks={oneContributionPerAgent:['coder','critic'].every(id=>contributions.filter(s=>s.agent_id===id).length===1),
  eachReadOnce:['Leo (coder)','Remy (critic)'].every(agent=>stories.filter(s=>s.kind==='tool'&&s.tool==='read'&&s.agent===agent).length===1),
  reviewerCitesAllExpectedLines:expected.every(line=>new RegExp('validate_blend\\.py:'+line+'(?![0-9])').test(contributions.find(s=>s.agent_id==='critic')?.markdown||'')),
  noFailedTools:!stories.some(s=>s.kind==='tool'&&!s.ok)};
await record({Checks:checks,expectedLines:expected});console.log(JSON.stringify({checks,expectedLines:expected}));await log.close();
if(!Object.values(checks).every(Boolean))process.exitCode=1;
