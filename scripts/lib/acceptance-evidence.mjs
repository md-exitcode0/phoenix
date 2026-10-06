// Select an actual, successful contribution returned to this request's owner.
// Tool transport or a matching name alone cannot establish a handoff result.
export function peerResult(stories, turnId, ownerId, peerId) {
  for (const handoff of stories.filter(row=>row.kind==='handoff'&&row.requester===ownerId&&row.receiver===peerId
    &&typeof row.handoff_id==='string'&&row.handoff_id.length>0&&row.execution?.turn_id===turnId)) {
    const returned=stories.filter(row=>row.kind==='return'&&row.reply_to===handoff.handoff_id);
    if(returned.length!==1)continue;
    const row=returned[0];
    if(row.ok===true&&row.status==='done'&&row.requester===ownerId&&row.receiver===peerId
      &&row.execution?.turn_id===turnId&&row.execution?.task_id===handoff.execution.task_id
      &&row.execution?.attempt_id===handoff.execution.attempt_id&&typeof row.body==='string'&&row.body.trim()
      &&Number.isFinite(row.event_sequence)&&row.event_sequence>handoff.event_sequence)return row;
  }
  return null;
}
