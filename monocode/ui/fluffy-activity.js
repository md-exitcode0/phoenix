/* Per-agent activity reduction. Structured live inputs only. */
(function(root){
 'use strict';
 const canonical=id=>id==='orchestrator'?'phoenix':String(id||'');
 const browserTools=new Set('browser_act browser_click browser_close browser_console browser_dropdown_options browser_evaluate browser_extract browser_find_elements browser_find_text browser_go_back browser_input browser_input_credential browser_navigate browser_save_as_pdf browser_download browser_screenshot browser_scroll browser_search browser_search_page browser_select_dropdown browser_send_keys browser_status browser_state browser_upload_file browser_import_cookies browser_switch browser_wait'.split(' '));
 const codingTools=new Set('read write str_replace grep glob list_directory codebase_search index_codebase symbol_search file_symbols callers callees impact call_path bash exec command run_command execute_command apply_patch shell terminal'.split(' '));
 const category=tool=>{const name=String(tool||'').split('.').at(-1);return browserTools.has(name)?'browsing':codingTools.has(name)?'coding':null};
 const scopeKey=e=>e?.turn_id&&e?.task_id&&e?.attempt_id?JSON.stringify([e.turn_id,e.task_id,e.attempt_id]):'';
 const waitingStatus=s=>['waiting_user','waiting_peer','queued','waiting','provider_wait'].includes(s);
 const runningStatus=s=>['working','running','browsing','coding','thinking','reviewing','reasoning','using_tool'].includes(s);
 function createStore(){
  const entries=new Map(),listeners=new Set(),seenRegistryEvents=new WeakSet();let dead=false;
  function entry(id){id=canonical(id);if(!entries.has(id))entries.set(id,{agentId:id,sessionId:'',key:'',execution:null,sequence:-1,mode:'idle',reason:'idle',terminal:false,retired:new Set(),tools:new Map(),completedCalls:new Set(),waiting:null,registryStatus:'',registryTurn:'',registrySequence:-1,registryTerminal:false,retiredRegistryTurns:new Set(),retiredSessions:new Set(),revision:0});return entries.get(id)}
  const snapshot=id=>{const a=entry(id);return{agentId:a.agentId,sessionId:a.sessionId,execution:a.execution?{...a.execution}:null,mode:a.mode,reason:a.reason,terminal:a.terminal||a.registryTerminal,revision:a.revision,pendingTools:a.tools.size,sequence:a.sequence,waitingReason:a.waiting,registryTurnId:a.registryTurn||null,registrySequence:a.registrySequence}};
  function emit(a,mode,reason){if(a.mode===mode&&a.reason===reason)return false;a.mode=mode;a.reason=reason;a.revision++;for(const f of listeners)f(snapshot(a.agentId));return true}
  function resetTools(a){a.tools.clear();a.completedCalls.clear()}
  function wait(a,reason){a.waiting=reason;return emit(a,'waiting',reason)}
  function holdWaiting(a){return a.waiting?emit(a,'waiting',a.waiting):false}
  function activeTool(a){return[...a.tools.values()].at(-1)}
  function toolEdge(a,kind,event,reasonPrefix=''){
   const mode=category(event.tool),callId=event.call_id||event.tool_call_id,key=callId?'call:'+callId:JSON.stringify([event.tool,event.target]);
   if(callId&&a.completedCalls.has(key))return false;
   if(kind==='tool_start'){
    if(!mode)return false;
    a.tools.set(key,mode);
    return a.waiting?holdWaiting(a):emit(a,activeTool(a),reasonPrefix+'tool_started');
   }
   a.tools.delete(key);if(callId)a.completedCalls.add(key);
   if(a.waiting)return holdWaiting(a);
   if(event.ok===false)return emit(a,'error',reasonPrefix+'tool_error');
   const active=activeTool(a);
   if(active)return emit(a,active,reasonPrefix+'tool_running');
   return mode?emit(a,mode,reasonPrefix+'tool_completed'):false;
  }
  function terminal(a,mode,reason,native=false){
   // Ending the model turn while its ask is pending does not answer the ask.
   if(mode==='success'&&a.waiting)return holdWaiting(a);
   a.tools.clear();a.waiting=null;if(native)a.registryTerminal=true;else a.terminal=true;
   return emit(a,mode,reason);
  }
  function begin(input){
   const {agentId,sessionId,execution,queued=false}=input;
   if(dead||input.replay||input.historical||input.source==='history'||!canonical(agentId)||!scopeKey(execution))return false;
   const a=entry(agentId),key=scopeKey(execution),session=String(sessionId||'');
   if(!session||a.retired.has(key)||a.retiredSessions.has(session))return false;
   if(a.key===key)return !a.terminal&&a.sessionId===session;
   if(a.sessionId===session&&input.sequence!=null&&Number(input.sequence)<=a.sequence)return false;
   if(input.sequence!=null&&(!Number.isSafeInteger(Number(input.sequence))||Number(input.sequence)<0))return false;
   if(a.key)a.retired.add(a.key);
   if(a.sessionId&&a.sessionId!==session)a.retiredSessions.add(a.sessionId);
   // Retired ownership is never revived by an old unknown tool edge.
   a.key=key;a.execution={...execution};a.sequence=input.sequence==null?-1:Number(input.sequence);a.terminal=false;a.registryTerminal=false;resetTools(a);a.sessionId=session;a.waiting=queued?'queued':null;
   emit(a,queued?'waiting':'idle',queued?'queued':'turn_started');return true;
  }
  function accept(input){
   if(dead||input.replay||input.historical||input.source==='history'||!input.agentId)return null;
   const a=entry(input.agentId),key=scopeKey(input.execution);if(!key||a.retired.has(key))return null;
   if(!a.key){if(!begin({...input,sequence:undefined}))return null}
   if(key!==a.key||a.terminal||String(input.sessionId||'')!==a.sessionId)return null;
   if(input.sequence!=null){const n=Number(input.sequence);if(!Number.isSafeInteger(n)||n<0||n<=a.sequence)return null;a.sequence=n}
   return a;
  }
  function ingest(input){
   if(input.kind==='user'||input.kind==='turn_started')return begin(input);
   const a=accept(input);if(!a)return false;
   const event=input.event||input,kind=input.kind||event.kind;
   if(kind==='tool_start'||kind==='tool')return toolEdge(a,kind,event);
   if(kind==='visible_commentary'){
    if(a.waiting||!input.visible||!input.midProgress||!event.text?.trim()||a.tools.size)return false;
    return emit(a,'thinking','visible_commentary');
   }
   if(kind==='ask_pending'||waitingStatus(kind))return wait(a,kind);
   if(kind==='group_member_status'){
    const status=event.state;
    if(waitingStatus(status))return wait(a,status);
    if(status==='working'||status==='running'){a.waiting=null;return emit(a,activeTool(a)||'thinking','running')}
    if(status==='failed')return terminal(a,'error','turn_error');
    if(status==='cancelled')return terminal(a,'idle','stopped');
    if(status==='done')return terminal(a,'success','turn_completed');
    return false;
   }
   if(kind==='failure'||kind==='error'||(kind==='execution_ended'&&event.error))return terminal(a,'error','turn_error');
   if(kind==='stopped'||kind==='canceled')return terminal(a,'idle','stopped');
   if(['settled','turn_completed','execution_ended'].includes(kind))return terminal(a,event.ok===false?'idle':'success',event.ok===false?'stopped':'turn_completed');
   return false;
  }
  // The native registry remains useful without inventing an execution envelope.
  // Call and turn IDs, when present, fence overlap, replay and late completion.
  // Older envelopes without IDs retain only their actual tool/target identity.
  function registry(input){
   if(dead||input.replay||input.historical||input.source==='history'||!input.agentId||!input.sessionId)return false;
   const a=entry(input.agentId),session=String(input.sessionId),status=String(input.status||''),label=String(input.label||''),event=input.event||{},kind=input.kind||event.kind,turn=String(event.turn_id||''),previousStatus=a.registryStatus;
   const sequence=input.sequence??event.event_sequence;
   if(sequence!=null&&(!Number.isSafeInteger(Number(sequence))||Number(sequence)<0))return false;
   if(a.retiredSessions.has(session)||turn&&a.retiredRegistryTurns.has(turn))return false;
   if(input.event&&typeof input.event==='object'){
    if(seenRegistryEvents.has(input.event))return false;
    seenRegistryEvents.add(input.event);
   }
   if(a.sessionId&&a.sessionId!==session){a.retiredSessions.add(a.sessionId);resetTools(a);a.waiting=null;a.registryTurn='';a.registrySequence=-1;a.registryTerminal=false}
   a.sessionId=session;
   const started=kind==='user'||kind==='turn_started';
   if(started&&event.steered!==true&&(!turn||turn!==a.registryTurn)){
    if(a.registryTurn)a.retiredRegistryTurns.add(a.registryTurn);
    a.registryTurn=turn;a.registrySequence=-1;a.registryTerminal=false;a.waiting=null;resetTools(a);
   }else if(turn&&a.registryTurn&&turn!==a.registryTurn)return false;
   else if(turn&&!a.registryTurn)a.registryTurn=turn;
   if(sequence!=null){const n=Number(sequence);if(n<=a.registrySequence)return false;a.registrySequence=n}
   // Only a real idle-to-working registry boundary revives a legacy terminal.
   // A stale label accompanying the preceding settled event cannot do so.
   if(!kind&&a.registryTerminal&&previousStatus==='idle'&&runningStatus(status)){
    if(a.registryTurn)a.retiredRegistryTurns.add(a.registryTurn);
    a.registryTurn='';a.registrySequence=-1;a.registryTerminal=false;resetTools(a);
   }
   a.registryStatus=status;
   if(kind==='failure'||kind==='error'||kind==='execution_ended'&&event.error||kind==='group_member_status'&&event.state==='failed')return terminal(a,'error','native_live_terminal',true);
   if(['stopped','canceled'].includes(kind)||kind==='group_member_status'&&event.state==='cancelled')return terminal(a,'idle','native_live_stopped',true);
   if(a.registryTerminal)return false;
   if(waitingStatus(status))a.waiting=status;
   if(kind==='ask_pending'||waitingStatus(kind)||kind==='group_member_status'&&waitingStatus(event.state))return wait(a,kind==='group_member_status'?event.state:kind);
   if(!kind&&waitingStatus(previousStatus)&&runningStatus(status)||kind==='group_member_status'&&runningStatus(event.state))a.waiting=null;
   if(kind==='tool_start'||kind==='tool'){
    if(!runningStatus(status)&&!a.waiting){a.tools.clear();return emit(a,'idle','native_registry')}
    return toolEdge(a,kind,event,'native_');
   }
   if(['settled','execution_ended','turn_completed'].includes(kind)||kind==='group_member_status'&&event.state==='done')return terminal(a,event.ok===false?'error':'success','native_live_terminal',true);
   if(a.waiting)return holdWaiting(a);
   let mode=activeTool(a)||'idle';
   if(!a.tools.size&&runningStatus(status)){
    if(/brows|searching.*(?:web|page)/i.test(label)||status==='browsing')mode='browsing';
    else if(/command|reading|writing|file|coding|terminal|patch|build|test/i.test(label)||status==='coding')mode='coding';
    else mode='thinking';
   }
   if(status==='idle'){a.tools.clear();mode='idle'}
   return emit(a,mode,'native_registry');
  }
  function settleFinite(agentId,revision){
   if(dead)return false;
   const a=entry(agentId);
   if(a.revision!==revision||!['success','error'].includes(a.mode))return false;
   // Terminal outcomes finish at idle. A tool error is only an accent over
   // the current wait, unfinished calls or still-running execution.
   if(a.terminal||a.registryTerminal)return emit(a,'idle','finite_finished');
   if(a.waiting)return holdWaiting(a);
   return emit(a,activeTool(a)||(runningStatus(a.registryStatus)||a.execution?'thinking':'idle'),'finite_finished');
  }
  return Object.freeze({begin,ingest,registry,snapshot,subscribe(fn){if(dead)throw Error('Activity store destroyed');listeners.add(fn);return()=>listeners.delete(fn)},settleFinite,destroy(){dead=true;listeners.clear();entries.clear()},diagnostics(){return{destroyed:dead,agents:entries.size,subscribers:listeners.size}}});
 }
 root.PhoenixFluffyActivity=Object.freeze({createStore,canonical,category,scopeKey});
})(globalThis);
