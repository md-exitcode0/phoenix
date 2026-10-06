import assert from 'node:assert/strict';
import {auditComputerTranscript} from './audit-computer-transcript.mjs';
const result = auditComputerTranscript({id:'fixture',messages:[
  {type:'ToolResult',tool_name:'computer_act',input:JSON.stringify({actions:[{type:'key',combo:'a'},{type:'type',text:'é'}]}),success:false,output:'step 2 failed'},
  {type:'ToolResult',tool_name:'computer_act',input:JSON.stringify({actions:[{type:'key',combo:'esc'}],screenshot:true}),success:true,output:'end-of-batch screenshot unavailable'},
  {type:'ToolResult',tool_name:'read',success:true,output:'unrelated'},
]});
assert.equal(result.desktop_tool_receipts,2);
assert.equal(result.failed_desktop_receipts,1);
assert.equal(result.requested_batch_actions,3);
assert.equal(result.executed_actions,null);
assert.equal(result.batches[1].observation_saved,false);
assert.equal(result.batches[1].observation_requested,true);
assert.equal(auditComputerTranscript({messages:[]}).observed_transport_error_fraction,null);
assert.throws(()=>auditComputerTranscript({messages:[{type:'ToolResult',tool_name:'computer_act',input:'broken'}]}),/Corrupt/);
console.log('COMPUTER_AUDIT_OK: partial failures and missing observations remain explicit');
