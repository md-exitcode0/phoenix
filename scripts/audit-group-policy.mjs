import {readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';

export function auditGroupPolicy(rows,activation) {
  const stories=rows.map(row=>row.value?.Story).filter(Boolean);
  const violations=[],tools=[];
  for(const story of stories.filter(story=>story.kind==='tool')) {
    const role=/\(([^()]+)\)$/.exec(story.agent||'')?.[1];
    const allowed=activation.tool_constraints?.[role];
    const permitted=Array.isArray(allowed)&&allowed.some(pattern=>pattern.endsWith('*')
      ? story.tool.startsWith(pattern.slice(0,-1)):story.tool===pattern);
    const row={agent:role||story.agent,tool:story.tool,ok:story.ok,permitted};
    tools.push(row);if(!permitted)violations.push(row);
  }
  const contributions=Object.fromEntries(activation.active_agent_ids.map(id=>[id,
    stories.filter(s=>s.kind==='group_message'&&s.agent_id===id&&s.markdown?.trim()).length]));
  return {executed_tool_events:tools.length,violations,tools,contributions,
    limits:'Audits executed tool names and published contributions. It does not prove shell-command side effects or artifact quality.'};
}

if(process.argv[1]?.endsWith('/audit-group-policy.mjs')) {
  const [directory,destination]=process.argv.slice(2);
  const rows=(await readFile(join(directory,'events.jsonl'),'utf8')).trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
  const activation=JSON.parse(await readFile(join(directory,'constrained-activation.json'),'utf8'));
  const result=auditGroupPolicy(rows,activation);
  await writeFile(destination,JSON.stringify(result,null,2),{flag:'wx'});
  console.log(JSON.stringify({executed:result.executed_tool_events,violations:result.violations,contributions:result.contributions}));
  if(result.violations.length)process.exitCode=1;
}
