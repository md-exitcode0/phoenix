// Audit durable full inputs, never the abbreviated Story.target display text.
export function auditBlenderTools(messages, observedCalls) {
  const results = messages.filter(message => message.type === 'ToolResult');
  // Runtime feedback records are not dispatched tool executions. Retain them
  // separately so admission failures are visible without corrupting call counts.
  const validations = results.filter(message => message.tool_name === 'response_validation');
  const receipts = results.filter(message => message.tool_name !== 'response_validation');
  // Learning a control or loading the runtime's mandatory design contract is
  // not scene construction. Keep actual code/terminal/MCP execution excluded.
  const allowed = new Set(['todo_write', 'image_analyze', 'recall', 'final_answer',
    'design_reference', 'skill_search', 'skill_install', 'skill', 'read', 'list_directory']);
  let malformedInputs = 0, prohibitedConsole = false;
  for (const receipt of receipts) {
    let input;
    try { input = JSON.parse(receipt.input); }
    catch { malformedInputs++; continue; }
    if (!input || typeof input !== 'object' || Array.isArray(input)) {
      malformedInputs++; continue;
    }
    const actions = Array.isArray(input.actions) ? input.actions : [input];
    for (const action of actions) {
      const keys = String(action?.combo || '').toLowerCase().split('+').map(key => key.trim());
      if (keys.includes('shift') && keys.includes('f4')) prohibitedConsole = true;
    }
  }
  const complete = receipts.length === observedCalls && malformedInputs === 0;
  return {
    receipts,
    validations,
    audit: {
      source: 'durable executed-tool ToolResult inputs',
      validation_events: validations.length,
      failed_validation_events: validations.filter(message => message.success === false).length,
      complete, receipt_count: receipts.length, observed_calls: observedCalls,
      malformed_inputs: malformedInputs,
      all_tools_allowed: complete ? receipts.every(receipt =>
        receipt.tool_name?.startsWith('computer_') || allowed.has(receipt.tool_name) ||
        (receipt.tool_name === 'work' && JSON.parse(receipt.input).action === 'workflow')) : null,
      prohibited_console_shortcut: prohibitedConsole ? true : complete ? false : null,
      // Shortcut/tool checks cannot prove that typed content never invoked a
      // console via menus. Full input and screen review is still required.
      ui_only_visual_review: 'required',
    },
  };
}

// A resumed acceptance carries historical receipts for model continuity. Only
// the exact new user turn belongs in its execution metrics. Missing boundaries
// fail closed rather than silently reporting an incomplete or mixed audit.
export function currentBlenderTurn(messages, prompt) {
  const start = messages.findLastIndex(message => message.type === 'User' && message.content === prompt);
  if (start < 0) throw Error('Current turn boundary is missing; do not mix prior tool receipts into this audit');
  return messages.slice(start + 1);
}

// Plain native continuations keep the original task and actor history. Never
// reinterpret a guided/scripted trial or an unconfirmed writer as this test.
export function plainNativeContinuation({execution,setup,metrics}, sceneNames) {
  if(execution?.gateway?.code!==0||execution.credentialRemoved!==true||!execution.durableState
    ||execution.error||execution.snapshotError||execution.cleanupFailures?.length
    ||!metrics?.terminal||metrics.terminal.Done?.background_work_pending===true)
    throw Error('Native continuation requires a clean terminal snapshot and no active work');
  if(setup?.mode!=='plain-reference-native-ui'||setup.nativeGuiRequired!==true||setup.observerCoaching!==false)
    throw Error('Guided or non-native history cannot enter the plain native benchmark');
  const originalPrompt=setup.originalPrompt||setup.prompt;
  if(typeof originalPrompt!=='string'||!originalPrompt.trim())throw Error('Original native task is missing');
  const scenes=sceneNames.filter(name=>typeof name==='string'&&name.endsWith('.blend')&&!/[\\/\0]/.test(name));
  const sceneName=scenes.includes('banana.blend')?'banana.blend':scenes.length===1?scenes[0]:null;
  if(!sceneName)throw Error('Native continuation needs one unambiguous saved draft; no scene guessed');
  return {originalPrompt,sceneName,prompt:'Continue the existing task to completion using its saved work and original requirements.'};
}

// Process success and a saved draft cannot make a quota-stopped task pass.
// These are execution/method/delivery checks, never visual acceptance.
export function nativeCompletionChecks(metrics) {
  return {
    completed:metrics.terminal?.Done?.completion==='completed'&&metrics.cancelSent===false,
    completeInputAudit:metrics.tool_input_audit_complete===true,
    allowedTools:metrics.all_tools_allowed===true,
    noConsoleShortcut:metrics.prohibited_console_shortcut===false,
    expectedScenePresent:metrics.blend_bytes>0,
    expectedRenderPresent:metrics.preview_bytes>0,
  };
}
