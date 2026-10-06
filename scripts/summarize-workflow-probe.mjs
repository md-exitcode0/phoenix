// Read-only comparison of saved synthetic live probe evidence; never submit work.
import {readFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {citesLine} from './source-citation.mjs';
for(const argument of process.argv.slice(2)) {
    const output=resolve(argument);
    const execution=JSON.parse(await readFile(join(output,'execution.json'),'utf8'));
    if(!/^\/tmp\/phoenix-group-overlap-[A-Za-z0-9]+$/.test(execution.home))throw Error('Not an isolated probe home');
    const log=await readFile(join(output,'process.log'),'utf8');
    const rows=(await readFile(join(output,'experiment','gateway-events.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
    const submitted=rows.find(row=>row.value.Submission);
    const stories=rows.filter(row=>row.value.Story);
    const tools=stories.filter(row=>row.value.Story.kind==='tool');
    const finals=stories.filter(row=>row.value.Story.kind==='group_message');
    const checks=rows.find(row=>row.value.Checks)?.value.Checks;
    const elapsed=log.match(/turn done .* in ([\d.]+)s \((\d+) tokens/);
    const canonical=JSON.parse(await readFile(join(execution.home,'sessions',submitted.value.Submission.group.canonical_session_id+'.json'),'utf8'));
    const finalPersistedExactly=finals.length===1&&canonical.messages.filter(message=>message.type==='GroupContribution'
        &&message.message_id===finals[0].value.Story.message_id&&message.body===finals[0].value.Story.markdown).length===1;
    const delay=kind=>{
        const event=stories.find(row=>kind==='read'?row.value.Story.kind==='tool'&&row.value.Story.tool==='read':row.value.Story.kind===kind);
        return event?Date.parse(event.at)-Date.parse(submitted.at):null;
    };
    console.log(JSON.stringify({output,originalExitCode:execution.code,elapsedSeconds:elapsed?Number(elapsed[1]):null,
        cumulativeTokens:elapsed?Number(elapsed[2]):null,toolCount:tools.length,
        tools:tools.reduce((counts,row)=>{const name=row.value.Story.tool;counts[name]=(counts[name]||0)+1;return counts;},{}),
        handoffAfterMs:delay('handoff'),readAfterMs:delay('read'),finalPersistedExactly,
        citationIncludesRequiredLine:finals.length===1&&citesLine(finals[0].value.Story.markdown,'validate_blend.py',61),checks}));
}
