// Read full tool receipts rather than truncated journal previews. This measures
// requests and transport results, never infers task success from a green tool.
import {readFile, writeFile} from 'node:fs/promises';

export function auditComputerTranscript(session) {
  const calls = (session.messages || []).filter(m => m.type === 'ToolResult');
  const desktop = calls.filter(m => m.tool_name?.startsWith('computer_'));
  const batches = [];
  for (const [index, call] of desktop.entries()) {
    if (!['computer_act', 'computer_window_act'].includes(call.tool_name)) continue;
    let input;
    try {input = JSON.parse(call.input);} catch {throw Error(`Corrupt full batch input at desktop receipt ${index}`);}
    if (!Array.isArray(input.actions)) throw Error(`Missing actions at desktop receipt ${index}`);
    batches.push({receipt:index, tool:call.tool_name, requested_actions:input.actions.length,
      argument_bytes:Buffer.byteLength(call.input), transport_success:call.success === true,
      observation_requested:call.tool_name === 'computer_act' ? input.screenshot !== false : input.capture !== false,
      observation_saved:/Screenshot saved:/.test(call.output || ''),
      actions_by_type:input.actions.reduce((counts, action) => {
        counts[action.type] = (counts[action.type] || 0) + 1; return counts;
      }, {})});
  }
  const failed = desktop.filter(c => c.success === false);
  return {session_id:session.id, total_tool_receipts:calls.length,
    desktop_tool_receipts:desktop.length, failed_desktop_receipts:failed.length,
    observed_transport_error_fraction:desktop.length ? failed.length/desktop.length : null,
    requested_batch_actions:batches.reduce((n,b)=>n+b.requested_actions,0),
    executed_actions:null,
    executed_actions_note:'A failed batch can execute a prefix. Requested action counts and successful receipts do not establish delivered effects.',
    task_quality:'unverified — inspect saved artifacts and actual screens', batches,
    failures:failed.map(c=>({tool:c.tool_name, output:c.output}))};
}

if (process.argv[1]?.endsWith('/audit-computer-transcript.mjs')) {
  const [source, destination] = process.argv.slice(2);
  const report = auditComputerTranscript(JSON.parse(await readFile(source,'utf8')));
  await writeFile(destination, JSON.stringify(report,null,2), {flag:'wx',mode:0o600});
  console.log(JSON.stringify({session:report.session_id, tools:report.desktop_tool_receipts,
    failed:report.failed_desktop_receipts, requestedActions:report.requested_batch_actions,
    taskQuality:report.task_quality}));
}
