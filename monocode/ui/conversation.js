"use strict";

(() => {
  const ui = window.PhoenixUI;
  if (!ui) return;
  const $ = (id) => document.getElementById(id);
  const conversationTail = $("conversationTail");
  const lastConversationRow = () => conversationTail.previousElementSibling;
  const preview = !ui.TAURI || ui.SIDEBAR_PREVIEW;
  const previewShot = new URLSearchParams(location.search).get("shot") || "";
  const state = {
    item: null,
    sessionId: "",
    subscription: null,
    turnSocket: null,
    browserSocket: null,
    browserNative: false,
    browserSurfaceInfo: null,
    browserSurfaceGeneration: 0,
    browserSurfaceBoundsFrame: 0,
    browserSurfaceRecoveryTimer: 0,
    browserSurfaceBoundsInFlight: false,
    browserSurfaceBoundsPending: null,
    browserSurfaceBoundsFailures: 0,
    browserSurfaceStatusTimer: null,
    browserSurfaceStatusPending: null,
    browserTeachingInspect: null,
    browserSurfaceOperationChain: Promise.resolve(),
    browserActivationPromise: null,
    browserControlEnabled: false,
    browserControlWaiters: [],
    browserControlPending: null,
    working: false,
    attachments: [],
    mentions: [],
    groupEveryone: false,
    drafts: new Map(),
    // One send at a time; the sent draft is kept aside (not in the composer)
    // until the runtime acknowledges it, and put back only if the send fails.
    sendInFlight: false,
    unackedSend: null,
    lastSendAt: 0,
    previewQueues: new Map(),
    tasks: [],
    tasksBySession: new Map(),
    queue: [],
    queueBySession: new Map(),
    queueExpandedBySession: new Map(),
    queueActions: new Set(),
    activeTools: new Map(),
    toolRows: [],
    usage: { used: 0, limit: 1 },
    usageBySession: new Map(),
    usageByConversationAgent: new Map(),
    turnUsageSeen: false,
    models: [],
    modelSnapshot: null,
    selectedLane: null,
    selectedModel: null,
    reasoning: "high",
    permission: "workspace",
    workspace: "",
    teaching: null,
    browserFrame: null,
    browserFramePending: null,
    browserFramePainting: false,
    browserFramePaintQueued: false,
    browserFrameUrl: "",
    browserAddressEditing: false,
    browserAddressPending: "",
    browserFrameObjectUrl: "",
    browserCanvasContext: null,
    browserResizeTimer: 0,
    browserResizeKey: "",
    browserMode: null,
    browserOwnerAgentId: null,
    browserOwnerId: null,
    browserTarget: null,
    browserTyping: "",
    browserTypingTimer: null,
    browserTypingPending: null,
    browserWheelFrame: 0,
    browserWheelX: 0,
    browserWheelY: 0,
    browserWheelPoint: null,
    browserActionQueue: Promise.resolve(),
    browserBoundKey: "",
    inspectionOpen: false,
    inspectionExpanded: false,
    inspectionTab: "desktop",
    inspectionConversationStates: new Map(),
    inspectionSwitchGeneration: 0,
    inspectionBrowserResumePromise: null,
    inspectionBrowserOpening: false,
    inspectionRestoreSidebar: false,
    inspectionWidth: Number(localStorage.getItem("phoenix-inspection-width")) || 520,
    summaryOpen: false,
    inspectionResizeStart: null,
    inspectionResizeFrame: 0,
    inspectionImageRequest: 0,
    inspectionImages: [],
    activeInspectionImageId: "",
    imageCommentMode: false,
    imageCommentDraft: null,
    imageAnnotations: new Map(),
    imageActualSize: false,
    environmentSnapshot: null,
    environmentRequest: 0,
    environmentLoading: false,
    environmentAction: "",
    summaryAgentsFolded: localStorage.getItem("phoenix-summary-subagents-folded") !== "false",
    terminalProcesses: [],
    ephemeralWorkers: new Map(),
    browserTabs: [],
    browserFavicons: new Map(),
    browserTabRenderSignature: "",
    closingBrowserTabs: new Set(),
    browserZoom: 1,
    browserGhostTimer: 0,
    browserDownloadSnapshot: new Map(),
    browserDownloadTimer: null,
    browserDownloadPending: false,
    browserDownloadCurrent: null,
    loginAsk: null,
    teachingAsk: null,
    voiceActive: false,
    voiceBusy: false,
    voiceCapture: null,
    composerGeneration: 0,
    revisionOwnerHint: null,
    loadGeneration: 0,
    selectionAbortController: null,
    conversationViews: new Map(),
    pinToLatest: true,
    painting: false,
    scrollFrame: 0,
    composerMeasureFrame: 0,
    composerEndHeight: 0,
    railFrame: 0,
    scrollIdle: 0,
    turnStatus: null,
    activeTurnId: "",
    activeGroupAgentIds: [],
    queuedWakeTurnId: "",
    queuedDrafts: new Map(),
    replayWorkCluster: null,
    turnFocusUntil: 0,
    turnStartedAt: 0,
    reasoningCursorTimer: 0,
    providerRetryTimer: 0,
    providerRetryGroup: null,
    providerRetryDeadline: 0,
    taskSignature: "",
    taskAllComplete: false,
    taskPlanExpanded: localStorage.getItem("phoenix-task-plan-expanded") === "1",
    displayMigrationDirty: false,
    queueRefreshTimer: 0,
    pendingAnswer: null,
    answerMeta: new Map(),
    displayRows: [],
    displayBytes: 0,
    displayDirty: false,
    initialVisibleTurns: Math.max(5,Math.min(50,Number(localStorage.getItem("phoenix-conversation-initial-turns"))||5)),
    renderedRowStart: 0,
    historyPagePending: false,
    historyScrollIntentUntil: 0,
    // Set when feeds_get failed for the open thread: the durable journal is
    // unknown, so writing would replace it with a partial reconstruction.
    displayJournalUnsafe: false,
    groupWorkDisclosure: new Map(),
    workDisclosureSequence: 0,
    sendOrb: null,
    sendOrbHovered: false,
    displayPersistTimer: 0,
    displayPersistChain: Promise.resolve(),
    feedHasAgent: false,
    shimmerClusters: new Set(),
    renderingTurnId: "",
    deletingTurnId: "",
    replayNeedsRepaint: false,
    completionKeys: new Set(),
    pendingHandoffReturns: [],
    workspacePicking: false,
    composerCommentPopoverTimer: 0,
  };
  const DISPLAY_FEED_KEY = "conversation:display:v2";
  const DISPLAY_ROW_CAP = 2400;
  const DISPLAY_BYTE_CAP = 6 * 1024 * 1024;
  // Detached DOM is kept only for the most recently visited conversations.
  // Four complete views make Phoenix/Avery switching instant without allowing
  // a company with hundreds of agents to retain hundreds of large DOM trees.
  const CONVERSATION_VIEW_CACHE_CAP = 4;
  const DURABLE_STORY_KINDS = new Set(["user","narration","commentary","tool","receipt","handoff","group_message","group_member_status","return","card","brief","failure","context","context_compaction","diff","ask_pending","answer","notice","settled"]);

  const icons = {
    work: '<svg viewBox="0 0 20 20"><path d="M4 15V9M8 15V5M12 15v-3M16 15V7"/></svg>',
    user: '<svg viewBox="0 0 20 20"><circle cx="10" cy="7" r="3"/><path d="M4.5 17c.8-3.2 2.6-4.8 5.5-4.8s4.7 1.6 5.5 4.8"/></svg>',
    search: '<svg viewBox="0 0 20 20"><circle cx="8.7" cy="8.7" r="5.2"/><path d="m12.6 12.6 3.7 3.7"/></svg>',
    check: ui.checkIcon,
    chevron: '<svg viewBox="0 0 20 20"><path d="m7 4 6 6-6 6"/></svg>',
    warn: '<svg viewBox="0 0 20 20"><path d="M10 3 2.7 16h14.6L10 3Z"/><path d="M10 7v4M10 14v.1"/></svg>',
  };
  // A real raster Chrome mark, bundled as data instead of another generic
  // Phoenix line icon. Browser work should read as browser work at a glance.
  const CHROME_ICON = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAACXBIWXMAAAsTAAALEwEAmpwYAAAAAXNSR0IArs4c6QAAAARnQU1BAACxjwv8YQUAAAanSURBVFjDnZddjJTVHcZ/z3lnl+VLdpG2yy6UIahdRNMRQxO0FramhpIYoU3UpEbxokk1GkAvegncNK2Jyk0/LkzF3jT1wmjaC8UW0LRqamiBsoqhuLOKWD8qC+zKgjPn6cU578wuWF19J5PzvmfOnOf5f5zn/3/FNK/hWrW72TFrQxFYoxBqQlWJbgNCowqqyxwoCu2bCDyzdN+B0ensq88HHqjGDm0R4S7J3ZJskCSCMCg9pQEk0m/axbm4Y+ErB+pfisBwrdodw8xtKtiCccYpQSSwBEYKwoSQ5yQoyYCs7Qtf/MeOL0RguDZQbQb2KlDNm5n2zplI+rs0aS5tmNamZ+ffRkxzcOG+i70RLpx4vTZQbQbvRVTd4pjBwYI07TQqzYHtyfsYY1F+qlKx9921tepnEni9NlAtgvcaLljoEkCe7LVsottMcYsryBjbmVs1hItJTCFQKO6VvURMtQawkZy8PYUXtqWL4trmWSZDckm1CB17h9fWui8icLR2xTagapCRsJ2iakyaUGICyaaMk9a2AF2uT95IppgcCsBL5oTK5ilJWPv9huqmPe8Nr3/1QxNyApnWfWZqQlDXylXMWnMjs77zXSoL+xLm2CE89i/i8M9gYgQlv0tKIzZkKgLZplI0lmqQekiuL7Y9df3X+HhmJZ3sbKGMibYkNPcSzd/yU3p/9TiX3HYHb/irPPnqWZ47fI7XTl9J6P0RldVDhMsfgo4egUoLjAIigCVndzRi8oJqj2/ornQVJwHW7f+QO/e8m7wlWkcvzJ1H32920XnFAC8fO8+2Z84wdKIxJe6LewoeuGk2t66aiccO0fznemicKhMlZVFKhNJDpyrFJ0uL/ttWrAPfDnC0dyarjp2he7xZipuRNP/+rcxecyMP7x5n6x9O88GZeJF2nJ4wzw2dA8R1yxdD0QUn90xVHKl9I2Y0Hd4IyLcYE4m27F1re8tkMkClr595t9/Jk6+e5ZHdY5+r7Y/sHuPlY+cJi+5FPd82Chk0jy0SQaJSCzG6mr0kInpt0SyGFs/K8od6fnwvAA/vHp9u3Wqt1YKbM2hIXwKtnFAwYk3ArtnG0dgmxsgvv9fL+IyAwDO+sZyhEw2On2xOm8DLx85z/GQTXbpekiwFZzm3HXKCSlJYEmx32y3JAsN7czv4U9aKzssHOP7R9MHL66Vj56Hr65ggW7IlKShbDwRsusMkqctin1j88ZvdjHcFAZw6G78wgXldykIYUAhZSNv5IAVQoWAzGh1N8gGOlm3GZsiPXb/AjXffYUV/xxcm0D+/gscOYwJJSQJGGTiFQ9LJYDzsiErgVt0x+svAHF45+hIr+iqs6KtMG3xxT8FVfRU8PkTKAdmEVKDzqcihORgUfRAbZ+CYScSUEnpsbD8AO26ZO20CD9w0O4XzrUdStcyKaIVUUVQoiV2oh+jmC0mfchUpJSB//37iIL87/DSrl3XyYN74s64Hsxqeevtxe+Id0gkINkGTdcApFPuKvg0r6tHxJ9gzL+gr5Px08P0j3NB/rW6+upfFPQVDJxqcnphasVcv62Dn7fO4ddVM3jl5lMq/t6rLE6klyjUzjXIunKrExt0CuOq363dGs7kMRYa3Y252sOd2zuG+lXforqs3AiRt+KjJqbOR6y7rZFFPAcCuQ0/RHHmUTfPfxqX+O9GwY8aOJsYnKmvfTwQGfr2uGjo0XNbu1pFs3+BUQVg0p1f3XXsHyxcsY/mCZQAcP/Mef37zrzz/5t94+4P97LnyrSz/zp2b28BOlb7SjEs1+J96q5e58rHv73T05lbDF3MbkgVKziFxOzzEdntW5s0vqv/1xvnjalVBorM/yZ2WhLcXNxzfMaUj6izObrepm1iexiTPMTUUMYVHOLcLMW06Wca/NfecN156NiVaq+gUaguPANVL8CkEDty9b5TIIFEjTNWDHBK7BCqtbT+l559XR8s+3XZZcJSEBxmHkaIRBv9vU3rknmfrsgZt6u2to5OV2d1ZpHK6iIgw/GDBBP0zIhBEOvc58jIOwhopGgxqsF7/zPeCI/c8Ww+EQaDeSshW/GOrJ033iWZ/Z4P7+z9ObRehHQIlMkj1ohEvAv9UAiWJo/c+v5SoHWSJTrmActkQlso82NR7LlnfinMwFLnoeGcx65NrPg18Wi+n1UfXVkMRttneZLl1qnIn7v7OqBdXngHHSf0foyY+UVHcqdVHvtzL6acQ6QbWAhsQS2zXwN0PXTbBD79yvi4zGhVfcMMHOmYXT+ua6b2e/w/5+MbZeznFOQAAAABJRU5ErkJggg==";
  function escape(value) { return ui.escapeHtml(value); }
  function escapeRegex(value) { return String(value||"").replace(/[.*+?^${}()|[\]\\]/g,"\\$&"); }
  function currentProfile() { return state.item ? ui.profileFor(state.item) : null; }
  function canvasConversationOwner(item=state.item) {
    if(!item?.id||!['agent','group'].includes(item.kind))throw new Error('No canonical conversation owner is selected.');
    return {kind:item.kind,id:String(item.id)};
  }
  function currentName() { return state.item ? (ui.displayName(state.item, currentProfile()) || "Phoenix") : "Phoenix"; }
  function targetAgent() { return state.item?.kind === "agent" && state.item.id !== "phoenix" ? state.item.id : null; }
  function targetGroup() { return state.item?.kind === "group" ? state.item.id : null; }
  function conversationKey(item = state.item) { return item ? `${item.kind}:${item.id}` : "agent:phoenix"; }
  function conversationIdentity(item=state.item,sessionId=state.sessionId) {
    return item?.kind&&item?.id&&sessionId?`${item.kind}:${item.id}\u0000${sessionId}`:"";
  }
  function selectionToken(generation,item,sessionId,controller) {
    return Object.freeze({generation,key:conversationIdentity(item,sessionId),sessionId,owner:canvasConversationOwner(item),signal:controller.signal});
  }
  function selectionIsCurrent(token) {
    return Boolean(token&&!token.signal.aborted&&token.generation===state.loadGeneration&&token.key===conversationIdentity());
  }
  function activeSelectionToken(generation=state.loadGeneration) {
    if(!state.item||!state.sessionId)return null;
    if(!state.selectionAbortController||state.selectionAbortController.signal.aborted)state.selectionAbortController=new AbortController();
    return selectionToken(generation,state.item,state.sessionId,state.selectionAbortController);
  }
  // Chips re-render from text on every keystroke. Serialize them as private-use
  // markers so a chip always comes back as the same chip; display text alone
  // ("Iris", "Everyone") is lossy and silently dropped pings.
  const CHIP_OPEN="\uE000",CHIP_CLOSE="\uE001";
  function composerTokenProfile(node){return node?.dataset?.composerAgent?knownAgentProfile(node.dataset.composerAgent):null;}
  function composerNodeText(node,mode="display"){
    if(node.nodeType===Node.TEXT_NODE)return node.data.replaceAll("\u200B","");
    if(node.nodeType!==Node.ELEMENT_NODE)return"";
    if(node.matches?.(".composer-inline-agent")){
      if(node.dataset.everyone==="true")return mode==="tokens"?`${CHIP_OPEN}everyone${CHIP_CLOSE}`:mode==="request"?"@everyone":"Everyone";
      const profile=composerTokenProfile(node),id=node.dataset.composerAgent||"",label=profile?.display_name||node.dataset.composerLabel||node.textContent.trim();
      return mode==="tokens"?`${CHIP_OPEN}${id}${CHIP_CLOSE}`:mode==="request"?`@${id}`:label;
    }
    if(node.tagName==="BR")return"\n";
    let text=[...node.childNodes].map((child)=>composerNodeText(child,mode)).join("");
    if(node!==$("composerInput")&&/^(?:DIV|P)$/.test(node.tagName)&&!text.endsWith("\n"))text+="\n";
    return text;
  }
  function composerText(mode="display"){return composerNodeText($("composerInput"),mode).replace(/\n$/,"");}
  function composerGroupProfiles(){
    if(state.item?.kind!=="group")return[];
    const members=new Set((ui.state.view?.directory.members||[]).filter((member)=>member.group_id===state.item.id).map((member)=>member.agent_id));
    return(ui.state.view?.directory.agents||[]).filter((agent)=>members.has(agent.agent_id)&&activatableCoworker(agent));
  }
  function composerAgentToken(profile){
    const node=document.createElement("span");node.className="composer-inline-agent";node.contentEditable="false";node.dataset.composerAgent=profile.agent_id;node.dataset.composerLabel=profile.display_name;node.style.setProperty("--agent-chip",profile.color||"var(--ember)");node.setAttribute("role","button");node.setAttribute("aria-label",`${profile.display_name} will be pinged. Click to remove.`);node.title=`Wakes ${profile.display_name} · click to remove`;node.innerHTML=`<span class="agent-chip-avatar">${ui.avatarSvg(profile)}</span><strong>${escape(profile.display_name)}</strong>`;return node;
  }
  function composerEveryoneToken(){
    const node=document.createElement("span");node.className="composer-inline-agent";node.contentEditable="false";node.dataset.everyone="true";node.setAttribute("role","button");node.setAttribute("aria-label","Everyone will be pinged. Click to remove.");node.title="Wakes everyone · click to remove";node.innerHTML='<span class="agent-chip-avatar mention-everyone-icon">@</span><strong>Everyone</strong>';return node;
  }
  function composerMentionMatch(text,start,allowEnd,plainIds=null,profiles=composerGroupProfiles(),includeEveryone=!state.groupEveryone){
    const candidates=[];
    const chip=new RegExp(`${CHIP_OPEN}([^${CHIP_CLOSE}]*)${CHIP_CLOSE}`,"gu");chip.lastIndex=start;const chipMatch=chip.exec(text);
    if(chipMatch){const id=chipMatch[1],from=chipMatch.index,to=from+chipMatch[0].length;if(id==="everyone")candidates.push(state.item?.kind==="group"&&includeEveryone?{from,to,profile:null,everyone:true}:{from,to,profile:null,everyone:false,label:""});else{const profile=profiles.find((row)=>row.agent_id===id);candidates.push(profile?{from,to,profile,everyone:false}:{from,to,profile:null,everyone:false,label:knownAgentProfile(id)?.display_name||id});}}
    const boundary="[^\\p{L}\\p{N}_]",push=(match,profile,everyone=false)=>{if(!match)return;const prefix=match[1]||"",from=match.index+prefix.length,to=match.index+match[0].length;if(!allowEnd&&to===text.length)return;candidates.push({from,to,profile,everyone});};
    profiles.forEach((profile)=>{
      const name=escapeRegex(profile.display_name),ids=[profile.agent_id,profile.internal_role,profile.display_name].filter(Boolean).map(escapeRegex).join("|");
      const plain=new RegExp(`(^|${boundary})(${name})(?=$|${boundary})`,`gu`);plain.lastIndex=start;if(!plainIds||plainIds.has(profile.agent_id))push(plain.exec(text),profile);
      const tagged=new RegExp(`(^|${boundary})@(?:${ids})(?=$|${boundary})`,`giu`);tagged.lastIndex=start;push(tagged.exec(text),profile);
    });
    if(state.item?.kind==="group"&&includeEveryone){const everyone=new RegExp(`(^|${boundary})@everyone(?=$|${boundary})`,`giu`);everyone.lastIndex=start;push(everyone.exec(text),null,true);}
    return candidates.sort((left,right)=>left.from-right.from||(right.to-right.from)-(left.to-left.from))[0]||null;
  }
  function appendComposerPlainText(fragment,value){
    const parts=String(value??"").split("\n");
    parts.forEach((part,index)=>{if(index)fragment.append(document.createElement("br"));if(part)fragment.append(document.createTextNode(part));else if(index&&index===parts.length-1)fragment.append(document.createElement("br"));});
  }
  function renderComposerText(value,{allowEnd=false,prefixMentions=[]}={}){
    const input=$("composerInput"),text=String(value||""),fragment=document.createDocumentFragment(),seen=new Set();let cursor=0;
    if(!text.trim()&&!prefixMentions.length)delete input.dataset.explicitPings;
    state.groupEveryone=false;
    const tagged=composerGroupProfiles().some(profile=>[profile.agent_id,profile.internal_role,profile.display_name].filter(Boolean).some(name=>new RegExp(`@${escapeRegex(name)}(?=$|[^\\p{L}\\p{N}_])`,"iu").test(text)));
    if(tagged||prefixMentions.length)input.dataset.explicitPings="true";
    const plainIds=input.dataset.explicitPings==="true"?new Set([...state.mentions,...prefixMentions]):null;
    while(cursor<text.length){const match=composerMentionMatch(text,cursor,allowEnd,plainIds);if(!match){appendComposerPlainText(fragment,text.slice(cursor));break;}if(match.from>cursor)appendComposerPlainText(fragment,text.slice(cursor,match.from));if(match.everyone){fragment.append(composerEveryoneToken());state.groupEveryone=true;state.mentions=[];}else if(match.profile&&!seen.has(match.profile.agent_id)&&!state.groupEveryone){fragment.append(composerAgentToken(match.profile));seen.add(match.profile.agent_id);}else appendComposerPlainText(fragment,match.label??(match.profile?.display_name&&text[match.from]===CHIP_OPEN?match.profile.display_name:text.slice(match.from,match.to)));cursor=match.to;}
    if(!text)fragment.append(document.createTextNode(""));
    if(state.item?.kind==="group"&&!state.groupEveryone){const allowed=new Set(composerGroupProfiles().map((profile)=>profile.agent_id)),missing=prefixMentions.map(knownAgentProfile).filter(Boolean).filter((profile)=>allowed.has(profile.agent_id)&&!seen.has(profile.agent_id));if(missing.length){const prefixed=document.createDocumentFragment();missing.forEach((profile,index)=>{if(index)prefixed.append(document.createTextNode(" "));prefixed.append(composerAgentToken(profile));seen.add(profile.agent_id);});if(text)prefixed.append(document.createTextNode(" "));prefixed.append(fragment);input.replaceChildren(prefixed);}else input.replaceChildren(fragment);}else input.replaceChildren(fragment);
    if(state.item?.kind==="group"){state.mentions=state.groupEveryone?[]:[...input.querySelectorAll("[data-composer-agent]")].map((node)=>node.dataset.composerAgent).filter((id,index,rows)=>id&&rows.indexOf(id)===index);}else{state.mentions=[];state.groupEveryone=false;}
  }
  function composerCaretOffset(){
    const input=$("composerInput"),selection=getSelection();if(!selection?.rangeCount||!input.contains(selection.anchorNode))return composerText().length;
    const range=selection.getRangeAt(0).cloneRange();range.selectNodeContents(input);range.setEnd(selection.anchorNode,selection.anchorOffset);const holder=document.createElement("div");holder.append(range.cloneContents());const before=[...holder.childNodes].map((node)=>composerNodeText(node,"display")).join(""),atStructuralEnd=selection.anchorNode===input&&selection.anchorOffset===input.childNodes.length;return(atStructuralEnd?before.replace(/\n$/,""):before).length;
  }
  function setComposerCaretOffset(offset,{anchorLine=false}={}){
    const input=$("composerInput"),selection=getSelection(),range=document.createRange();let remaining=Math.max(0,Number(offset)||0),placed=false;
    const walk=(node)=>{for(const child of [...node.childNodes]){if(child.nodeType===Node.TEXT_NODE){const length=composerNodeText(child).length;if(remaining<=length){range.setStart(child,Math.min(remaining,child.data.length));placed=true;return;}remaining-=length;continue;}if(child.nodeType!==Node.ELEMENT_NODE)continue;if(child.matches?.(".composer-inline-agent")){const length=composerNodeText(child).length;if(remaining<=length){remaining<length/2?range.setStartBefore(child):range.setStartAfter(child);placed=true;return;}remaining-=length;continue;}if(child.tagName==="BR"){if(remaining===0){range.setStartBefore(child);placed=true;return;}if(remaining===1){if(anchorLine){const marker=document.createTextNode("\u200B");child.after(marker);range.setStart(marker,1);}else{const next=child.nextSibling;next?.nodeType===Node.ELEMENT_NODE&&next.tagName==="BR"?range.setStartBefore(next):range.setStartAfter(child);}placed=true;return;}remaining-=1;continue;}walk(child);if(placed)return;}};walk(input);if(!placed)range.selectNodeContents(input),range.collapse(false);else range.collapse(true);selection.removeAllRanges();selection.addRange(range);
  }
  function normalizeComposerTokens(allowEnd=false){const input=$("composerInput"),selection=getSelection(),anchorLine=selection?.anchorNode?.nodeType===Node.TEXT_NODE&&selection.anchorNode.data==="\u200B",text=composerText("tokens"),caret=composerCaretOffset();renderComposerText(text,{allowEnd});setComposerCaretOffset(caret,{anchorLine});}
  function composerSplitAtCaret(mode="tokens"){
    const input=$("composerInput"),selection=getSelection(),full=composerText(mode);if(!selection?.rangeCount||!input.contains(selection.anchorNode))return[full,""];
    const range=selection.getRangeAt(0).cloneRange();range.selectNodeContents(input);range.setEnd(selection.anchorNode,selection.anchorOffset);const holder=document.createElement("div");holder.append(range.cloneContents());const before=[...holder.childNodes].map((node)=>composerNodeText(node,mode)).join("").slice(0,full.length);return[before,full.slice(before.length)];
  }
  function composerTokensDisplayLength(text){return text.replace(new RegExp(`${CHIP_OPEN}([^${CHIP_CLOSE}]*)${CHIP_CLOSE}`,"gu"),(_,id)=>id==="everyone"?"Everyone":knownAgentProfile(id)?.display_name||id).length;}
  function insertComposerLineBreak(){
    const[before,after]=composerSplitAtCaret(),caret=composerTokensDisplayLength(before);
    renderComposerText(`${before}\n${after}`,{allowEnd:true});
    setComposerCaretOffset(caret+1,{anchorLine:true});autosize();persistComposerDraft();renderImageCommentState();
  }
  function initializeComposerInput(){
    const input=$("composerInput");if(input.dataset.richComposer==="true")return;input.dataset.richComposer="true";
    Object.defineProperty(input,"value",{configurable:true,get:()=>composerText("display"),set:(value)=>renderComposerText(String(value??""),{allowEnd:true})});
    Object.defineProperty(input,"placeholder",{configurable:true,get:()=>input.dataset.placeholder||"",set:(value)=>{input.dataset.placeholder=String(value||"");}});
  }
  function draftStorageKey(item=state.item){return `phoenix-composer-draft:${conversationKey(item)}`;}
  function normalizedImageComments(value){return(Array.isArray(value)?value:[]).map((comment)=>({x:Math.max(0,Math.min(100,Number(comment?.x)||0)),y:Math.max(0,Math.min(100,Number(comment?.y)||0)),text:String(comment?.text||"").trim(),createdAt:comment?.createdAt||null})).filter((comment)=>comment.text);}
  function draftSnapshot(){return{text:$("composerInput")?.value||"",attachments:state.attachments.map(({name,path,size,type,preview,comments})=>({name,path,size,type,preview:preview||"",comments:normalizedImageComments(comments)})).filter((file)=>file.path),tokens:$("composerInput")?composerText("tokens"):"",mentions:[...state.mentions],everyone:Boolean(state.groupEveryone)};}
  function persistComposerDraft(item=state.item){
    if(!item)return;const draft=draftSnapshot(),empty=!draft.text&&!draft.attachments.length&&!draft.mentions.length&&!draft.everyone,key=draftStorageKey(item);
    const durable={...draft,attachments:draft.attachments.map(({preview,...file})=>file)};
    if(empty)localStorage.removeItem(key);else try{localStorage.setItem(key,JSON.stringify(durable));}catch{}
    state.drafts.set(conversationKey(item),draft);
  }
  function restoredComposerDraft(item=state.item){
    const memory=state.drafts.get(conversationKey(item));if(memory)return memory;
    try{const value=JSON.parse(localStorage.getItem(draftStorageKey(item))||"null");
      // Legacy drafts have no acknowledged submission identity. Repeated
      // prompt text cannot prove they were sent, so restore them intact.
      if(value&&typeof value.text==="string")return{text:value.text,tokens:typeof value.tokens==="string"?value.tokens:null,attachments:Array.isArray(value.attachments)?value.attachments:[],mentions:Array.isArray(value.mentions)?value.mentions:[],everyone:Boolean(value.everyone)};}catch{}
    return{text:"",attachments:[],mentions:[],everyone:false};
  }
  function permissionKey() { return `phoenix-permission:${state.item?.kind || "agent"}:${state.item?.id || "phoenix"}`; }
  function modelLane() { return state.item?.kind === "group" ? null : state.item?.id || "phoenix"; }
  function notificationEnabled(category) {
    // Opening a thread repaints its whole journal back through renderStory, so
    // every historical answer/failure would re-fire its notification — months of
    // them, every time you click a chat. A repaint is replay, never news.
    if (state.painting) return false;
    const root = document.documentElement.dataset;
    if (root.notificationsEnabled === "false") return false;
    return root[`notifications${category.replace(/^./,(letter)=>letter.toUpperCase())}`] !== "false";
  }
  function spokenRepliesEnabled() {
    const root = document.documentElement.dataset;
    return root.voiceEnabled !== "false" && root.voiceReplies === "true";
  }
  function speakReply(markdownText) {
    // Same replay trap as notificationEnabled: a repaint would read every
    // answer in the thread's history out loud.
    if (preview || state.painting || !spokenRepliesEnabled()) return;
    const text = String(markdownText || "")
      .replace(/```[\s\S]*?```/g, " Code block omitted. ")
      .replace(/!\[[^\]]*\]\([^)]*\)/g, " ")
      .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
      .replace(/[`*_>#~-]/g, " ")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 4000);
    if (text) ui.invoke("voice_speak", { text }).catch((error) => ui.toast(`Could not play spoken reply: ${error}`, true));
  }
  function itemForConversationLabel(label) {
    const directory = ui.state.view?.directory;
    const agent = directory?.agents.find((row) => row.display_name === label);
    if (agent) return { kind:"agent", id:agent.agent_id };
    const group = directory?.groups.find((row) => row.name === label);
    return group ? { kind:"group", id:group.group_id } : null;
  }

  // Minimal, dependency-free tokeniser. It runs over ALREADY-ESCAPED text, so
  // it may only wrap spans — it never introduces markup that could reintroduce
  // the raw source. Comments and strings are matched first so a keyword inside
  // either is not re-coloured.
  const CODE_RULES = [
    ["cb-comment", /(\/\/[^\n]*|#[^\n]*|\/\*[\s\S]*?\*\/)/g],
    ["cb-string", /(&quot;[^&\n]*?&quot;|&#39;[^&\n]*?&#39;|`[^`\n]*?`)/g],
    ["cb-keyword", /\b(const|let|var|function|return|if|else|for|while|import|export|from|class|new|await|async|try|catch|throw|fn|pub|impl|struct|enum|match|use|mut|self|def|end|elif|then|do|in|of|null|true|false|None|Some|Ok|Err)\b/g],
    ["cb-number", /\b(\d+(?:\.\d+)?)\b/g],
  ];
  function highlightCode(escaped) {
    // Split on existing spans so later rules cannot nest inside earlier ones.
    let parts = [escaped];
    CODE_RULES.forEach(([cls, pattern]) => {
      parts = parts.flatMap((part) => {
        if (typeof part !== "string") return [part];
        const out = []; let last = 0;
        part.replace(pattern, (match, _g, offset) => {
          if (offset > last) out.push(part.slice(last, offset));
          out.push({ cls, text: match });
          last = offset + match.length;
          return match;
        });
        if (last < part.length) out.push(part.slice(last));
        return out;
      });
    });
    return parts.map((part) => typeof part === "string" ? part : `<span class="${part.cls}">${part.text}</span>`).join("");
  }
  // Code fences follow beUI's CodeBlock: a header (file icon, language,
  // Ready, copy) over numbered, highlighted lines.
  function codeBlockMarkup(lang, code) {
    const kit = window.PhoenixAgentKit, language = escape(lang || "code");
    const lines = code.replace(/\n$/, "").split("\n");
    const body = lines.map((line, index) => `<span class="cb-row"><span class="cb-num">${index + 1}</span><span class="cb-line">${highlightCode(line) || " "}</span></span>`).join("");
    return `<figure class="code-block" data-language="${language}">`
      + `<figcaption class="cb-head">${kit.icon("fileCode", "cb-file")}<span class="code-language cb-lang">${language}</span>`
      + `<span class="cb-ready">${kit.icon("check")}Ready</span>`
      + `<button type="button" class="code-copy" data-copy-code aria-label="Copy code" title="Copy code">${kit.icon("copy")}</button></figcaption>`
      + `<div class="code-body cb-body"><pre class="code-source cb-pre"><code data-language="${language}">${body}</code></pre></div>`
      + `<button type="button" class="code-peek" data-code-peek>Show ${lines.length} line${lines.length === 1 ? "" : "s"} of ${language}</button>`
      + `</figure>`;
  }
  function localMarkdownPath(value) {
    // Markdown destinations encode spaces and Unicode as URL bytes; the
    // native opener expects a filesystem path and enforces the workspace.
    try { return decodeURIComponent(value); } catch { return value; }
  }
  function inlineMarkdown(text) {
    return text.replace(/`([^`\n]+)`/g, (_, value) => /^(?:\/home\/|\/tmp\/|artifacts\/|\.\/)[^\n?*\[\]{}]+\.(?:md|txt|json|csv|pdf|png|jpe?g|webp|gif|avif|html?)$/i.test(value) ? `<button type="button" class="markdown-local-link" data-open-local-path="${value}"><code>${value}</code></button>` : `<code>${value}</code>`)
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/\*([^*\n]+)\*/g, "<em>$1</em>")
      // file:///path links (agents use them for local demos) are local paths too.
      .replace(/\[([^\]]+)\]\(file:\/\/(\/[^)\n]+)\)/g, '<button type="button" class="markdown-local-link" data-open-local-path="$2">$1</button>')
      .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>')
      // Artifact/file links are first-class Markdown too. Keep them out of
      // href navigation (WebKit swallows local target=_blank links) and route
      // the click through the workspace-bounded native opener instead.
      .replace(/\[([^\]]+)\]\(((?:\.{0,2}\/|\/|[\w.-]+\/)[^)\n]+)\)/g, '<button type="button" class="markdown-local-link" data-open-local-path="$2">$1</button>');
  }
  function markdownTableCells(line) {
    let row=String(line||"").trim();
    if(!row.includes("|"))return null;
    if(row.startsWith("|"))row=row.slice(1);
    let trailingSlashes=0;for(let index=row.length-2;index>=0&&row[index]==="\\";index-=1)trailingSlashes+=1;
    if(row.endsWith("|")&&trailingSlashes%2===0)row=row.slice(0,-1);
    const cells=[];let cell="",codeTicks=0;
    for(let index=0;index<row.length;index+=1){
      const char=row[index];
      if(char==="\\"&&row[index+1]==="|"){cell+="|";index+=1;continue;}
      if(char==="`"){codeTicks=codeTicks?0:1;cell+=char;continue;}
      if(char==="|"&&!codeTicks){cells.push(cell.trim());cell="";continue;}
      cell+=char;
    }
    cells.push(cell.trim());
    return cells.length>1?cells:null;
  }
  function markdownTableAlignment(delimiter) {
    const value=delimiter.trim();
    if(value.startsWith(":")&&value.endsWith(":"))return"center";
    if(value.endsWith(":"))return"right";
    return"left";
  }
  function markdownTableMarkup(header,delimiter,body) {
    const width=header.length,alignments=delimiter.map(markdownTableAlignment),normalized=(row)=>Array.from({length:width},(_,index)=>row[index]||"");
    const cell=(tag,value,index)=>`<${tag} class="markdown-align-${alignments[index]||"left"}">${inlineMarkdown(value)}</${tag}>`;
    return `<div class="markdown-table-wrap" tabindex="0" role="region" aria-label="Scrollable table"><table><thead><tr>${normalized(header).map((value,index)=>cell("th",value,index)).join("")}</tr></thead><tbody>${body.map((row)=>`<tr>${normalized(row).map((value,index)=>cell("td",value,index)).join("")}</tr>`).join("")}</tbody></table></div>`;
  }
  function markdown(source) {
    let normalized=String(source ?? "").replace(/\r\n/g, "\n");
    // Recover unmistakable Markdown blocks when a provider flattened them
    // onto one transport line. Ordinary prose is left untouched.
    normalized=normalized.replace(/\s+(#{1,3})\s+(?=\S)/g,"\n\n$1 ");
    const numbered=(normalized.match(/(?:^|\s)\d+[.)]\s+/g)||[]).length;
    if(numbered>=2)normalized=normalized.replace(/\s+(\d+[.)])\s+(?=\S)/g,"\n$1 ");
    const bullets=(normalized.match(/(?:^|[;:])\s+-\s+/g)||[]).length;
    if(bullets>=2)normalized=normalized.replace(/([;:])\s+-\s+/g,"$1\n- ").replace(/;\s+-\s+/g,"\n- ");
    const imageBlocks=[];
    normalized=normalized.replace(/!\[([^\]]*)\]\(([^)\n]+?\.(?:png|jpe?g|webp|gif|avif))\)/gi,(_,alt,rawPath)=>{const path=normalizedLocalImagePath(rawPath);if(!path)return _;const name=String(alt||"").trim()||path.split("/").filter(Boolean).at(-1)||"Image";const id=imageBlocks.push(`<button type="button" class="relayed-image markdown-relayed-image loading" data-relayed-image-path="${escape(path)}" aria-label="Open ${escape(name)} in image workspace"><span>${escape(name)}</span></button>`)-1;return `\u0002${id}\u0002`;});
    let text = escape(normalized);
    const blocks = [];
    text = text.replace(/```([\w-]*)\n([\s\S]*?)```/g, (_, lang, code) => {
      const id = blocks.push(codeBlockMarkup(lang, code)) - 1;
      return `\u0000${id}\u0000`;
    });
    const tables=[],tableLines=text.split("\n"),tableText=[];
    for(let index=0;index<tableLines.length;index+=1){
      const header=markdownTableCells(tableLines[index]),delimiter=markdownTableCells(tableLines[index+1]);
      const valid=header&&delimiter&&header.length===delimiter.length&&delimiter.every((value)=>/^:?-{3,}:?$/.test(value.trim()));
      if(!valid){tableText.push(tableLines[index]);continue;}
      const body=[];let cursor=index+2;
      while(cursor<tableLines.length){const row=markdownTableCells(tableLines[cursor]);if(!row)break;body.push(row);cursor+=1;}
      const id=tables.push(markdownTableMarkup(header,delimiter,body))-1;tableText.push(`\u0003${id}\u0003`);index=cursor-1;
    }
    text=inlineMarkdown(tableText.join("\n"));
    const lines = text.split("\n"), out = []; let list = null, quote = false;
    const closeList = () => { if (list) { out.push(`</${list}>`); list = null; } };
    const closeQuote = () => { if (quote) { closeList(); out.push("</blockquote>"); quote = false; } };
    for (const line of lines) {
      const quoted = line.match(/^\s*&gt;\s?(.*)$/);
      if (quoted) {
        if (!quote) { closeList(); out.push("<blockquote>"); quote = true; }
        if (quoted[1].trim()) out.push(`<p>${quoted[1]}</p>`);
        continue;
      }
      closeQuote();
      const bullet = line.match(/^\s*[-*]\s+(.+)/), numbered = line.match(/^\s*\d+[.)]\s+(.+)/);
      if (bullet || numbered) {
        const kind = bullet ? "ul" : "ol"; if (list !== kind) { closeList(); out.push(`<${kind}>`); list = kind; }
        const body=(bullet || numbered)[1],task=body.match(/^\[([ xX])\]\s+(.*)$/);
        out.push(task?`<li class="task-item" data-done="${task[1]!==" "}"><span class="task-box" role="img" aria-label="${task[1]!==" "?"Done":"Not done"}"></span><span>${task[2]}</span></li>`:`<li>${body}</li>`); continue;
      }
      closeList();
      if (/^[\u0000\u0002\u0003]\d+[\u0000\u0002\u0003]$/.test(line)) out.push(line);
      else if (/^###\s+/.test(line)) out.push(`<h3>${line.slice(4)}</h3>`);
      else if (/^##\s+/.test(line)) out.push(`<h2>${line.slice(3)}</h2>`);
      else if (/^#\s+/.test(line)) out.push(`<h1>${line.slice(2)}</h1>`);
      else if (line.trim()) out.push(`<p>${line}</p>`);
    }
    closeList(); closeQuote();
    return out.join("").replace(/\u0000(\d+)\u0000/g, (_, i) => blocks[Number(i)] || "").replace(/\u0002(\d+)\u0002/g,(_,i)=>imageBlocks[Number(i)]||"").replace(/\u0003(\d+)\u0003/g,(_,i)=>tables[Number(i)]||"");
  }

  const LOCAL_IMAGE_PATH=/((?:\/home\/|\/tmp\/|\/var\/tmp\/|\.\.\/|\.\/|artifacts\/|images\/|screenshots\/)[^<>\n`"']*?\.(?:png|jpe?g|webp|gif|avif))(?=$|[\s`"',;:)\]])/gim;
  function normalizedLocalImagePath(value){let path=String(value||"").trim().replace(/^['"]|['"]$/g,"");if(/[?*\[\]{}]/.test(path)||!/\.(?:png|jpe?g|webp|gif|avif)$/i.test(path))return"";if(!path.startsWith("/")){if(!state.workspace)return"";path=`${state.workspace.replace(/\/$/,"")}/${path.replace(/^\.\//,"")}`;}return path;}
  function relayedImageReferences(source){
    const visible=String(source||"").replace(/```[\s\S]*?```/g,"").replace(/!\[[^\]]*\]\([^)\n]+?\.(?:png|jpe?g|webp|gif|avif)\)/gi,""),paths=[];let match;LOCAL_IMAGE_PATH.lastIndex=0;
    while((match=LOCAL_IMAGE_PATH.exec(visible))&&paths.length<6){const path=normalizedLocalImagePath(match[1].trim().replace(/[),.;:]+$/g,""));if(path&&!paths.includes(path))paths.push(path);}
    return paths.map((path)=>({path,name:path.split("/").filter(Boolean).at(-1)||"Image"}));
  }
  function relayedImagesMarkup(source){const images=relayedImageReferences(source);return images.length?`<div class="relayed-images">${images.map((image)=>`<button type="button" class="relayed-image loading" data-relayed-image-path="${escape(image.path)}" aria-label="Open ${escape(image.name)} in activity sidebar"><span>${escape(image.name)}</span></button>`).join("")}</div>`:"";}
  function hydrateRelayedImages(node){
    node?.querySelectorAll("[data-relayed-image-path]").forEach(async(button)=>{const path=button.dataset.relayedImagePath;if(!path)return;try{const source=preview?ui.phoenixLogoSource():await ui.invoke("image_data_url",{path});if(!button.isConnected)return;const image=document.createElement("img");image.alt=button.querySelector("span")?.textContent||"Relayed image";image.src=source;button.prepend(image);button._inspectionSource=source;button.classList.remove("loading");syncActivitySummary();}catch{button.classList.remove("loading");button.classList.add("failed");}});
  }

  function agentIdentityCandidates(id) {
    const raw=String(id||"").trim(),lower=raw.toLowerCase();if(!lower)return[];
    const decorated=raw.match(/^(.+?)\s*\(([^()]+)\)\s*$/),withoutInstance=raw.replace(/#\d+$/,""),values=[raw,withoutInstance];
    if(decorated)values.push(decorated[1],decorated[2]);
    if(/^(?:orchestrator|phoenix)$/i.test(raw))values.push("phoenix","orchestrator");
    return[...new Set(values.map((value)=>String(value||"").trim().toLowerCase()).filter(Boolean))];
  }
  function knownAgentProfile(id) {
    const candidates=agentIdentityCandidates(id);
    return ui.state.view?.directory.agents.find((agent)=>[agent.agent_id,agent.internal_role,agent.display_name].some((value)=>candidates.includes(String(value||"").trim().toLowerCase())));
  }
  function canonicalAgentId(id) {
    const profile=knownAgentProfile(id);if(profile)return profile.agent_id;
    const candidate=agentIdentityCandidates(id)[0]||"";
    if(isEphemeralVolumeAgent(candidate))return "volume_worker";
    return /^(?:orchestrator|phoenix)$/.test(candidate)?"phoenix":candidate;
  }
  function isEphemeralVolumeAgent(value) {
    return /volume[\s_-]*worker/i.test(String(value||""));
  }
  function isEphemeralVolumeHandoff(event) {
    if(!event||typeof event!=="object")return false;
    return [event.agent,event.from,event.to,event.requester,event.receiver].some(isEphemeralVolumeAgent);
  }
  function agentProfile(id) {
    const known=knownAgentProfile(id);if(known)return known;
    const canonical=canonicalAgentId(id)||"phoenix";
    if(canonical==="phoenix")return ui.state.view?.directory.agents.find((agent)=>agent.agent_id==="phoenix");
    return{agent_id:canonical,internal_role:canonical,display_name:String(id||canonical).replace(/\s*\([^()]+\)\s*$/,""),color:"#77736d",icon_seed:canonical,metadata_json:"{}"};
  }
  function visibleAnswerText(text, agent = null) {
    const raw=String(text||""),match=raw.match(/^\s*(?:\*\*|__)?\[([a-z][a-z0-9_-]{1,63})\](?:\*\*|__)?[ \t]*/i);
    if(!match)return raw;
    const profile=agentProfile(agent||state.item?.id),tag=match[1].toLowerCase();
    const directoryLabels=(ui.state.view?.directory.agents||[]).flatMap((entry)=>[entry.agent_id,entry.internal_role]);
    const privateLabels=new Set([agent,profile?.agent_id,profile?.internal_role,state.item?.kind==="agent"?state.item.id:null,...directoryLabels].filter(Boolean).map((value)=>String(value).trim().toLowerCase()));
    return privateLabels.has(tag)?raw.slice(match[0].length):raw;
  }
  function agentLabel(id) {
    const profile=knownAgentProfile(id),key=String(id||"").trim().toLowerCase(),runtimeLabels={orchestrator:"Phoenix",phoenix:"Phoenix",computer_use:"Desktop control",browser:"Browser",librarian:"Librarian",indexer:"Indexer",vision:"Vision",image:"Image generation"};
    return profile?.display_name||runtimeLabels[key]||String(id||"Phoenix").replace(/\s*\([^()]+\)\s*$/," ").trim().replaceAll("_"," ").replace(/^./,(letter)=>letter.toUpperCase());
  }
  function avatar(profile, kind = "agent") { return `<span class="message-avatar">${ui.avatarSvg(profile, kind)}</span>`; }
  function syncMessageGroups() {
    // Presentation only: each prompt/answer keeps its identity, turn and actions.
    // Activity, decisions and a different speaker always break a message group.
    let previous = null;
    for (const node of $("conversationFeed").children) {
      node.classList.remove("message-continuation", "message-group-start");
      if (node.hidden || node.matches(".superseded-answer,.superseded-update") || getComputedStyle(node).display === "none") {
        if (node.classList.contains("decision-request")) previous = null;
        continue;
      }
      const message = node.dataset.slot === "message"
        && !node.matches(".decision-request,.peer-message");
      const speaker = message ? (node.dataset.from === "user" ? "user" : node.dataset.speaker) : null;
      if (speaker && previous?.speaker === speaker) {
        node.classList.add("message-continuation");
        previous.node.classList.add("message-group-start");
      }
      previous = speaker ? { node, speaker } : null;
    }
  }
  function feedNode(className, html, data = {}) {
    const node = document.createElement("article"); node.className = className; node.innerHTML = html;
    const turnId=state.renderingTurnId||state.activeTurnId||state.displayRows.at(-1)?.turn_id||"";
    if(turnId)node.dataset.turnId=turnId;
    Object.assign(node.dataset, data);
    if(!state.painting){node.classList.add("message-entering");node.addEventListener("animationend",()=>node.classList.remove("message-entering"),{once:true});setTimeout(()=>node.classList.remove("message-entering"),500);}
    const feed=$("conversationFeed"),pending=feed.querySelector(':scope > .user-message[data-queued-pending="true"]'),authored=/(?:^|\s)user-message(?:\s|$)/.test(className)&&!/(?:^|\s)peer-message(?:\s|$)/.test(className);
    // Output from the still-running turn stays before any follow-up the user
    // has queued. The queued prompt is a normal bubble, but it is also the next
    // turn boundary—not a place for the current answer to land underneath.
    if(pending&&!authored&&turnId&&turnId===state.activeTurnId)feed.insertBefore(node,pending);else feed.insertBefore(node,conversationTail);
    if(/(?:^|\s)(?:agent-message|group-message|commentary-line|work-cluster)(?:\s|$)/.test(className))setComposerThreadState(true);
    if (!state.painting) scrollLatest();
    return node;
  }
  function paintFeed(draw, followLatest = true) {
    const feed = $("conversationFeed");
    state.painting = true;
    feed.classList.add("painting");
    try { draw(); } finally { feed.classList.remove("painting"); state.painting = false; }
    reconcileGroupStatusContinuations();
    renderPromptRail();
    syncComposerFade();
    if(followLatest)scrollLatest(true);
  }
  function setComposerThreadState(hasAgent) {
    const fade = $("composerFade");
    if (!fade) return;
    state.feedHasAgent=Boolean(hasAgent);
    fade.hidden = !hasAgent;
    $("conversationStage")?.classList.toggle("has-thread", hasAgent);
  }
  function syncComposerFade() {
    const feed=$("conversationFeed");
    setComposerThreadState(Boolean(feed.querySelector(".agent-message,.group-message,.commentary-line,.work-cluster")));
    $("composerFade").hidden=!feed.querySelector(".message-row,.group-message,.commentary-line,.work-cluster");
  }
  function feedSlack(feed = $("conversationFeed")) {
    return feed.scrollHeight - feed.scrollTop - feed.clientHeight;
  }
  function scrollLatest(force = false) {
    const body = $("conversationFeed");
    if (force) state.pinToLatest = true;
    if (!state.pinToLatest && !force) return;
    if(!force&&state.working&&performance.now()<state.turnFocusUntil)return;
    if(state.scrollFrame)return;
    // Follow the stream on an easing curve rather than snapping scrollTop to the
    // bottom on every row. CSS scroll-behavior is deliberately not used here:
    // each new row would interrupt the previous smooth scroll and the feed would
    // stutter. One rAF that lerps toward the end and stops as soon as it lands
    // keeps the follow continuous and costs nothing once settled.
    const settle=()=>{
      state.scrollFrame=0;
      // A work row can schedule this callback before beginTurnActivity installs
      // the focus window. Re-check inside the frame so it cannot race the new
      // prompt back out of view.
      if(!force&&state.working&&performance.now()<state.turnFocusUntil)return;
      if(!state.pinToLatest&&!force)return;
      const target=body.scrollHeight-body.clientHeight,distance=target-body.scrollTop;
      if(distance<=0)return;
      const eased=document.documentElement.dataset.motion==="minimal"
        ||window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      // A long jump (thread switch, jump-to-latest) lands at once; only the
      // short deltas that streaming produces are worth easing.
      if(eased||distance>900||distance<1.5){body.scrollTop=target;return;}
      body.scrollTop+=Math.max(1,distance*0.22);
      state.scrollFrame=requestAnimationFrame(settle);
    };
    state.scrollFrame=requestAnimationFrame(settle);
  }
  function anchorConversationBottom(generation=state.loadGeneration,session=state.sessionId) {
    const feed=$("conversationFeed");if(!feed)return;
    if(state.scrollFrame){cancelAnimationFrame(state.scrollFrame);state.scrollFrame=0;}
    state.pinToLatest=true;
    const land=()=>{if(generation!==state.loadGeneration||session!==state.sessionId)return;feed.scrollTop=Math.max(0,feed.scrollHeight-feed.clientHeight);$("jumpLatest").hidden=true;updatePromptRailActive();};
    land();
    // Disclosure restoration and composer measurement both change the feed
    // height after its first paint. Land once per layout frame so opening a
    // conversation always starts at its newest message, never at the top.
    requestAnimationFrame(()=>{land();requestAnimationFrame(land);});
  }
  function onFeedScroll() {
    const feed=$("conversationFeed");
    if(feed.scrollTop<120&&performance.now()<state.historyScrollIntentUntil&&state.renderedRowStart>0&&!state.historyPagePending)queueOlderConversationTurns();
    clearTimeout(state.scrollIdle);
    state.scrollIdle = setTimeout(() => {
      const slack = feedSlack();
      if (state.working && performance.now() < state.turnFocusUntil) {
        state.pinToLatest = true;
        $("jumpLatest").hidden = true;
        updatePromptRailActive();
        return;
      }
      state.pinToLatest = slack < 48;
      $("jumpLatest").hidden = slack < 64;
      updatePromptRailActive();persistReadingPosition();
    }, 160);
  }
  function promptMessages() { return [...document.querySelectorAll("#conversationFeed .user-message")]; }
  function updatePromptRailActive() {
    const rail = $("conversationPromptRail"), messages = promptMessages(), buttons = [...rail.querySelectorAll("button")];
    if (!messages.length || !buttons.length || rail.hidden) return;
    const feed = $("conversationFeed");
    const mid = feed.scrollTop + feed.clientHeight * 0.43;
    let activeIndex = Number(buttons[0].dataset.messageIndex), distance = Infinity;
    buttons.forEach((button) => {
      const index = Number(button.dataset.messageIndex), message = messages[index]; if (!message) return;
      const next = Math.abs(message.offsetTop + message.offsetHeight / 2 - mid);
      if (next < distance) { distance = next; activeIndex = index; }
    });
    buttons.forEach((button) => {
      const active = Number(button.dataset.messageIndex) === activeIndex;
      button.classList.toggle("active", active);
      if (active) {
        button.setAttribute("aria-current", "true");
        const railTop=rail.scrollTop,railBottom=railTop+rail.clientHeight,top=button.offsetTop,bottom=top+button.offsetHeight;
        if(top<railTop+8)rail.scrollTo({top:Math.max(0,top-18),behavior:"smooth"});
        else if(bottom>railBottom-8)rail.scrollTo({top:bottom-rail.clientHeight+18,behavior:"smooth"});
      }
      else button.removeAttribute("aria-current");
    });
  }
  // Proximity rail: a dash grows with how close the pointer is, not just with a
  // direct hover, so the whole rail leans toward the cursor. Distance is read
  // once per frame from a cached geometry pass — no layout thrash per dash.
  // Prompt rail = beUI PreviewRail: the tick under the pointer is full
  // length and its neighbours shrink by distance (.68, .44, .25); one preview
  // card glides beside the rail to the highlighted prompt and swaps its text
  // with a short blur.
  const RAIL_SCALES=[1,.68,.44];
  function promptRailCard(){
    let card=$("promptRailCard");
    if(!card){card=document.createElement("div");card.id="promptRailCard";card.className="prompt-rail-card";card.setAttribute("aria-hidden","true");card.innerHTML='<div class="prompt-rail-card-body"><p class="prompt-rail-card-title"></p><p class="prompt-rail-card-desc"></p></div>';$("conversationPromptRail").parentElement.append(card);}
    return card;
  }
  function syncRailProximity(pointerY, focused=null) {
    const rail = $("conversationPromptRail");
    if (!rail || rail.hidden) return;
    const buttons = [...rail.querySelectorAll("button")];
    if (!buttons.length) return;
    let hovered=focused?buttons.indexOf(focused):-1;
    if(hovered<0&&pointerY!=null){let best=Infinity;buttons.forEach((button,index)=>{const box=button.getBoundingClientRect(),d=Math.abs(pointerY-(box.top+box.height/2));if(d<best&&d<=box.height){best=d;hovered=index;}});}
    buttons.forEach((button,index)=>{const distance=hovered<0?Infinity:Math.abs(index-hovered);button.style.setProperty("--tick",String(RAIL_SCALES[distance]??.25));button.classList.toggle("highlighted",index===hovered);});
    const card=promptRailCard();
    if(hovered<0){card.classList.remove("visible");return;}
    const button=buttons[hovered],preview=button.querySelector(".prompt-rail-preview"),host=card.parentElement.getBoundingClientRect(),box=button.getBoundingClientRect(),railBox=rail.getBoundingClientRect();
    const title=preview?.querySelector("strong")?.textContent||"",desc=preview?.querySelector("small")?.textContent||"";
    if(card.dataset.index!==String(hovered)){card.dataset.index=String(hovered);const body=card.querySelector(".prompt-rail-card-body");body.classList.remove("swap");void body.offsetWidth;body.classList.add("swap");card.querySelector(".prompt-rail-card-title").textContent=title;const d=card.querySelector(".prompt-rail-card-desc");d.textContent=desc;d.hidden=!desc;}
    const wasVisible=card.classList.contains("visible");
    card.style.left=`${railBox.right-host.left+16}px`;
    if(!wasVisible)card.style.transition="none";
    card.style.top=`${box.top+box.height/2-host.top}px`;
    if(!wasVisible){card.getBoundingClientRect();card.style.transition="";}
    card.classList.add("visible");
  }
  function scheduleRailProximity(pointerY) {
    state.railPointerY = pointerY;
    if (state.railProximityFrame) return;
    state.railProximityFrame = requestAnimationFrame(() => {
      state.railProximityFrame = 0;
      syncRailProximity(state.railPointerY);
    });
  }
  function togglePromptHistory(open, restoreFocus = false) {
    const button = $("historyButton"), rail = $("conversationPromptRail");
    if (!button || !rail) return;
    button.setAttribute("aria-expanded", String(open));
    rail.classList.toggle("history-open", open);
    if (open) rail.querySelector('button[aria-current="true"], button')?.focus();
    else if (restoreFocus) button.focus();
  }
  function renderPromptRail() {
    const rail = $("conversationPromptRail"), messages = promptMessages(), limit = Math.max(8,Math.min(20,Number(localStorage.getItem("phoenix-prompt-rail-count"))||20)), first = Math.max(0, messages.length - limit);
    rail.hidden = !messages.length;
    if ($("historyButton")) $("historyButton").disabled = !messages.length;
    rail.innerHTML = messages.slice(first).map((message, offset) => {
      const index=first+offset,files=message._messageAttachments||[],authored=String(message._messagePromptText||"").trim();
      const attachmentName=files.length===1?String(files[0].name||""):"";
      const attachmentTitle=!files.length?"":files.length>1?`${files.length} attachments`:attachmentName&&!/^sha256-[a-f0-9]{32,}/i.test(attachmentName)?attachmentName:files[0]&&isComposerImage(files[0])?"Attached image":"Attachment";
      const prompt=(authored||(!files.length?message.querySelector('.user-bubble')?.innerText:"")||attachmentTitle||"Your prompt").trim().replace(/\s+/g," ");
      let reply="",cursor=message.nextElementSibling;
      while(cursor&&!cursor.classList.contains("user-message")){if(cursor.matches(".agent-message,.group-message")){reply=(cursor.querySelector(".markdown")||cursor).innerText.trim().replace(/\s+/g," ");break;}cursor=cursor.nextElementSibling;}
      const title=prompt.length>64?`${prompt.slice(0,61).trimEnd()}…`:prompt,description=reply.length>92?`${reply.slice(0,89).trimEnd()}…`:reply;
      return `<button type="button" data-message-index="${index}" aria-label="Go to: ${escape(title)}"><i aria-hidden="true"></i><span class="prompt-rail-preview"><strong>${escape(title)}</strong>${description?`<small>${escape(description)}</small>`:""}</span></button>`;
    }).join("");
    updatePromptRailActive();
  }
  function jumpToPrompt(index) {
    const target = promptMessages()[Number(index)] || promptMessages().at(-1); if (!target) return;
    state.pinToLatest=false;target.scrollIntoView({ behavior:matchMedia("(prefers-reduced-motion: reduce)").matches?"auto":"smooth", block:"center" }); target.classList.add("flash"); setTimeout(() => target.classList.remove("flash"), 1100);
  }
  function clearFeed() { togglePromptHistory(false); stopTurnMood(); clearTimeout(state.reasoningCursorTimer);state.reasoningCursorTimer=0;if(state.scrollFrame){cancelAnimationFrame(state.scrollFrame);state.scrollFrame=0;} $("conversationFeed").replaceChildren(conversationTail); $("conversationFeed").classList.remove("turn-active","thread-switching"); $("conversationPromptRail").replaceChildren(); $("conversationPromptRail").hidden = true; state.turnStatus = null; state.activeTurnId=""; state.queuedWakeTurnId=""; state.replayWorkCluster = null; state.replayNeedsRepaint=false; state.pendingAnswer = null; state.pendingHandoffReturns=[]; state.activeTools.clear(); state.shimmerClusters.clear(); state.toolRows = []; state.taskSignature="";state.taskAllComplete=false;setComposerThreadState(false); }
  function detachChildren(node) {
    const fragment=document.createDocumentFragment();
    for(const child of [...(node?.childNodes||[])])if(child!==conversationTail)fragment.appendChild(child);
    return fragment;
  }
  function conversationScrollBookmark(feed=$("conversationFeed")) {
    if(!feed)return{top:0,bottom:0,pin:true,anchor:null,offset:0};
    const top=feed.scrollTop,bottom=Math.max(0,feed.scrollHeight-feed.clientHeight-top),pin=state.pinToLatest||bottom<48;
    const anchor=pin?null:[...feed.children].find((node)=>node.offsetTop+node.offsetHeight>=top)||null;
    return{top,bottom,pin,anchor,offset:anchor?anchor.offsetTop-top:0};
  }
  function restoreConversationScroll(bookmark,key=conversationIdentity()) {
    const feed=$("conversationFeed");if(!feed||!bookmark)return;
    if(state.scrollFrame){cancelAnimationFrame(state.scrollFrame);state.scrollFrame=0;}
    state.pinToLatest=Boolean(bookmark.pin);
    const land=()=>{
      if(key!==conversationIdentity()||!feed.isConnected)return;
      if(bookmark.pin)feed.scrollTop=Math.max(0,feed.scrollHeight-feed.clientHeight);
      else if(bookmark.anchor?.isConnected)feed.scrollTop=Math.max(0,bookmark.anchor.offsetTop-bookmark.offset);
      else feed.scrollTop=Math.max(0,Math.min(bookmark.top,feed.scrollHeight-feed.clientHeight));
      $("jumpLatest").hidden=state.pinToLatest||feedSlack(feed)<64;updatePromptRailActive();
    };
    land();requestAnimationFrame(()=>{land();requestAnimationFrame(land);});
  }
  function readingStorageKey(){return `phoenix-reading-position:${conversationIdentity()}`;}
  function persistReadingPosition(){
    const feed=$('conversationFeed');if(!state.item||state.painting||state.historyLoadFailed||feed.querySelector('.conversation-loading'))return;
    const bookmark=conversationScrollBookmark(feed),nodes=[...feed.children],anchor=bookmark.anchor;
    const value={top:bookmark.top,bottom:bookmark.bottom,pin:bookmark.pin,offset:bookmark.offset,turnId:anchor?.dataset.turnId||'',index:anchor?nodes.filter(n=>n.dataset.turnId===anchor.dataset.turnId).indexOf(anchor):0};
    try{localStorage.setItem(readingStorageKey(),JSON.stringify(value));}catch{}
  }
  function savedReadingPosition(){try{const value=JSON.parse(localStorage.getItem(readingStorageKey())||'null');return value&&typeof value.pin==='boolean'&&Number.isFinite(value.top)?value:null;}catch{return null;}}
  function restoreReadingPosition(value){
    if(!value)return;const feed=$('conversationFeed');
    if(!value.pin&&value.turnId){const row=state.displayRows.findIndex(entry=>displayTurnId(entry)===value.turnId);if(row>=0&&row<state.renderedRowStart){state.renderedRowStart=row;repaintConversation(null,false,false);}}
    const anchor=[...feed.children].filter(node=>node.dataset.turnId===value.turnId)[value.index||0]||null;restoreConversationScroll({...value,anchor});
  }
  function stashConversationView() {
    const key=conversationIdentity();if(!key)return null;window.PhoenixQuestionDrafts?.persistAll();persistReadingPosition();
    const feed=$("conversationFeed"),rail=$("conversationPromptRail"),approval=$("approvalStack"),bookmark=conversationScrollBookmark(feed);
    if(state.scrollFrame){cancelAnimationFrame(state.scrollFrame);state.scrollFrame=0;}
    clearTimeout(state.scrollIdle);state.scrollIdle=0;
    clearTimeout(state.reasoningCursorTimer);state.reasoningCursorTimer=0;
    const view={
      key,item:{...state.item},sessionId:state.sessionId,ready:!state.historyLoadFailed&&!feed.hasAttribute("aria-busy")&&!feed.querySelector(".conversation-loading"),feed:detachChildren(feed),rail:detachChildren(rail),approval:detachChildren(approval),bookmark,
      displayRows:state.displayRows,displayBytes:state.displayBytes,displayDirty:state.displayDirty,displayJournalUnsafe:state.displayJournalUnsafe,answerMeta:state.answerMeta,renderedRowStart:state.renderedRowStart,
      activeTools:state.activeTools,toolRows:state.toolRows,working:state.working,turnStatus:state.turnStatus,activeTurnId:state.activeTurnId,activeGroupAgentIds:[...state.activeGroupAgentIds],queuedWakeTurnId:state.queuedWakeTurnId,replayWorkCluster:state.replayWorkCluster,
      turnFocusUntil:state.turnFocusUntil,turnStartedAt:state.turnStartedAt,pendingAnswer:state.pendingAnswer,replayNeedsRepaint:state.replayNeedsRepaint,completionKeys:state.completionKeys,
      pendingHandoffReturns:state.pendingHandoffReturns,shimmerClusters:state.shimmerClusters,feedHasAgent:state.feedHasAgent,turnUsageSeen:state.turnUsageSeen,
      tasks:state.tasks,queue:state.queue,taskSignature:state.taskSignature,taskAllComplete:state.taskAllComplete,
    };
    state.conversationViews.delete(key);state.conversationViews.set(key,view);
    while(state.conversationViews.size>CONVERSATION_VIEW_CACHE_CAP)state.conversationViews.delete(state.conversationViews.keys().next().value);
    // clearFeed() owns the active renderer collections. Hand it fresh ones so
    // resetting the new surface cannot clear Maps/Sets retained by this view.
    state.activeTools=new Map();state.shimmerClusters=new Set();state.toolRows=[];
    return view;
  }
  function resetConversationRuntime() {
    clearFeed();closeApproval();replaceDisplayRows([],false);state.answerMeta=new Map();state.displayJournalUnsafe=false;
    state.activeTools=new Map();state.toolRows=[];state.working=false;state.turnStatus=null;state.activeTurnId="";state.activeGroupAgentIds=[];state.queuedWakeTurnId="";state.replayWorkCluster=null;state.renderedRowStart=0;state.historyPagePending=false;state.historyScrollIntentUntil=0;
    state.turnFocusUntil=0;state.turnStartedAt=0;state.pendingAnswer=null;state.replayNeedsRepaint=false;state.completionKeys=new Set();state.pendingHandoffReturns=[];state.shimmerClusters=new Set();state.feedHasAgent=false;state.turnUsageSeen=false;
    state.taskSignature="";state.taskAllComplete=false;$("composerZone").classList.remove("working");$("taskBlock").classList.remove("live");syncSendMode();renderTasks();renderQueue();syncActivitySummary();
  }
  function takeConversationView(key=conversationIdentity()) {
    const view=state.conversationViews.get(key);if(!view)return null;state.conversationViews.delete(key);return view;
  }
  function restoreConversationView(view) {
    if(!view||view.key!==conversationIdentity())return false;
    resetConversationRuntime();
    state.displayRows=view.displayRows;state.displayBytes=view.displayBytes;state.displayDirty=view.displayDirty;state.displayJournalUnsafe=view.displayJournalUnsafe;state.answerMeta=view.answerMeta;state.renderedRowStart=Number(view.renderedRowStart)||0;
    state.activeTools=view.activeTools;state.toolRows=view.toolRows;state.working=view.working;state.turnStatus=view.turnStatus;state.activeTurnId=view.activeTurnId;state.activeGroupAgentIds=[...(view.activeGroupAgentIds||[])];state.queuedWakeTurnId=view.queuedWakeTurnId;state.replayWorkCluster=view.replayWorkCluster;
    state.turnFocusUntil=view.turnFocusUntil;state.turnStartedAt=view.turnStartedAt;state.pendingAnswer=view.pendingAnswer;state.replayNeedsRepaint=view.replayNeedsRepaint;state.completionKeys=view.completionKeys;
    state.pendingHandoffReturns=view.pendingHandoffReturns;state.shimmerClusters=view.shimmerClusters;state.feedHasAgent=view.feedHasAgent;state.turnUsageSeen=view.turnUsageSeen;
    state.tasks=view.tasks||state.tasks;state.queue=view.queue||state.queue;state.taskSignature=view.taskSignature;state.taskAllComplete=view.taskAllComplete;
    $("conversationFeed").insertBefore(view.feed,conversationTail);$("conversationPromptRail").appendChild(view.rail);$("approvalStack").appendChild(view.approval);
    $("conversationPromptRail").hidden=!$("conversationPromptRail").children.length;setComposerThreadState(state.feedHasAgent);syncApprovalStack();
    $("composerZone").classList.toggle("working",state.working);$("taskBlock").classList.toggle("live",state.working&&Boolean(state.tasks?.length));syncSendMode();renderTasks();renderQueue();syncActivitySummary();
    restoreConversationScroll(view.bookmark,view.key);return true;
  }
  function renderConversationLoading() {
    const feed=$("conversationFeed"),profile=currentProfile();
    feed.setAttribute("aria-busy","true");
    const node=document.createElement("article");node.className="conversation-loading";node.innerHTML=`<span class="conversation-loading-avatar">${ui.avatarSvg(profile,state.item?.kind)}</span><span><strong>Opening ${escape(currentName())}</strong><small>Loading this conversation…</small></span><i aria-hidden="true"></i>`;feed.insertBefore(node,conversationTail);
  }
  function displayRowsEqual(left,right) {
    return left===right||(left.length===right.length&&left.every((entry,index)=>{const other=right[index];return entry.source===other?.source&&displayTurnId(entry)===displayTurnId(other)&&JSON.stringify(entry.value)===JSON.stringify(other.value);}));
  }
  function authoredTurnStarts(rows=state.displayRows){const starts=[];(rows||[]).forEach((entry,index)=>{if(isAuthoredBoundaryEntry(entry))starts.push(index);});return starts;}
  function recentConversationRowStart(rows=state.displayRows,count=state.initialVisibleTurns){const starts=authoredTurnStarts(rows),visible=Math.max(5,Math.min(50,Number(count)||5));return starts.length>visible?starts[starts.length-visible]:0;}
  function olderConversationRowStart(rows=state.displayRows,current=state.renderedRowStart,count=state.initialVisibleTurns){const starts=authoredTurnStarts(rows).filter((index)=>index<current),page=Math.max(5,Math.min(50,Number(count)||5));return starts.length?starts[Math.max(0,starts.length-page)]:0;}
  function normalizedConversationRowStart(rows=state.displayRows,current=state.renderedRowStart){return authoredTurnStarts(rows).filter((index)=>index<=current).at(-1)||0;}
  function historyLoader(){const button=document.createElement("button");button.type="button";button.className="conversation-history-loader";button.innerHTML='<span aria-hidden="true">↑</span><strong>Earlier messages</strong>';button.onclick=()=>queueOlderConversationTurns(true);return button;}
  function syncConversationWindowDebug(){const feed=$("conversationFeed");if(!feed)return;feed.dataset.totalRows=String(state.displayRows.length);feed.dataset.renderedRows=String(Math.max(0,state.displayRows.length-state.renderedRowStart));feed.dataset.renderedTurns=String(feed.querySelectorAll(":scope > .user-message").length);feed.dataset.hasEarlier=String(state.renderedRowStart>0);}
  function renderHistoryLoader(){const feed=$("conversationFeed");feed.querySelector(":scope > .conversation-history-loader")?.remove();if(state.renderedRowStart>0)feed.prepend(historyLoader());syncConversationWindowDebug();}
  function queueOlderConversationTurns(immediate=false){
    if(state.historyPagePending||state.renderedRowStart<=0)return;
    state.historyPagePending=true;
    const load=()=>{
      const feed=$("conversationFeed"),previousStart=state.renderedRowStart,nextStart=olderConversationRowStart(state.displayRows,previousStart),oldHeight=feed.scrollHeight,oldTop=feed.scrollTop;
      if(nextStart>=previousStart){state.historyPagePending=false;return;}
      const preserved=detachChildren(feed),saved={activeTurnId:state.activeTurnId,replayWorkCluster:state.replayWorkCluster,turnStatus:state.turnStatus,feedHasAgent:state.feedHasAgent};
      const loader=preserved.querySelector?.(".conversation-history-loader");loader?.remove();state.replayWorkCluster=null;state.painting=true;
      try{state.displayRows.slice(nextStart,previousStart).forEach(renderDisplayEntry);}finally{state.painting=false;}
      feed.insertBefore(preserved,conversationTail);state.renderedRowStart=nextStart;state.activeTurnId=saved.activeTurnId;state.replayWorkCluster=saved.replayWorkCluster;state.turnStatus=saved.turnStatus;state.feedHasAgent=saved.feedHasAgent;renderHistoryLoader();renderPromptRail();syncComposerFade();
      const land=()=>{feed.scrollTop=Math.max(0,oldTop+feed.scrollHeight-oldHeight);};land();requestAnimationFrame(()=>{land();requestAnimationFrame(land);});state.historyPagePending=false;
    };
    if(immediate)load();else queueMicrotask(load);
  }
  function repaintConversation(bookmark=null,followLatest=!bookmark,resetWindow=false) {
    const feed=$("conversationFeed");clearFeed();closeApproval();
    if(resetWindow)state.renderedRowStart=recentConversationRowStart();else state.renderedRowStart=normalizedConversationRowStart();
    paintFeed(()=>{state.displayRows.slice(state.renderedRowStart).forEach(renderDisplayEntry);if(!lastConversationRow())renderEmpty();renderHistoryLoader();},followLatest);
    feed.removeAttribute("aria-busy");
    if(bookmark&&!followLatest)restoreConversationScroll(bookmark);
  }
  function repaintOwnedTurn(turnId){
    const feed=$("conversationFeed"),nodes=[...feed.children].filter((node)=>node.dataset.turnId===turnId);
    if(!nodes.length)return; // Keep paged-out history out of the live tail.
    const bookmark=conversationScrollBookmark(),marker=document.createComment("owned turn");
    nodes[0].before(marker);
    const preserved=detachChildren(feed);
    nodes.forEach((node)=>node.remove());
    const keys=["activeTurnId","replayWorkCluster","turnStatus","feedHasAgent","activeTools","toolRows","shimmerClusters","activeGroupAgentIds","painting"];
    const saved=Object.fromEntries(keys.map((key)=>[key,state[key]]));
    state.activeTurnId=turnId;state.replayWorkCluster=null;state.turnStatus=null;state.feedHasAgent=false;
    state.activeTools=new Map();state.toolRows=[];state.shimmerClusters=new Set();state.painting=true;
    try{state.displayRows.filter((entry)=>displayTurnId(entry)===turnId).forEach(renderDisplayEntry);}
    finally{
      const replacement=detachChildren(feed);marker.replaceWith(replacement);feed.insertBefore(preserved,conversationTail);
      Object.assign(state,saved);renderPromptRail();restoreConversationScroll(bookmark);
    }
  }
  function renderConversationLoadError() {
    const feed=$("conversationFeed");feed.querySelector(".conversation-loading")?.remove();feed.querySelector(".conversation-empty")?.remove();
    feed.removeAttribute("aria-busy");
    if(feed.querySelector(".conversation-load-error"))return;
    const node=document.createElement("article");node.className="conversation-load-error";node.setAttribute("role","status");
    node.innerHTML='<strong>This conversation hasn’t finished loading.</strong><p>Your messages and draft are still saved. You can retry loading them.</p><button type="button">Retry loading</button>';
    const key=conversationIdentity();node.querySelector("button").onclick=()=>{if(key===conversationIdentity())selectConversation({item:{...state.item},sessionId:state.sessionId});};feed.prepend(node);
  }
  function renderEmpty() {
    if(state.historyLoadFailed){renderConversationLoadError();return;}
    const profile = currentProfile();
    feedNode("conversation-empty", `<span class="empty-avatar">${ui.avatarSvg(profile, state.item?.kind)}</span><strong>${escape(currentName())} is ready</strong><p>${escape(state.item?.kind === "group" ? profile?.description : profile?.description || "Ask anything. The thread stays with this coworker.")}</p>`);
  }
  function isCompactionText(text) {
    return String(text || "").includes("[AUTO-COMPACTED HISTORY");
  }
  function isCompactionEvent(event) {
    if (!event || typeof event !== "object") return false;
    return ["text", "markdown", "body", "subject", "detail", "preview"].some((key) => isCompactionText(event[key]));
  }
  function isInternalRuntimeText(text) {
    const value=String(text||"").trim().toLowerCase();
    return value.startsWith("[late ask answer]") || value.startsWith("[queued wake]") || value.startsWith("queued prompt queued_") || /^leader convergence for turn (?:turn_|leader[-_])/.test(value) || /^initiating\s+[a-z0-9_-]+\s+call\.?$/i.test(value);
  }
  function askAnswerPrompt(value) {
    const row=value&&typeof value==="object"?value:{text:value},origin=row.origin||null;
    if(origin?.kind==="ask_answer")return{detail:String(origin.display||row.text||"").trim(),askId:origin.ask_id||"",agentId:origin.agent_id||""};
    let source=String(row.text||"").trim(),agentId="";
    const addressed=source.match(/^@([a-z0-9_-]+)\s+/i);if(addressed){agentId=addressed[1];source=source.slice(addressed[0].length);}
    if(!source.toLowerCase().startsWith("[late ask answer]"))return null;
    // Current wording: 'answered the saved question ask-…: "…". Continue …'.
    const saved=source.match(/answered the saved question ([a-z]+-[0-9a-z]+): "([\s\S]*?)"\.\s+(?:Continue|This answer)/i);
    if(saved){const answers=[...saved[2].matchAll(/^\s*A:\s*(.*)$/gm)].map((match)=>match[1].trim()).filter(Boolean),detail=answers.length?answers.join("\n\n"):saved[2].trim();return detail?{detail,askId:saved[1],agentId}:null;}
    const startMarker='had already ended: "',endMarker='". This answer supersedes',start=source.indexOf(startMarker),end=source.lastIndexOf(endMarker);
    if(start<0||end<start+startMarker.length)return null;
    const raw=source.slice(start+startMarker.length,end),answers=[...raw.matchAll(/^\s*A:\s*(.*)$/gm)].map((match)=>match[1].trim()).filter(Boolean),detail=(answers.length?answers.join("\n\n"):raw.trim());
    return detail?{detail,askId:"",agentId}:null;
  }
  function runtimeFailureSummary(value){
    const text=String(value||"").replace(/\u00a0/g," ").trim().replace(/^\*\*\[[^\]\r\n]{1,120}\]\*\*\s*/,"");
    if(text.startsWith('I could not advance because my only next action repeated the already-blocked'))return 'This run stopped repeating an action. Your progress is saved.';
    if(!/^(?:The provider became unavailable after|Phoenix stopped this agent at a hard runtime boundary:|(?:This agent|The agent|The\s+`[^`]+`\s+agent|Phoenix)\s+could not complete (?:its|the) turn|## Result\s+[^\r\n]+ could not complete its provider-backed turn\.)/i.test(text))return null;
    if(/without confirmed external termination|termination was NOT CONFIRMED/i.test(text))return "A browser action timed out. Check its result before retrying.";
    if(/whole-turn deadline/i.test(text))return "This run timed out. Your progress is saved.";
    const auth=/authentication rejected|401|stored (?:oauth |auth )?token is expired|refresh token|oauth token refresh returned http \d+:.*sign in again|saved login rejected|sign.in expired/i.test(text);
    const quota=/usage.limit.reached|usage limit (?:has been )?reached|quota exceeded|insufficient_quota/i.test(text);
    if(auth&&quota)return "The available provider accounts could not continue. Your progress is saved.";
    if(quota)return "The provider reported a usage limit. Your progress is saved.";
    if(auth)return "Provider sign-in needs attention. Your progress is saved.";
    if(/provider/i.test(text))return "The provider could not continue. Your progress is saved.";
    return "This run stopped before finishing. Your progress is saved.";
  }
  function renderRuntimeFailure(text,agent,meta=null){
    const summary=runtimeFailureSummary(text);if(!summary)return null;
    const owner=canonicalAgentId(agent||state.item?.id||"phoenix"),key=answerKey(summary),turn=state.renderingTurnId||state.activeTurnId||"";
    const existing=[...$("conversationFeed").querySelectorAll('.runtime-error')].find(n=>n.dataset.answerKey===key&&n.dataset.turnId===turn&&canonicalAgentId(n.dataset.agentId)===owner);if(existing){const details=existing.querySelector('pre');if(details&&String(text).length>details.textContent.length)details.textContent=String(text);return existing;}
    const cluster=precedingWorkCluster(owner);if(cluster)settleWorkCluster(cluster,true,meta);
    const node=feedNode("message-row agent-message runtime-error",`${avatar(agentProfile(owner))}<div class="message-content"><header><strong>${escape(agentLabel(owner))}</strong></header><details class="runtime-error-details"><summary><span>${escape(summary)}</span><span class="runtime-error-help" aria-hidden="true">?</span><span class="sr-only"> Error details</span></summary><pre></pre></details></div>`,{agentId:owner,from:"assistant",slot:"message"});
    node.dataset.answerKey=key;node.querySelector('pre').textContent=String(text);return node;
  }
  function friendlyBoundaryFailure(text) {
    const value=String(text||"").trim();
    if(!/^Phoenix stopped this agent at a hard runtime boundary:/i.test(value))return null;
    if(/whole-turn deadline/i.test(value))return"This run took too long and stopped before it could finish. Any unfinished action was left unconfirmed.";
    return"This run stopped before it could finish. Any unfinished action was left unconfirmed.";
  }
  function internalReturnFailure(text) {
    const value=String(text||"").trim();
    const runtime=runtimeFailureSummary(value);if(runtime)return runtime;
    const boundary=friendlyBoundaryFailure(value);if(boundary)return boundary;
    if(/(?:execution-economy|replay slice|RECOVERY ROUTE|\"node_id\"|\"attempt_id\"|failed work:|ok work:)/i.test(value))return"This coworker stopped before finishing. Its internal retries were kept out of the conversation.";
    return humanFailureDetail(value).slice(0,180)||"This coworker could not finish that part.";
  }
  const INTERNAL_NOTICE=/^(?:memory lookup is taking longer|memory preload|librarian |members reported; the group leader will converge next$)/;
  function visibleNotice(value) {
    const text=String(value||"").trim(),lower=text.toLowerCase();if(!text)return null;
    if(lower.startsWith("queued prompt")||lower.startsWith("queued group turn"))return /failed|could not|stopped/.test(lower)?"A queued message needs review. Open the queue above the composer to inspect the saved reason before removing it or sending a new request.":null;
    if(lower.startsWith("queued message ready")||lower.startsWith("late popup answer queued")||lower.startsWith("popup answer queued")||lower.startsWith("provider-native context")||lower.startsWith("context auto-compacted")||lower.startsWith("context overflow recovered")||lower.startsWith("context overflow could not commit")||lower.startsWith("checkpoint ")||lower.startsWith("talk →")||lower==="after barrier"||lower.includes(" · trace "))return null;
    // Internal runtime status (a slow memory preload, warm-ups) is diagnostics,
    // not conversation: log it, never show it as a transcript row.
    if(INTERNAL_NOTICE.test(lower)){console.info("[phoenix] runtime notice:",text);return null;}
    return text.replace(/\bqueued_[a-f0-9]{12,}\b/gi,"").replace(/\b[0-9a-f]{8}-[0-9a-f-]{27,}\b/gi,"").replace(/\s{2,}/g," ").trim()||null;
  }
  function providerRetryParts(value){
    const text=String(value||"").trim(),match=text.match(/^provider temporarily unavailable:\s*(.*?);\s*retrying in\s+(\d+)s\s*\(attempt\s+(\d+)\)$/i);if(!match)return null;
    const message=safeRuntimeCopy(match[1]).replace(/^codex stream reported an error:\s*/i,"").replace(/^openai codex api error\s*\([^)]*\):\s*/i,"").trim();
    return{message:message||"The model provider is temporarily unavailable.",seconds:Math.max(0,Number(match[2])||0),attempt:Math.max(1,Number(match[3])||1)};
  }
  function isTransientProviderBoundary(value){const text=String(value||"").toLowerCase();return text.includes("overloaded")&&(text.includes("could not complete its turn")||text.includes("provider became unavailable")||text.includes("provider unavailable after"));}
  function safeRuntimeCopy(value) {
    return String(value || "")
      .replace(/\b(?:queued|ask|login|teaching|session|call|run|trace|tool)[_-][a-z0-9][a-z0-9_-]{7,}\b/gi, "")
      .replace(/\b[0-9a-f]{8}-[0-9a-f-]{27,}\b/gi, "")
      .replace(/\b(?:session|profile|credential|queue|ask|teaching|tool|trace)[ _-]?id\s*[:=]\s*[^\s,;]+/gi, "")
      .replace(/\s+([,.;:!?])/g, "$1")
      .replace(/[ \t]{2,}/g, " ")
      .replace(/\n[ \t]+/g, "\n")
      .trim();
  }
  function humanFailureDetail(value, tool = "") {
    const raw = String(value || "").trim();
    if (!raw) return "";
    const toolName = String(tool || "").toLowerCase();
    if (/Tool exceeded its bounded execution window/i.test(raw)) return /termination was NOT CONFIRMED|without confirmed/i.test(raw)?"This action timed out. Check its result before trying again.":"This action took too long and was stopped. Check any partial changes before trying again.";
    if (/Command FAILED with exit code/i.test(raw)) return "The command didn’t finish successfully. Open activity for the details.";
    if (toolName === "response_validation") return "Phoenix adjusted the approach before continuing.";
    if (/credential vault is locked|Passes is locked/i.test(raw)) return "Passes is locked.";
    if(/Browser input target changed during preparation/i.test(raw))return "The browser page changed before the click could be sent. Phoenix needs to check the page again.";
    if(/Browser screenshot timed out/i.test(raw))return "Phoenix could not prepare the browser view in time. The action was not sent.";
    const httpStatus=raw.match(/HTTP\s+(\d{3})\b/i);
    if(httpStatus){
      const webOwned=/browser|web_|crawl|scrape/.test(toolName);
      return webOwned?`Website request failed (HTTP ${httpStatus[1]}).`:`Request failed (HTTP ${httpStatus[1]}).`;
    }
    let message = raw;
    const jsonStart = raw.indexOf("{");
    if (jsonStart >= 0) {
      try {
        const payload = JSON.parse(raw.slice(jsonStart));
        const reason = payload?.error || payload?.message || payload?.detail || payload?.reason;
        if (reason != null && typeof reason !== "object") message = String(reason);
        const nearby = Array.isArray(payload?.closest)
          ? [...new Set(payload.closest.map((entry) => entry?.text).filter((text) => typeof text === "string" && text.trim()).map((text) => text.trim()))].slice(0, 3)
          : [];
        if (nearby.length) message += ` Nearby: ${nearby.join(", ")}.`;
      } catch {}
    }
    message = safeRuntimeCopy(message)
      .replace(/\b(?:cx|cy|x|y|width|height)\s*[:=]\s*-?\d+(?:\.\d+)?\b/gi, "")
      .replace(/\b(?:internal frame|accessibility id|backend id)\b/gi, "")
      .replace(/\s+([,.;:!?])/g, "$1")
      .replace(/[ \t]{2,}/g, " ")
      .trim();
    // Diagnostics are allowed to be rich internally, but a conversation row
    // never becomes a JSON viewer. Keep the failure useful and human-sized.
    if (!message || /^[\[{]/.test(message) || /[\[{][\s\S]*[\]}]/.test(message)) return "That action did not complete.";
    return message.length > 320 ? `${message.slice(0, 317).trimEnd()}…` : message;
  }
  function turnFailureCard(error, agent = targetAgent() || "phoenix") {
    const detail=String(error?.message||error||"").trim();
    if(/(?:stored (?:oauth|auth) token is expired|oauth (?:access )?token (?:is |has )?expired|expired (?:oauth )?(?:access )?token)/i.test(detail))return{kind:"card",agent,subject:"Provider sign-in expired",body:"Open Settings → Models & Providers → Providers, choose Sign in again for this account, then resend the message. Your model routes will stay unchanged.",ok:false};
    return{kind:"card",agent,subject:"Could not finish",body:humanFailureDetail(detail)||"Phoenix could not finish this message. Please try again.",diagnostics:detail,ok:false};
  }
  function humanMentions(value) {
    return String(value || "").replace(/(^|\s)@([a-z0-9_-]+)/gi,(whole,boundary,id)=>{
      const profile=knownAgentProfile(id);
      return profile?.display_name?`${boundary}@${profile.display_name}`:whole;
    });
  }
  function renderSentMentions(container) {
    if(!container)return;
    const profiles=ui.state.view?.directory.agents||[];
    const walker=document.createTreeWalker(container,NodeFilter.SHOW_TEXT);
    const nodes=[];
    while(walker.nextNode())if(!walker.currentNode.parentElement.closest("a,code,pre,.user-attachments,.sent-agent-mention"))nodes.push(walker.currentNode);
    for(const node of nodes){
      const text=node.textContent,fragment=document.createDocumentFragment();let cursor=0;
      // Plain names become chips in groups, matching the composer. Elsewhere
      // only explicit @mentions are decorated. Never rewrite code or links.
      const plainIds=state.item?.kind==="group"?null:new Set();
      while(cursor<text.length){
        const match=composerMentionMatch(text,cursor,true,plainIds,profiles,true);
        if(!match){fragment.append(document.createTextNode(text.slice(cursor)));break;}
        fragment.append(document.createTextNode(text.slice(cursor,match.from)));
        if(text[match.to]==="@"||text[match.from-1]==="@"){
          fragment.append(document.createTextNode(text.slice(match.from,match.to)));cursor=match.to;continue;
        }
        const chip=match.everyone?composerEveryoneToken():composerAgentToken(match.profile);
        chip.classList.add("sent-agent-mention");chip.removeAttribute("contenteditable");chip.removeAttribute("role");chip.removeAttribute("aria-label");
        delete chip.dataset.composerAgent;delete chip.dataset.composerLabel;
        chip.title=match.everyone?"Everyone":match.profile.display_name;
        fragment.append(chip);cursor=match.to;
      }
      node.replaceWith(fragment);
    }
  }
  function scheduledPrompt(value) {
    const row=value&&typeof value==="object"?value:{text:value},text=String(row.text||"").trim(),typed=row.origin?.kind==="routine"&&Boolean(row.turn_id);
    if(/^scheduled work\s*·/i.test(text))return{detail:text.replace(/^scheduled work\s*·\s*/i,"").trim(),turnId:row.turn_id||"",origin:row.origin||null};
    // Legacy rows have only the envelope. New rows carry typed origin + a
    // stable occurrence id; the parser remains solely as migration support and
    // to remove the model-facing envelope from the authored prompt.
    const match=text.match(/^\[cron\s+[^|\]]+\|\s*scheduled\s+[^\]]+\]\s*([\s\S]*)$/i);
    if(!match&&!typed)return null;
    const detail=(match?.[1]||text).trim();
    return{detail,turnId:row.turn_id||"",origin:row.origin||null};
  }
  function beginAuthoredBoundary() {
    if(state.painting&&state.replayWorkCluster?.isConnected)settleWorkCluster(state.replayWorkCluster,false);
    if(state.painting)state.replayWorkCluster=null;
  }
  function renderScheduledTurn(value) {
    const scheduled=scheduledPrompt(value);if(!scheduled)return null;
    beginAuthoredBoundary();
    const node=feedNode("message-row user-message routine-message",`<div class="message-content"><button type="button" class="turn-delete-button prompt-delete-button" data-delete-prompt aria-label="Permanently delete this routine turn and its complete response" title="Delete routine and response">${deleteIcon()}</button><div class="user-bubble routine-bubble"><div class="routine-marker"><span>${toolVisual("cron").svg}</span><strong>Routine</strong></div><div class="routine-prompt">${markdown(scheduled.detail)}</div></div></div>`,{slot:"message",from:"user",routineId:scheduled.origin?.routine_id||""});
    if(!state.painting)renderPromptRail();
    return node;
  }
  // One bubble per answer: the ask record and the resumed turn can both
  // describe the same reply (the history row may lack the ask id).
  function findAnswerBubble(askId,text){
    const clean=(value)=>String(value||"").replace(/\s+/g," ").trim();
    return [...$("conversationFeed").querySelectorAll(":scope > .answer-resume-message")].reverse().slice(0,4)
      .find((node)=>(askId&&node.dataset.askId===askId)||(text&&clean(node.querySelector(".answer-resume-prompt")?.textContent)===clean(text)))||null;
  }
  function renderAskAnswerTurn(value) {
    const answer=askAnswerPrompt(value);if(!answer)return null;
    // The resumed turn marks when the user really answered; a bubble drawn
    // earlier at the question's position moves here instead of doubling.
    findAnswerBubble(answer.askId,answer.detail)?.remove();
    beginAuthoredBoundary();
    const target=agentLabel(answer.agentId||state.item?.id||"phoenix");
    const node=feedNode("message-row user-message answer-resume-message",`<div class="message-content"><button type="button" class="turn-delete-button prompt-delete-button" data-delete-prompt aria-label="Permanently delete this answer and its complete response" title="Delete answer and response">${deleteIcon()}</button><div class="user-bubble answer-resume-bubble"><div class="answer-resume-marker"><span>${icons.reply||QUEUE_GLYPH}</span><strong>Answer to ${escape(target)}</strong></div><div class="answer-resume-prompt">${markdown(answer.detail)}</div></div></div>`,{slot:"message",from:"user",askId:answer.askId});
    if(!state.painting)renderPromptRail();
    return node;
  }
  function renderGroupContinuationTurn(value) {
    if(value?.origin?.kind!=="group_continuation")return null;
    beginAuthoredBoundary();
    const node=feedNode("message-row user-message routine-message group-continuation-message",`<div class="message-content"><div class="user-bubble routine-bubble"><div class="routine-marker"><span>${QUEUE_GLYPH}</span><strong>Group continuation</strong></div><div class="routine-prompt">${markdown(value.origin.display||"Prerequisites ready; remaining work is continuing.")}</div></div></div>`,{slot:"message",from:"user",originalTurnId:value.origin.original_turn_id||""});
    if(!state.painting)renderPromptRail();
    return node;
  }
  function hydrateUserMessageImages(node) {
    node?.querySelectorAll("[data-message-image]").forEach(async(button)=>{
      const file=node._messageAttachments?.[Number(button.dataset.messageImage)],image=button.querySelector("img");
      if(!file?.path||!image||image.getAttribute("src"))return;
      button.classList.add("loading");
      try{const source=file.preview||await ui.invoke("image_data_url",{path:file.path});if(!button.isConnected)return;file.preview=source;image.src=source;button.classList.remove("loading");syncActivitySummary();}catch{button.classList.remove("loading");button.classList.add("failed");}
    });
  }
  function renderUser(text, attachments = [], queued = null, options = null) {
    if (isCompactionText(text) && !attachments.length) return null;
    // The next authored prompt is the only replay boundary. Receipts, asks,
    // handoffs and tool calls all remain inside the preceding turn trace.
    beginAuthoredBoundary();
    // Messages that arrived from a chat app carry a "[via Telegram · Name]"
    // marker for the coworker; show it as a small label, not raw text.
    const via=/^\[via (Telegram|Discord) · ([^\]\n]{1,80})\]\n?/.exec(String(text||""));
    if(via)text=String(text).slice(via[0].length);
    state.replayWorkCluster=null;
    const visibleText=humanMentions(text);
    const files = attachments.length ? `<div class="user-attachments">${attachments.map((file,index) => isComposerImage(file)?`<button type="button" class="user-image-attachment" data-message-image="${index}" aria-label="Open ${escape(file.name||"image")} in activity sidebar"><img src="${escape(file.preview||"")}" alt="${escape(file.name||"Attached image")}"><strong>${escape(file.name||"Image")}</strong></button>`:`<span>${icons.work}<strong>${escape(file.name || "Attachment")}</strong></span>`).join("")}</div>` : "";
    const node = feedNode("message-row user-message", `<div class="message-content"><button type="button" class="turn-delete-button prompt-delete-button" data-delete-prompt aria-label="Permanently delete this prompt and its complete response" title="Delete prompt and response">${deleteIcon()}</button><div class="user-bubble" data-slot="message-bubble-content">${files}${markdown(visibleText)}</div>${via?`<span class="via-channel via-${via[1].toLowerCase()}">${via[1]==="Telegram"?'<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M16.5 4 3.5 9.2l4.6 1.6L14 6.6l-4.6 5v3.9l2.3-2.6 3 2.2L16.5 4Z"/></svg>':'<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M5 5.5c3-1.5 7-1.5 10 0l1.5 8c-1.5 1.2-3 1.8-4 2l-.8-1.4M8.3 14.1l-.8 1.4c-1-.2-2.5-.8-4-2L5 5.5"/><circle cx="7.8" cy="10.2" r="1"/><circle cx="12.2" cy="10.2" r="1"/></svg>'}via ${escape(via[1])} · ${escape(via[2])}</span>`:""}</div>`,{slot:"message",from:"user"});
    if(via)node.classList.add("from-channel");
    renderSentMentions(node.querySelector(".user-bubble"));
    node._messageAttachments=attachments;node._messagePromptText=visibleText;
    hydrateUserMessageImages(node);
    if(queued?.id){node.dataset.queuedId=queued.id;node.dataset.queuedPending=String(!queued.canonical);}
    if(options?.steered)markSteeredNode(node,true);
    if (!state.painting) renderPromptRail();
    syncActivitySummary();
    return node;
  }
  function normalizedAgentTalkSubject(value) {
    return String(value||"").replace(/\s*(?:—|-)\s*background return\s*$/i,"").trim();
  }
  function normalizedAgentTalkBody(value) {
    return String(value||"")
      .replace(/^\s*<!--\s*phoenix-message-priority:(low|normal|high|urgent)\s*-->\s*/i,"")
      // Runtime instruction that wraps a coworker's result for its owner;
      // never user-facing text.
      .replace(/^\s*A coworker returned this internal result while you remain accountable to the user\.[\s\S]*?\n\s*\n/i,"")
      .replace(/\s*<!--\s*phoenix-background-return:[^>]+-->\s*$/i,"")
      .trim();
  }
  function agentContextMessageMeta(row) {
    const body=String(row?.text||row?.body||""),marker=body.match(/^\s*<!--\s*phoenix-message-priority:(low|normal|high|urgent)\s*-->/i),status=String(row?.status||"").toLowerCase();
    if(!marker&&!status.includes("message_"))return null;
    return{priority:(marker?.[1]||(status.startsWith("priority_")?"high":"normal")).toLowerCase()};
  }
  function currentConversationAddresses() {
    if(state.item?.kind!=="agent")return new Set();
    const profile=currentProfile(),values=[state.item.id,currentName(),profile?.agent_id,profile?.internal_role];
    if(state.item.id==="phoenix")values.push("phoenix","orchestrator");
    return new Set(values.filter(Boolean).flatMap((value)=>[String(value).trim().toLowerCase(),canonicalAgentId(value)]));
  }
  function isIncomingAgentTalk(row) {
    return Boolean(row&&state.item?.kind==="agent"&&[String(row.to||"").trim().toLowerCase(),canonicalAgentId(row.to)].some((address)=>currentConversationAddresses().has(address)));
  }
  const OWNER_SCOPED_STORY_KINDS=new Set(["narration","commentary","reasoning","thinking","tool_start","tool","receipt","brief","settled","context","context_compaction","diff","usage","answer"]);
  // Canonical history from older builds labels legitimate owner tool rows as
  // `orchestrator`, so only terminal answers are safe to reject by an explicit
  // foreign speaker. Operational story rows already carry reliable ownership.
  const OWNER_SCOPED_HISTORY_ROLES=new Set(["answer","narration","commentary","reasoning","tool","context_compaction"]);
  function isInternalHistoryTool(row){return row?.role==="tool"&&/^__phoenix_/i.test(String(row.tool||""));}
  function storyVisibleInConversation(event) {
    if(!event?.kind)return false;
    if(["tool_start","tool"].includes(event.kind)&&String(event.tool||"").toLowerCase()==="response_validation")return false;
    if(state.item?.kind!=="agent"||!OWNER_SCOPED_STORY_KINDS.has(event.kind))return true;
    const owner=canonicalAgentId(state.item.id||"phoenix"),eventOwner=canonicalAgentId(event.agent||owner);
    return !eventOwner||eventOwner===owner;
  }
  function historyVisibleInConversation(row) {
    if(isInternalHistoryTool(row))return false;
    if(!row?.role||state.item?.kind!=="agent"||!OWNER_SCOPED_HISTORY_ROLES.has(row.role))return true;
    const owner=canonicalAgentId(state.item.id||"phoenix"),rowOwner=canonicalAgentId(row.agent||owner);
    return !rowOwner||rowOwner===owner;
  }
  function displayRowsVisibleInConversation(rows) {
    if(state.item?.kind==="group")return rows.filter((entry)=>displayRole(entry)!=="answer");
    if(state.item?.kind!=="agent")return rows;
    return rows.filter((entry)=>entry?.source==="story"?storyVisibleInConversation(entry.value):historyVisibleInConversation(entry?.value));
  }
  function isAuthoredBoundaryEntry(entry) {
    const role=displayRole(entry);
    // A coworker's completed result belongs to the owner's existing turn. It
    // is not a new user-authored boundary: treating it as one made the recent
    // turn window split at the return and, on reload, painted the coworker
    // below the owner's already-finished answer.
    return role==="user"||(role==="talk"&&isIncomingAgentTalk(entry?.value)&&!isCompletedAgentReturn(entry?.value));
  }
  function rawHandoffIds(row) {
    return [row?.reply_to,row?.causation_id,row?.handoff_id,row?.delegation_id,row?.work_id,row?.job_id]
      .filter((value)=>value!==undefined&&value!==null&&String(value).trim()!=="")
      .map((value)=>String(value));
  }
  function isCompletedAgentReturn(row) {
    if(!row||row.role!=="talk"||row.reply_expected===true||agentContextMessageMeta(row))return false;
    const status=String(row.status||"").toLowerCase();
    return Boolean(row.reply_to||row.reply_expected===false&&(rawHandoffIds(row).length||row.ok!==undefined)||/done|complete|returned|finished|blocked|failed/.test(status));
  }
  function isHandoffReturnEntry(entry) {
    return displayRole(entry)==="return"||(entry?.source==="history"&&isCompletedAgentReturn(entry.value));
  }
  function correlatedHandoffEntry(rows,returnEntry) {
    if(!isHandoffReturnEntry(returnEntry))return null;
    const returned=returnEntry.value||{},ids=new Set(rawHandoffIds(returned)),receiver=canonicalAgentId(returned.agent||returned.from||returned.receiver);
    const candidates=(rows||[]).filter((entry)=>["handoff","talk"].includes(displayRole(entry))&&!isHandoffReturnEntry(entry));
    const exact=[...candidates].reverse().find((entry)=>{
      const handoff=entry.value||{},handoffIds=rawHandoffIds(handoff);
      if(!handoffIds.some((id)=>ids.has(id)))return false;
      const target=canonicalAgentId(handoff.receiver||handoff.to||delegatedAgent(handoff));
      return !receiver||!target||receiver===target;
    });
    if(exact)return exact;
    if(ids.size)return null;
    const subject=handoffSubjectKey(returned.subject);
    return [...candidates].reverse().find((entry)=>{
      const handoff=entry.value||{},target=canonicalAgentId(handoff.receiver||handoff.to||delegatedAgent(handoff));
      return (!receiver||receiver===target)&&subject&&subject===handoffSubjectKey(handoff.subject||handoff.text);
    })||null;
  }
  function repairHandoffReturnOrder(rows) {
    let changed=false;
    (rows||[]).forEach((entry)=>{
      if(!isHandoffReturnEntry(entry))return;
      const handoff=correlatedHandoffEntry(rows,entry),turnId=displayTurnId(handoff);
      if(!turnId||displayTurnId(entry)===turnId)return;
      entry.turn_id=turnId;
      if(entry.value?.turn_id)entry.value.turn_id=turnId;
      changed=true;
    });
    const ordered=repairRecoveredRowOrder(rows||[]);
    if(ordered.some((entry,index)=>entry!==(rows||[])[index]))changed=true;
    return{rows:ordered,changed};
  }
  function incomingAgentTalkKey(entry) {
    const row=entry?.value||{},role=displayRole(entry);
    if(role==="return")return `incoming:${canonicalAgentId(row.agent||row.from)}:${normalizedAgentTalkSubject(row.subject)}:${normalizedAgentTalkBody(row.body||row.text)}`;
    if(role==="talk"&&isIncomingAgentTalk(row))return `incoming:${canonicalAgentId(row.from)}:${normalizedAgentTalkSubject(row.subject)}:${normalizedAgentTalkBody(row.text||row.body)}`;
    return "";
  }
  function initiatingAgentForTurn(event={}) {
    if(state.item?.kind!=="group")return canonicalAgentId(state.item?.id||"phoenix");
    const turn=event.turn_id||state.renderingTurnId||state.activeTurnId;
    const entries=state.displayRows.filter(entry=>displayTurnId(entry)===turn),values=entries.map(entry=>entry.value||{});
    const request=entries.find(entry=>displayRole(entry)==="user")?.value;
    const explicit=request?.initiating_agent_id||request?.origin?.agent_id;
    if(explicit)return canonicalAgentId(explicit);
    // Historical turns use their handoff ancestry; return arrival order is
    // never the authority for choosing who speaks for the request.
    const edges=values.filter(value=>value.kind==="handoff"||(value.role==="talk"&&!isCompletedAgentReturn(value)&&!agentContextMessageMeta(value))).map(value=>({from:canonicalAgentId(value.requester||value.from),to:canonicalAgentId(value.receiver||value.to)}));
    const receivers=new Set(edges.map(edge=>edge.to));
    const root=edges.find(edge=>edge.from&&!receivers.has(edge.from));
    if(root)return root.from;
    const start=values.find(value=>["group_member_status","reasoning","commentary","narration","tool_start","tool"].includes(value.kind||value.role)&&(value.agent_id||value.agent));
    if(start)return canonicalAgentId(start.agent_id||start.agent);
    const row=[...$("conversationFeed").querySelectorAll(".team-work-row")].find(node=>node.dataset.turnId===turn);
    return canonicalAgentId(row?.dataset.agent||state.activeGroupAgentIds[0]||event.agent_id||event.agent||event.from||"");
  }
  // A coworker ASKING this coworker for something is a request, not returned
  // work: it opens the turn on the right, like the user's own message, with a
  // small "from Tibo" label.
  function renderAgentRequest(event,from,body,id){
    const existing=[...$("conversationFeed").querySelectorAll(".from-agent")].find(node=>node.dataset.returnReceipt===id);
    if(existing)return existing;
    const node=renderUser(body);if(!node)return null;
    node.classList.add("from-agent");node.dataset.returnReceipt=id;node.dataset.agentId=from;
    const content=node.querySelector(".message-content"),bubble=node.querySelector(".user-bubble");
    // A long brief (IDs, rules, approvals) shows its first lines only.
    if(bubble&&body.length>420){
      bubble.classList.add("clamped");
      content.insertAdjacentHTML("beforeend",'<button type="button" class="request-more" aria-expanded="false">Show more</button>');
      const more=content.querySelector(".request-more");
      more.onclick=()=>{const open=bubble.classList.toggle("clamped")===false;more.textContent=open?"Show less":"Show more";more.setAttribute("aria-expanded",String(open));};
    }
    content?.insertAdjacentHTML("beforeend",`<span class="via-channel via-agent"><span class="agent-chip-avatar">${ui.avatarSvg(agentProfile(from))}</span>from ${escape(agentLabel(from))}</span>`);
    return node;
  }
  // This coworker writing to another one ("Rory → Leon Lin"): its own words,
  // shown in its chat like a message from it, with who it went to.
  function renderOutgoingAgentTalk(event,from,body,id){
    const feed=$("conversationFeed"),existing=[...feed.querySelectorAll(".outgoing-talk")].find((node)=>node.dataset.talkId===id);
    if(existing)return existing;
    const to=canonicalAgentId(event.to||event.receiver)||String(event.to||""),profile=agentProfile(from);
    if(!to||to===from)return null;
    const node=feedNode("message-row agent-message outgoing-talk",`${avatar(profile)}<div class="message-content"><header><strong>${escape(agentLabel(from))}</strong><small>to ${escape(agentLabel(to))}</small></header><div class="markdown">${markdown(body)}</div></div>`,{talkId:id});
    return node;
  }
  function renderIncomingAgentTalk(event) {
    const from=canonicalAgentId(event.from||event.agent)||"agent",body=normalizedAgentTalkBody(event.text||event.body||event.subject),label=agentLabel(from),ok=returnSucceeded(event);
    if(!body)return null;
    // Your answer reaching a running turn: it sits where you answered.
    if(String(event.from||"").trim().toLowerCase()==="user"){const answered=renderAskAnswerTurn({text:event.text||event.body||""});if(answered)return answered;}
    const turn=event.turn_id||state.renderingTurnId||state.activeTurnId||"",id=answerKey(`${turn}|${from}|${body}`),feed=$("conversationFeed");
    // This coworker's own outgoing message (Tibo writing to Rory) is not
    // something that came back to it; never show it as returned work here.
    if(state.item?.kind==="agent"&&from===canonicalAgentId(state.item.id))return renderOutgoingAgentTalk(event,from,body,id);
    if(event.role==="talk"&&state.item?.kind==="agent"&&from!==canonicalAgentId(state.item.id)&&!isCompletedAgentReturn(event)&&!agentContextMessageMeta(event))return renderAgentRequest(event,from,body,id);
    const existing=[...feed.querySelectorAll(".coworker-return")].find(node=>node.dataset.returnReceipt===id);
    if(existing)return existing;
    if(state.item?.kind==="group"&&ok)return renderRoomMemberMessage(event,from,body,id);
    const cluster=ensureWorkCluster(state.item?.kind==="group"?from:(state.item?.id||"phoenix"));
    const node=document.createElement("details");node.className="coworker-return";node.dataset.returnReceipt=id;node.dataset.agentId=from;
    node.innerHTML=`<summary><span class="agent-chip-avatar">${ui.avatarSvg(agentProfile(from))}</span><strong>${escape(label)}</strong><span>${ok?"Returned work":"Needs attention"}</span><svg viewBox="0 0 20 20" aria-hidden="true"><path d="m6 8 4 4 4-4"/></svg></summary><div class="markdown">${relayedImagesMarkup(body)}${markdown(body)}</div>`;
    cluster.querySelector(".work-tools").append(node);cluster.classList.add("has-tools");
    if(state.item?.kind==="group"){cluster.dataset.teamState=ok&&event.historical&&event.status==="returned"&&event.ok==null?"returned":ok?"done":"blocked";settleWorkCluster(cluster,false);}
    else syncTeamWorkSummary(cluster);
    hydrateRelayedImages(node);return node;
  }
  function renderRoomMemberMessage(event,from,body,id){
    const feed=$("conversationFeed"),prior=[...feed.querySelectorAll(":scope > .member-message")].find(node=>node.dataset.returnReceipt===id);
    if(prior)return prior;
    const profile=agentProfile(from),text=visibleAnswerText(body,from);
    const node=feedNode("message-row group-message member-message",`${avatar(profile)}<div class="message-content" data-slot="message-content"><header><strong>${escape(agentLabel(from))}</strong></header><div class="markdown">${relayedImagesMarkup(text)}${markdown(text)}</div></div>`,{agentId:from,returnReceipt:id,slot:"message",from:"assistant"});
    if(event.turn_id)node.dataset.turnId=String(event.turn_id);
    PhoenixConversationUpdates.finalize(node);
    node.style.setProperty("--speaker",profile?.color||"var(--ember)");
    // A room is chronological: the leader's plan (and its assignments) comes
    // before the pitches it asked for, so a teammate is never moved above an
    // earlier leader message. Events and history both arrive in commit order.
    const cluster=ensureWorkCluster(from);
    if(cluster){cluster.dataset.teamState=event.historical&&event.status==="returned"&&event.ok==null?"returned":"done";settleWorkCluster(cluster,false);}
    hydrateRelayedImages(node);return node;
  }
  function answerKey(text) {
    let hash = 2166136261;
    for (const character of String(text || "")) { hash ^= character.codePointAt(0); hash = Math.imul(hash, 16777619); }
    return `answer:${(hash >>> 0).toString(36)}`;
  }
  function parseAnswerMeta(feeds) {
    const rows = new Map();
    Object.entries(feeds || {}).forEach(([key, value]) => {
      if (!key.startsWith("answer:") || typeof value !== "string") return;
      try { const meta=JSON.parse(value); if(meta&&typeof meta==="object")rows.set(key,meta); } catch {}
    });
    return rows;
  }
  function cloneDisplayValue(value) {
    try { return JSON.parse(JSON.stringify(value)); } catch { return null; }
  }
  function displayRole(entry) { return entry?.source==="story"?entry.value?.kind:entry?.value?.role; }
  function newTurnId() { return `turn-${Date.now().toString(36)}-${crypto.randomUUID?.()||Math.random().toString(36).slice(2)}`; }
  function ensureDisplayTurnIds(rows) {
    let current="";
    rows.forEach((entry,index)=>{
      if(isAuthoredBoundaryEntry(entry))current=entry.turn_id||entry.value?.turn_id||`legacy-${index}-${answerKey(entry.value?.text||entry.value?.body||entry.value?.subject||"")}`;
      if(!entry.turn_id&&current&&!entry.recovered_ask_unplaced)entry.turn_id=current;
    });
    return rows;
  }
  function displaySemantic(entry) {
    const row=entry?.value||{},role=entry?.source==="story"?row.kind:row.role;
    // Canonical recovery rows do not always carry the renderer's local turn
    // id. Comparing one side by turn id and the other by text made every
    // catch-up append the same prompts again. Counts are reconciled as a
    // multiset, so text identity still preserves intentionally repeated
    // identical prompts without allowing replay storms.
    if(role==="user"&&row.origin?.kind==="routine"&&row.turn_id)return `turn:${row.turn_id}:user`;
    if(role==="user"&&askAnswerPrompt(row))return `user:${askAnswerPrompt(row).detail}`;
    if(role==="user"){
      // The live echo and saved history differ in whitespace ("] Ask" vs
      // "]\nAsk" after a chat-app marker); that made a second bubble.
      const visible=String(visibleUserPromptText(row)||"").replace(/\s+/g," ").trim();
      return `user:${state.item?.kind==="group"?canonicalGroupPromptText(visible):visible}`;
    }
    if(role==="answer")return `answer:${canonicalAnswerText(row.text||row.markdown||"")}`;
    // Provider retries can persist the same contribution under a fresh
    // message id.  Within one authored turn, visible identity is the stable
    // speaker plus canonical body; the turn id check in equivalentDisplayRows
    // still preserves a deliberately repeated answer to a later prompt.
    if(role==="group_message")return `group-message:${canonicalAgentId(row.agent_id||row.agent)}:${canonicalAnswerText(row.markdown||row.text||row.body||"")}`;
    if(role==="ask_pending"||role==="ask")return `ask:${row.id||row.ask_id||""}`;
    // The lossless session history historically attributed nested coworker
    // calls to `orchestrator`, while the live story carried the human agent.
    // They are still the same receipt. Agent identity is already expressed by
    // the cluster, so excluding that legacy attribution prevents a second copy
    // on every reopen without merging distinct calls (target/detail remain).
    if(role==="tool")return `tool:${row.tool||""}:${row.target||""}:${row.ok!==false}:${row.detail||""}`;
    if(role==="narration"||role==="commentary"||role==="reasoning")return `progress:${normalizedProgressText(row.text||"")}`;
    if(role==="talk"&&agentContextMessageMeta(row))return `context-message:${handoffExplicitId(row)||`${canonicalAgentId(row.from)}:${canonicalAgentId(row.to)}:${normalizedAgentTalkSubject(row.subject)}:${normalizedAgentTalkBody(row.text||row.body)}`}`;
    if(role==="talk"&&isIncomingAgentTalk(row))return incomingAgentTalkKey(entry);
    // A completed return deliberately carries the same backend correlation id
    // as its originating handoff. Keep the role in the display key so an
    // adjacent return settles that handoff instead of being discarded as a
    // replay of the assignment row itself. Replays of either row still upsert
    // because rows of the same role retain the same semantic key.
    if(role==="return")return `return:${returnExplicitId(row)||incomingAgentTalkKey(entry)}`;
    if(role==="talk"||role==="handoff")return `handoff:${handoffExplicitId(row)||`${canonicalAgentId(delegatedAgent(row))}:${normalizedAgentTalkSubject(row.subject||row.text)}`}`;
    return `${role||"row"}:${JSON.stringify(row)}`;
  }
  function canonicalAnswerText(value) {
    return String(value||"").replace(/^\s*(?:\*\*|__)?\[[a-z][a-z0-9_-]{1,63}\](?:\*\*|__)?[ \t]*/i,"").trim();
  }
  function generatedImageCommentSplit(row) {
    const raw=String(row?.text||""),attachments=Array.isArray(row?.attachments)?row.attachments:[];
    if(!attachments.length)return{visible:raw.trim(),generated:""};
    const marker=raw.match(/\n{2,}Image comments:\s*\n/i);
    if(!marker)return{visible:raw.trim(),generated:""};
    const generated=raw.slice(marker.index+marker[0].length),names=new Set(attachments.map((file)=>String(file?.name||"").trim()).filter(Boolean));
    const named=[...generated.matchAll(/^\*\*([^*\n]+)\*\*\s*$/gm)].map((match)=>match[1].trim());
    if(!named.length||named.some((name)=>!names.has(name))||!/^\s*\*\*[^*\n]+\*\*\s*\n\s*\d+\.\s+\d+% from the left,\s*\d+% from the top\s+—\s+/m.test(generated))return{visible:raw.trim(),generated:""};
    return{visible:raw.slice(0,marker.index).trim(),generated};
  }
  function visibleUserPromptText(row){return generatedImageCommentSplit(row).visible;}
  function attachmentIdentity(row){return(Array.isArray(row?.attachments)?row.attachments:[]).map((file)=>`${file?.path||""}\u0000${file?.name||""}`).sort().join("\u0001");}
  function imageCommentUserMirrors(left,right){
    const a=left?.value||{},b=right?.value||{},aSplit=generatedImageCommentSplit(a),bSplit=generatedImageCommentSplit(b);
    if(!aSplit.generated&&!bSplit.generated)return false;
    return aSplit.visible===bSplit.visible&&Boolean(attachmentIdentity(a))&&attachmentIdentity(a)===attachmentIdentity(b);
  }
  function preferredImageCommentUser(left,right){
    const a=left?.value||{},b=right?.value||{},aGenerated=Boolean(generatedImageCommentSplit(a).generated),bGenerated=Boolean(generatedImageCommentSplit(b).generated);
    const preferred=!aGenerated&&bGenerated?left:aGenerated&&!bGenerated?right:(String(a.text||"").length<=String(b.text||"").length?left:right),other=preferred===left?right:left;
    preferred.value.attachments=preferred.value.attachments?.length?preferred.value.attachments:cloneDisplayValue(other.value.attachments||[]);
    const stable=[displayTurnId(left),displayTurnId(right)].find((id)=>id&&!/^legacy-/i.test(id));
    if(stable){preferred.turn_id=stable;preferred.value.turn_id ||= stable;}
    return preferred;
  }
  const pendingOwnedStories=new Map();
  function ownedStoryTurn(value){return String(value?.execution?.turn_id||"");}
  function ownedStoryEventKey(value){return value?.execution&&value.event_sequence!=null?JSON.stringify([value.execution.task_id,value.execution.attempt_id,value.event_sequence,value.kind,canonicalAgentId(value.agent_id||value.agent||value.from||"")]):"";}
  function ownedStoryHasBoundary(event){return state.displayRows.some((row)=>isAuthoredBoundaryEntry(row)&&displayTurnId(row)===ownedStoryTurn(event));}
  function deferOwnedStory(event,replay){
    const key=conversationIdentity(),pending=pendingOwnedStories.get(key)||[];
    const identity=(row)=>JSON.stringify(row.event);
    const entry={event:cloneDisplayValue(event),replay};
    if(!pending.some((row)=>identity(row)===identity(entry)))pending.push(entry);
    if(pending.length>DISPLAY_ROW_CAP)pending.splice(0,pending.length-DISPLAY_ROW_CAP);
    pendingOwnedStories.delete(key);pendingOwnedStories.set(key,pending);
    while(pendingOwnedStories.size>24)pendingOwnedStories.delete(pendingOwnedStories.keys().next().value);
  }
  function flushOwnedStories(){
    const key=conversationIdentity(),pending=pendingOwnedStories.get(key)||[],ready=pending.filter((row)=>ownedStoryHasBoundary(row.event));
    pendingOwnedStories.set(key,pending.filter((row)=>!ownedStoryHasBoundary(row.event)));
    ready.forEach(({event,replay})=>renderStory(event,replay));
  }
  function displayTurnId(entry) { return String(ownedStoryTurn(entry?.value)||entry?.turn_id||entry?.value?.turn_id||""); }
  function normalizedToolTarget(value) { return String(value||"").replace(/\s+/g," ").trim().replace(/(?:…|\.{3})$/u,"").trimEnd(); }
  function compatibleToolTargets(left,right) {
    const a=normalizedToolTarget(left),b=normalizedToolTarget(right);
    if(a===b)return true;
    // Story receipts intentionally shorten large tool inputs. A canonical
    // history row is the same call when either retained target is a meaningful
    // prefix of the other; short targets still require exact equality.
    return Math.min(a.length,b.length)>=24&&(a.startsWith(b)||b.startsWith(a));
  }
  function equivalentDisplayRows(left,right) {
    const leftRole=displayRole(left),rightRole=displayRole(right);
    // Server recovery and a live/client-journal event can describe the same
    // published delivery. Match original turn and route IDs before legacy text
    // keys; a reply's delivery ID can differ from the handoff it answers.
    const saved=left?.value||{},other=right?.value||{};
    if(leftRole==="user"&&rightRole==="user"&&((saved.steer_id&&saved.steer_id===other.turn_id)||(other.steer_id&&other.steer_id===saved.turn_id)))return true;
    if(state.item?.kind==="group"&&(saved.historical===true||other.historical===true)){
      const turn=displayTurnId(left);
      if(!turn||turn!==displayTurnId(right)||(saved.group_id&&other.group_id&&saved.group_id!==other.group_id))return false;
      if(saved.history_id&&other.history_id)return saved.history_id===other.history_id;
      if(leftRole==="group_member_status"&&rightRole===leftRole){
        const history=saved.historical?saved:other,live=saved.historical?other:saved;
        return canonicalAgentId(saved.agent_id||saved.agent)===canonicalAgentId(other.agent_id||other.agent)
          &&(saved.state===other.state||(history.state==="inactive"&&["queued","working"].includes(live.state)));
      }
      const category=(role,row)=>role==="talk"?(isCompletedAgentReturn(row)?"return":"handoff"):role;
      const role=category(leftRole,saved);
      if(["handoff","return"].includes(role)&&role===category(rightRole,other)){
        const route=(row)=>role==="return"?[row.reply_to||row.handoff_id,row.receiver||row.agent||row.from,row.requester||row.to]
          :[row.handoff_id,row.requester||row.from,row.receiver||row.to];
        const a=route(saved),b=route(other);
        if(!a.every(Boolean)||!b.every(Boolean)||a[0]!==b[0]||canonicalAgentId(a[1])!==canonicalAgentId(b[1])||canonicalAgentId(a[2])!==canonicalAgentId(b[2]))return false;
        if(role==="return")return saved.message_id&&other.message_id?saved.message_id===other.message_id
          :normalizedAgentTalkBody(saved.body||saved.text)===normalizedAgentTalkBody(other.body||other.text);
        return true;
      }
    }
    if(leftRole==="group_message"&&rightRole==="group_message"){
      const leftId=String(left?.value?.message_id||""),rightId=String(right?.value?.message_id||"");
      if(leftId||rightId)return Boolean(leftId&&rightId&&leftId===rightId);
    }
    const leftIncoming=incomingAgentTalkKey(left),rightIncoming=incomingAgentTalkKey(right);
    if(leftIncoming||rightIncoming)return Boolean(leftIncoming&&rightIncoming&&leftIncoming===rightIncoming);
    const handoffRoles=new Set(["talk","handoff"]);
    if(handoffRoles.has(leftRole)&&handoffRoles.has(rightRole))return displaySemantic(left)===displaySemantic(right);
    // Canonical history calls live authored commentary "narration".
    // These are two transports for the same update, not two messages.
    if(left.source!==right.source&&["narration","commentary"].includes(leftRole)&&["narration","commentary"].includes(rightRole)){
      const a=displayTurnId(left),b=displayTurnId(right);
      return (!a||!b||a===b)&&(state.item?.kind!=="group"||displayAgentId(left)===displayAgentId(right))&&displaySemantic(left)===displaySemantic(right);
    }
    if(leftRole!==rightRole)return false;
    if(leftRole==="user"&&imageCommentUserMirrors(left,right))return true;
    const leftTurn=displayTurnId(left),rightTurn=displayTurnId(right);
    // A saved group boundary is the authored identity, including after a
    // coworker rename changes the rendered mention text.
    if(leftRole==="user"&&state.item?.kind==="group"&&leftTurn&&rightTurn&&!/^legacy-/i.test(leftTurn)&&!/^legacy-/i.test(rightTurn))return leftTurn===rightTurn;
    if(leftTurn&&rightTurn&&leftTurn!==rightTurn)return false;
    const a=left?.value||{},b=right?.value||{};
    if(leftRole==="answer")return canonicalAnswerText(a.text||a.markdown||"")===canonicalAnswerText(b.text||b.markdown||"");
    const leftEvent=ownedStoryEventKey(a),rightEvent=ownedStoryEventKey(b);
    if(leftEvent&&rightEvent&&leftEvent!==rightEvent)return false;
    if(leftRole==="tool")return String(a.tool||"")===String(b.tool||"")&&(a.ok!==false)===(b.ok!==false)&&compatibleToolTargets(a.target,b.target);
    return displaySemantic(left)===displaySemantic(right);
  }
  const PRE_ANSWER_RECOVERY_ROLES=new Set(["narration","commentary","reasoning","tool_start","tool","receipt","handoff","return","card","brief","failure","context","diff","ask_pending","notice"]);
  function displayAgentId(entry){const row=entry?.value||{};return canonicalAgentId(row.agent_id||row.agent||row.from||"");}
  function recoveredInsertionIndex(rows,entry) {
    const role=displayRole(entry),effectiveRole=isHandoffReturnEntry(entry)?"return":role==="talk"&&!isIncomingAgentTalk(entry?.value)?"handoff":role,turnId=displayTurnId(entry);
    if(!turnId||!PRE_ANSWER_RECOVERY_ROLES.has(effectiveRole))return-1;
    const agent=displayAgentId(entry);
    return rows.findIndex((candidate)=>{
      if(displayTurnId(candidate)!==turnId)return false;
      const candidateRole=displayRole(candidate);
      if(candidateRole==="answer")return state.item?.kind!=="group";
      return candidateRole==="group_message"&&Boolean(agent)&&displayAgentId(candidate)===agent;
    });
  }
  function repairRecoveredRowOrder(rows) {
    const ordered=[];
    rows.forEach((entry)=>{const index=recoveredInsertionIndex(ordered,entry);if(index<0)ordered.push(entry);else ordered.splice(index,0,entry);});
    return ordered;
  }
  function askIsPending(ask) {
    return String(ask?.status||"pending").toLowerCase()==="pending"&&!ask?.resolved_at;
  }
  function pruneLateResolvedAskGhosts(rows) {
    // Old ConversationAsks reconciliation appended every historical answer to
    // the newest turn.  A question can never legitimately begin after that
    // turn's terminal answer, so remove only that impossible replay shape and
    // retain resolved cards that still sit in their original authored place.
    const answeredTurns=new Set();
    return (rows||[]).filter((entry)=>{
      const role=displayRole(entry),turnId=displayTurnId(entry);
      if(role==="answer"&&turnId)answeredTurns.add(turnId);
      if(role!=="ask_pending"||!turnId||!answeredTurns.has(turnId))return true;
      return askIsPending(entry.value);
    });
  }
  function latestTurnIsTerminal() {
    const owner=state.item?.kind==="agent"?canonicalAgentId(state.item.id||"phoenix"):"";
    for(let index=state.displayRows.length-1;index>=0;index-=1){
      const entry=state.displayRows[index],row=entry?.value||{},role=entry?.source==="story"?row.kind:row.role;
      if(role==="answer")return !row.awaiting_input;
      if(role==="settled"){
        // Nested coworkers share the parent event channel. Their hidden
        // lifecycle rows must not terminate the owner's still-running turn.
        if(owner&&canonicalAgentId(row.agent||owner)!==owner)continue;
        return true;
      }
      if(isAuthoredBoundaryEntry(entry))return false;
    }
    return false;
  }
  const LIVE_CONVERSATION_STATUSES=new Set(["working","reviewing","reasoning","using_tool"]);
  function selectedConversationIsLive() {
    const activity=ui.activityFor(state.item);
    // The gateway's status is authoritative. An answer on screen does not
    // end a task: after coworkers report back, the owner keeps working and
    // answers again, and queued messages wait for that real end.
    return LIVE_CONVERSATION_STATUSES.has(activity?.status)||Boolean(liveDelegate());
  }
  // A one-to-one chat whose latest message was handed to a coworker stays
  // "working" while that coworker works, and Stop reaches it: the owner's own
  // turn ended as soon as it delegated.
  function liveDelegate() {
    if(state.item?.kind!=="agent")return null;
    const feed=$("conversationFeed"),prompts=feed.querySelectorAll(":scope > .user-message"),last=prompts[prompts.length-1];
    const row=handoffRows().filter((node)=>!handoffIsSettled(node)&&(!last||(last.compareDocumentPosition(node)&Node.DOCUMENT_POSITION_FOLLOWING))).at(-1);
    const receiver=row&&agentProfile(row.dataset.handoffTo);
    if(!receiver||canonicalAgentId(receiver.agent_id)===canonicalAgentId(state.item.id))return null;
    const theirs=ui.activityFor?.({kind:"agent",id:receiver.agent_id});
    return LIVE_CONVERSATION_STATUSES.has(theirs?.status)&&theirs.canonical_session_id?{agentId:receiver.agent_id,sessionId:theirs.canonical_session_id}:null;
  }
  function activeGroupForDirectAgent() {
    if(state.item?.kind!=="agent")return null;
    const agentId=state.item.id,activities=ui.state.view?.activities||[];
    return activities.find((activity)=>activity?.item?.kind==="group"&&LIVE_CONVERSATION_STATUSES.has(activity.status)&&(activity.active_agent_ids||[]).includes(agentId))||null;
  }
  function syncWorkingElsewhere() {
    const banner=$("workingElsewhereBanner"),activity=activeGroupForDirectAgent();
    if(!banner)return;
    if(!activity){banner.hidden=true;banner.onclick=null;return;}
    const item=activity.item,name=ui.displayName(item,ui.profileFor(item))||"a group";
    banner.textContent=`${currentName()} is currently working in ${name}`;banner.hidden=false;banner.onclick=()=>ui.selectItem(item);
  }
  function syncSelectedLiveActivity() {
    syncWorkingElsewhere();
    syncTeamPresence();
    if(!selectedConversationIsLive()){
      // "Working" adopted from the gateway (after a reload, or a turn started
      // from Telegram) ends when the gateway says the coworker stopped; this
      // window owns no socket that would otherwise clear it. A stale flag made
      // the next message look queued behind a finished turn.
      if((state.item?.kind==="group"&&ui.activityFor(state.item)||state.item?.kind==="agent")&&state.working&&!state.turnSocket)setWorking(false);
      return false;
    }
    if(state.working)return false;
    const prompts=[...$("conversationFeed").querySelectorAll(":scope > .user-message:not([data-queued-pending=\"true\"])")];
    state.pinToLatest=true;
    state.turnStartedAt=Date.now();
    setWorking(true);
    beginTurnActivity(prompts.at(-1)||null);
    syncTeamPresence();
    return true;
  }
  function trimDisplayRows(rows) {
    if(!rows.length)return[];
    const groups=[];let group=[];
    rows.forEach((entry)=>{if(isAuthoredBoundaryEntry(entry)&&group.length){groups.push(group);group=[];}group.push(entry);});
    if(group.length)groups.push(group);
    let start=0,rowCount=rows.length,byteCount=32+groups.reduce((sum,turn)=>sum+JSON.stringify(turn).length+1,0);
    // Retention is turn-atomic. A long newest turn may exceed the soft target,
    // but its Routine/User boundary is never discarded while its response is
    // kept. Older complete turns are the only eviction unit.
    while(start<groups.length-1&&(rowCount>DISPLAY_ROW_CAP||byteCount>DISPLAY_BYTE_CAP)){
      rowCount-=groups[start].length;byteCount-=JSON.stringify(groups[start]).length+1;start+=1;
    }
    return groups.slice(start).flat();
  }
  function displayRowsBytes(rows) {
    return 32+rows.reduce((sum,entry)=>sum+JSON.stringify(entry).length+1,0);
  }
  // A turn can yield while its inline question awaits input. Its waiting
  // prose is progress, not a completed answer. Save this distinction so an
  // answered card or a relaunch cannot resurrect a premature final bubble.
  function markQuestionContinuations(rows) {
    const pending=new Map();
    for(const entry of rows){
      const role=displayRole(entry),turn=displayTurnId(entry),row=entry.value||{};
      if(isAuthoredBoundaryEntry(entry))pending.delete(turn);
      if((role==="ask_pending"||role==="ask")&&askIsPending(row)){
        const agents=pending.get(turn)||new Set();agents.add(canonicalAgentId(row.agent||state.item?.id||"phoenix"));pending.set(turn,agents);
      }
      if(role==="answer"&&pending.get(turn)?.has(canonicalAgentId(row.agent||state.item?.id||"phoenix")))row.awaiting_input=true;
    }
    return rows;
  }
  function replaceDisplayRows(rows,dirty=state.displayDirty) {
    const liveProgress=rows.filter(row=>row.source==="story"&&["commentary","narration"].includes(displayRole(row))).map(entry=>({entry,used:false}));
    const repaired=rows.filter(row=>{
      if(row.source!=="history"||!["commentary","narration"].includes(displayRole(row)))return true;
      const match=liveProgress.find(candidate=>!candidate.used&&equivalentDisplayRows(candidate.entry,row));
      if(!match)return true;match.used=true;return false;
    });
    dirty ||= repaired.length!==rows.length;rows=repaired;

    state.displayRows=markQuestionContinuations(rows);
    state.displayBytes=displayRowsBytes(rows);
    state.displayDirty=Boolean(dirty);
    return rows;
  }
  function appendDisplay(source,value,immediate=false,recovered=false) {
    if(state.painting||!value)return false;
    const copy=cloneDisplayValue(value);if(!copy)return false;
    const role=source==="story"?copy.kind:copy.role,lastTurn=state.activeTurnId||state.displayRows.at(-1)?.turn_id||"";
    const boundary=role==="user"||(role==="talk"&&isIncomingAgentTalk(copy)&&!isCompletedAgentReturn(copy));
    const ownedTurn=ownedStoryTurn(copy);
    const entry={source,value:copy,turn_id:ownedTurn||(boundary?(copy.turn_id||newTurnId()):lastTurn)},last=state.displayRows.at(-1);
    // Never put an unanchored old execution under the latest prompt. The
    // authoritative history catch-up owns introducing its missing boundary.
    if(ownedTurn&&!boundary&&!ownedStoryHasBoundary(copy)){deferOwnedStory(copy,recovered);return false;}
    if(source==="story"&&state.item?.kind==="group"){
      const savedIndex=state.displayRows.findIndex(row=>row.value?.historical===true&&equivalentDisplayRows(row,entry));
      if(savedIndex>=0){
        state.displayRows[savedIndex]=entry;replaceDisplayRows(state.displayRows,true);scheduleDisplayPersist(immediate);
        return "before_answer";
      }
    }
    if(["commentary","narration"].includes(role)&&state.displayRows.some(candidate=>candidate.source!==source&&equivalentDisplayRows(candidate,entry)))return false;
    const lastEvent=ownedStoryEventKey(last?.value),entryEvent=ownedStoryEventKey(entry.value);
    if(entryEvent&&state.displayRows.some(candidate=>ownedStoryEventKey(candidate.value)===entryEvent))return false;
    const receiptRole=role==="tool"||role==="tool_start";
    if(last&&(!receiptRole||(lastEvent&&entryEvent&&lastEvent===entryEvent))&&(!lastEvent||!entryEvent||lastEvent===entryEvent)&&displaySemantic(last)===displaySemantic(entry))return false;
    // A terminal answer has exactly one transport owner. Story, Done, and
    // canonical catch-up can arrive with interleaved settlement/receipt rows,
    // so adjacency is not a sufficient duplicate guard.
    if((role==="answer"||role==="group_message")&&state.displayRows.some((candidate)=>candidate.turn_id===entry.turn_id&&equivalentDisplayRows(candidate,entry)))return false;
    // A reconnect or a background journal may deliver a coworker's work after
    // that coworker's terminal contribution. Persist it in authored order so
    // reopening the group cannot put “Work details” below Theo's answer.
    let recoveredIndex=recoveredInsertionIndex(state.displayRows,entry);
    if(recoveredIndex<0&&ownedTurn&&!boundary){
      const ownerIndex=state.displayRows.findIndex((row)=>isAuthoredBoundaryEntry(row)&&displayTurnId(row)===ownedTurn);
      // A message sent mid-task (a steer) shares this turn's id and is not
      // the next turn: later work belongs after it, in the order it happened.
      const nextIndex=state.displayRows.findIndex((row,index)=>index>ownerIndex&&isAuthoredBoundaryEntry(row)&&displayTurnId(row)!==ownedTurn);
      if(nextIndex>=0)recoveredIndex=nextIndex;
    }
    if(recoveredIndex<0)state.displayRows.push(entry);else state.displayRows.splice(recoveredIndex,0,entry);
    if(state.historyHydrating)state.historyLiveRows?.push(cloneDisplayValue(entry));
    markQuestionContinuations(state.displayRows);
    if(boundary&&!copy.queued_id)state.activeTurnId=entry.turn_id;
    state.displayBytes+=JSON.stringify(entry).length+1;
    state.displayDirty=true;
    if(state.displayRows.length>DISPLAY_ROW_CAP||state.displayBytes>DISPLAY_BYTE_CAP)replaceDisplayRows(trimDisplayRows(state.displayRows),true);
    scheduleDisplayPersist(immediate);
    return recoveredIndex<0?"appended":"before_answer";
  }
  function parseDisplayRows(feeds) {
    const raw=feeds?.[DISPLAY_FEED_KEY];if(typeof raw!=="string")return [];
    try {
      const value=JSON.parse(raw);if(value?.version!==2||!Array.isArray(value.rows))return [];
      const candidates=value.rows.filter((row)=>{
        if(!row||!["history","story"].includes(row.source)||!row.value||typeof row.value!=="object")return false;
        if(row.source==="story"&&row.value.kind==="cross_answer")return false;
        if(row.source==="history"&&isInternalHistoryTool(row.value)){state.displayMigrationDirty=true;return false;}
        if(row.source==="story"&&row.value.kind==="handoff"&&isEphemeralVolumeHandoff(row.value)){state.displayMigrationDirty=true;return false;}
        return !(state.item?.kind==="group"&&((row.source==="story"&&row.value.kind==="answer")||(row.source==="history"&&row.value.role==="answer")));
      });
      candidates.forEach((entry)=>{if(ownedStoryTurn(entry.value))entry.turn_id=ownedStoryTurn(entry.value);});
      candidates.forEach((entry)=>{const role=entry.source==="story"?entry.value?.kind:entry.value?.role,answer=role==="user"?askAnswerPrompt(entry.value):null;if(!answer||entry.value.origin?.kind==="ask_answer")return;entry.value.text=answer.detail;entry.value.origin={kind:"ask_answer",ask_id:answer.askId,agent_id:answer.agentId||null,display:answer.detail};state.displayMigrationDirty=true;});
      candidates.forEach((entry)=>{if(entry.source==="history"&&entry.value?.role==="talk"&&!isIncomingAgentTalk(entry.value)){entry.turn_id="";delete entry.value.turn_id;}});
      // Canonical group contributions carry their immutable authored turn on
      // the value. Builds before this repair let the surrounding replay cursor
      // overwrite it, which moved every old Theo answer into the newest turn.
      candidates.forEach((entry)=>{if(state.item?.kind==="group"&&displayRole(entry)==="group_message"&&entry.value?.turn_id)entry.turn_id=entry.value.turn_id;});
      const rows=ensureDisplayTurnIds(candidates);
      // Older builds persisted both the live story receipt and the later
      // reconstructed canonical-history copy. Prefer the story copy one-for-one
      // while retaining legitimate repeated calls from either source. Match
      // inside one turn: the same tool tomorrow is not today's duplicate.
      const storyRows=rows.map((entry,index)=>({entry,index,used:false})).filter(({entry})=>entry.source==="story");
      const restored=[],groupMessageIndexes=new Map(),turnAliases=new Map();
      // Canonical history prompts carry no turn id, so they get an invented
      // legacy id even when the live story copy of the same prompt carries
      // the real one. Both copies then rendered as separate turns and every
      // other row opened a new work block. Map the legacy turn onto the real
      // one when the prompt text matches exactly.
      // The live copy may be truncated with an ellipsis; the history copy is whole.
      const promptMirrors=(left,right)=>{const a=String(left||"").replace(/\s+/g," ").trim(),b=String(right||"").replace(/\s+/g," ").trim();if(!a||!b)return false;if(a===b)return true;const cut=(t)=>t.replace(/(?:…|\.{3})$/u,"").trimEnd();const [shortText,longText]=a.length<=b.length?[cut(a),b]:[cut(b),a];return shortText.length>=40&&longText.startsWith(shortText);};
      const realPrompts=[],turnAnswers=new Map();
      rows.forEach((entry)=>{const id=displayTurnId(entry),role=displayRole(entry);if(role==="answer"||role==="group_message"){const text=canonicalAnswerText(entry.value?.text||entry.value?.markdown||"");if(text){const keys=turnAnswers.get(id)||new Set();keys.add(text);turnAnswers.set(id,keys);}}});
      rows.forEach((entry)=>{const id=displayTurnId(entry);if(displayRole(entry)==="user"&&id&&!/^legacy-/i.test(id))realPrompts.push({id,entry,used:false});});
      rows.forEach((entry)=>{
        const id=displayTurnId(entry);if(displayRole(entry)!=="user"||!/^legacy-/i.test(id)||turnAliases.has(id))return;
        const answers=turnAnswers.get(id),value=entry.value||{};
        const real=realPrompts.find((prompt)=>{
          if(prompt.used||!promptMirrors(prompt.entry.value?.text,value.text)||attachmentIdentity(prompt.entry.value)!==attachmentIdentity(value))return false;
          const other=prompt.entry.value||{},identity=value.message_id||value.client_turn_id||value.request_id;
          if(identity&&identity===(other.message_id||other.client_turn_id||other.request_id))return true;
          if(value.created_at&&value.created_at===other.created_at)return true;
          const known=turnAnswers.get(prompt.id);
          return Boolean(answers?.size&&known?.size&&[...answers].every((text)=>known.has(text)));
        });
        // Consume proven mirror occurrences. An equal prompt with a distinct
        // answer or attachment remains a separate authored legacy turn.
        if(real){real.used=true;turnAliases.set(id,real.id);}
      });
      rows.forEach((entry)=>{
        const alias=turnAliases.get(displayTurnId(entry));
        if(alias){entry.turn_id=alias;if(entry.value?.turn_id)entry.value.turn_id=alias;}
        const role=displayRole(entry);
        if(role==="user"&&alias){const kept=restored.find((candidate)=>displayRole(candidate)==="user"&&displayTurnId(candidate)===alias&&promptMirrors(candidate.value?.text,entry.value?.text));if(kept){if(String(entry.value?.text||"").length>String(kept.value?.text||"").length)kept.value.text=entry.value.text;state.displayMigrationDirty=true;return;}}
        if(role==="user"){
          const existingIndex=restored.findIndex((candidate)=>displayRole(candidate)==="user"&&imageCommentUserMirrors(candidate,entry));
          if(existingIndex>=0){const duplicateTurn=displayTurnId(entry);restored[existingIndex]=preferredImageCommentUser(restored[existingIndex],entry);const stableTurn=displayTurnId(restored[existingIndex]);if(duplicateTurn&&stableTurn&&duplicateTurn!==stableTurn)turnAliases.set(duplicateTurn,stableTurn);state.displayMigrationDirty=true;return;}
        }
        if(role==="group_message"&&entry.value?.message_id){
          const id=String(entry.value.message_id),existingIndex=groupMessageIndexes.get(id);
          if(existingIndex!=null){
            const existing=restored[existingIndex],incomingText=String(entry.value.markdown||entry.value.text||""),existingText=String(existing?.value?.markdown||existing?.value?.text||"");
            // Canonical history retains the authored newlines that old live
            // Story truncation flattened. Prefer it as the one visible copy.
            if(entry.source==="history"&&(existing.source!=="history"||incomingText.includes("\n")&&!existingText.includes("\n")))restored[existingIndex]=entry;
            state.displayMigrationDirty=true;return;
          }
          groupMessageIndexes.set(id,restored.length);
        }
        if(entry.source==="history"){const match=storyRows.find((candidate)=>!candidate.used&&equivalentDisplayRows(entry,candidate.entry));if(match){match.used=true;return;}}
        if((role==="answer"||role==="group_message")&&restored.some((candidate)=>candidate.turn_id===entry.turn_id&&equivalentDisplayRows(candidate,entry)))return;
        restored.push(entry);
      });
      if(restored.length!==rows.length)state.displayMigrationDirty=true;
      const withoutAskGhosts=pruneLateResolvedAskGhosts(restored);if(withoutAskGhosts.length!==restored.length)state.displayMigrationDirty=true;
      const steers=repairSteeredPromptMirrors(withoutAskGhosts);if(steers.length!==withoutAskGhosts.length)state.displayMigrationDirty=true;
      const mirrored=repairLegacyGroupCatchUpTurns(steers);if(mirrored.length!==steers.length)state.displayMigrationDirty=true;
      const repairedReturns=repairHandoffReturnOrder(mirrored);
      if(repairedReturns.changed)state.displayMigrationDirty=true;
      return ensureDisplayTurnIds(trimDisplayRows(repairedReturns.rows));
    } catch { return []; }
  }
  function canonicalGroupPromptText(value){
    let text=String(value||"").replace(/\s+/g," ").trim();
    (ui.state.view?.directory.agents||[]).forEach((agent)=>{
      [agent.agent_id,agent.internal_role,agent.display_name].filter(Boolean).forEach((label)=>{text=text.replace(new RegExp(`@${escapeRegex(String(label))}(?=\\s|$|[.,!?;:])`,"gi"),`@${agent.agent_id}`);});
      if(agent.display_name)text=text.replace(new RegExp(`(^|[^\\p{L}\\p{N}_])${escapeRegex(agent.display_name)}(?=$|[^\\p{L}\\p{N}_])`,`gu`),(_,prefix)=>`${prefix}@${agent.agent_id}`);
    });
    return text.toLowerCase();
  }
  function repairSteeredPromptMirrors(rows){
    const steers=new Map(rows.filter(row=>displayRole(row)==="user"&&row.value?.steer_id).map(row=>[row.value.steer_id,row]));
    return rows.filter(row=>{
      if(displayRole(row)!=="user"||row.value?.steer_id)return true;
      const original=steers.get(row.value?.turn_id||row.turn_id);
      return !original||attachmentIdentity(original.value)!==attachmentIdentity(row.value);
    });
  }
  function repairLegacyGroupCatchUpTurns(rows){
    if(state.item?.kind!=="group")return rows;
    const canonicalPrompts=new Set(rows.filter((entry)=>displayRole(entry)==="user"&&!/^legacy-/i.test(displayTurnId(entry))).map((entry)=>canonicalGroupPromptText(entry.value?.text)));
    if(!canonicalPrompts.size)return rows;
    const mirroredTurns=new Set(rows.filter((entry)=>displayRole(entry)==="user"&&/^legacy-/i.test(displayTurnId(entry))&&canonicalPrompts.has(canonicalGroupPromptText(entry.value?.text))).map(displayTurnId));
    return mirroredTurns.size?rows.filter((entry)=>!mirroredTurns.has(displayTurnId(entry))):rows;
  }
  function repairRepeatedCatchUpTurns(rows,canonicalRows) {
    // Builds before the user-text identity fix persisted each canonical
    // catch-up as a fresh set of `legacy-*` prompt turns. Tool/progress/answer
    // rows did not multiply because their semantic keys already matched. Only
    // migrate when at least two canonical prompts share the same 3x+ exact
    // inflation factor; this fingerprint avoids collapsing a user who really
    // did send identical text more than once.
    const canonicalUsers=new Map();
    (canonicalRows||[]).forEach((row)=>{if(row?.role!=="user")return;const key=`user:${row.text||""}`,list=canonicalUsers.get(key)||[];list.push(row);canonicalUsers.set(key,list);});
    const canonicalCounts=new Map([...canonicalUsers].map(([key,items])=>[key,items.length]));
    if(canonicalCounts.size<2)return{rows,changed:false};
    const turns=[];let turn=[];
    rows.forEach((entry)=>{if(displayRole(entry)==="user"&&turn.length){turns.push(turn);turn=[];}turn.push(entry);});
    if(turn.length)turns.push(turn);
    const occurrences=new Map();
    turns.forEach((group,index)=>{const prompt=group.find((entry)=>displayRole(entry)==="user");if(!prompt||prompt.source!=="history")return;const key=displaySemantic(prompt);if(!canonicalCounts.has(key))return;const list=occurrences.get(key)||[];list.push({group,index,prompt});occurrences.set(key,list);});
    const candidates=[];
    occurrences.forEach((items,key)=>{
      const expected=canonicalCounts.get(key)||0;
      if(!expected||items.length<expected*3||items.length%expected)return;
      if(!items.every(({prompt})=>/^legacy-\d+-answer:[a-z0-9]+$/i.test(String(prompt.turn_id||""))))return;
      candidates.push({key,expected,factor:items.length/expected,items});
    });
    const factorCounts=new Map();
    candidates.forEach(({factor})=>factorCounts.set(factor,(factorCounts.get(factor)||0)+1));
    const repeatedFactor=[...factorCounts.entries()].filter(([,count])=>count>=2).sort((a,b)=>b[1]-a[1]||b[0]-a[0])[0]?.[0];
    if(!repeatedFactor)return{rows,changed:false};
    const dropped=new Set();
    candidates.filter(({factor})=>factor===repeatedFactor).forEach(({key,expected,items})=>{
      const keepers=items.slice(0,expected);
      keepers.forEach((keeper,index)=>{
        const canonical=canonicalUsers.get(key)?.[index],stableId=canonical?.turn_id;
        if(!stableId)return;
        keeper.group.forEach((entry)=>{entry.turn_id=stableId;});
        keeper.prompt.value.turn_id=stableId;
        if(canonical.origin)keeper.prompt.value.origin=cloneDisplayValue(canonical.origin);
      });
      items.slice(expected).forEach((extra,index)=>{
        const keeper=keepers[index%keepers.length];
        extra.group.slice(1).forEach((entry)=>{entry.turn_id=keeper.prompt.turn_id;keeper.group.push(entry);});
        dropped.add(extra.index);
      });
    });
    const repaired=turns.filter((_,index)=>!dropped.has(index)).flat();
    return{rows:ensureDisplayTurnIds(trimDisplayRows(repaired)),changed:Boolean(dropped.size)};
  }
  function scheduleDisplayPersist(immediate=false) {
    if(preview||!state.sessionId||!state.displayDirty||state.displayJournalUnsafe)return;
    if(immediate){clearTimeout(state.displayPersistTimer);state.displayPersistTimer=window.setTimeout(()=>flushDisplayJournal(),0);return;}
    // Throttle instead of resetting a debounce on every streaming event. A
    // long tool run now checkpoints twice a second rather than rewriting a
    // multi-megabyte journal for every receipt—or postponing all durability
    // until the stream finally goes quiet.
    if(!state.displayPersistTimer)state.displayPersistTimer=window.setTimeout(()=>flushDisplayJournal(),500);
  }
  function flushDisplayJournal() {
    clearTimeout(state.displayPersistTimer);state.displayPersistTimer=0;
    if(preview||!state.sessionId||!state.displayDirty||state.displayJournalUnsafe)return state.displayPersistChain;
    const sessionId=state.sessionId,owner=canvasConversationOwner(),identity=conversationIdentity(),payload=JSON.stringify({version:2,rows:trimDisplayRows(state.displayRows)});
    state.displayDirty=false;
    state.displayPersistChain=state.displayPersistChain.catch(()=>{}).then(()=>ui.invoke("feeds_patch",{sessionId,owner,updates:{[DISPLAY_FEED_KEY]:payload}})).catch(()=>{if(conversationIdentity()===identity)state.displayDirty=true;else{const cached=state.conversationViews.get(identity);if(cached)cached.displayDirty=true;}});
    return state.displayPersistChain;
  }
  async function persistDisplayJournalStrict(sessionId=state.sessionId,owner=canvasConversationOwner()) {
    clearTimeout(state.displayPersistTimer);state.displayPersistTimer=0;
    if(preview||!sessionId||state.displayJournalUnsafe)return;
    const payload=JSON.stringify({version:2,rows:trimDisplayRows(state.displayRows)});
    await state.displayPersistChain.catch(()=>{});
    await ui.invoke("feeds_patch",{sessionId,owner,updates:{[DISPLAY_FEED_KEY]:payload}});
    if(state.sessionId===sessionId)state.displayDirty=false;
    state.displayPersistChain=Promise.resolve();
  }
  function reconcileHistory(rows,{appendOnly=false}={}) {
    const available=state.displayRows.map((entry)=>({entry,used:false}));
    const added=[];let reordered=false,canonicalTurn="";
    const canonicalRows=(rows||[]).filter((row)=>!(state.item?.kind==="group"&&row.role==="answer")&&historyVisibleInConversation(row));
    const answersByTurn=new Map();
    available.forEach(({entry})=>{if(displayRole(entry)!=="answer")return;const turn=displayTurnId(entry),keys=answersByTurn.get(turn)||new Set();keys.add(canonicalAnswerText(entry.value?.text||entry.value?.markdown||""));answersByTurn.set(turn,keys);});
    const recovered=canonicalRows.map((row,position)=>{
      const incoming=row.role==="talk"&&isIncomingAgentTalk(row),agentThread=state.item?.kind!=="group";
      const stableTurn=row.turn_id||(agentThread&&row.role!=="user"&&!incoming?canonicalTurn:"");
      const entry={source:"history",value:row,turn_id:stableTurn};
      const candidates=available.filter((candidate)=>!candidate.used&&equivalentDisplayRows(candidate.entry,entry)&&(row.role!=="user"||attachmentIdentity(candidate.entry.value)===attachmentIdentity(row)));
      let match;
      if(agentThread&&row.role==="user"&&!row.turn_id){
        const tail=canonicalRows.slice(position+1),boundary=tail.findIndex((value)=>value.role==="user"),block=boundary<0?tail:tail.slice(0,boundary);
        const answer=block.find((value)=>value.role==="answer"),text=answer&&canonicalAnswerText(answer.text||answer.markdown||"");
        if(text)match=candidates.find(({entry})=>answersByTurn.get(displayTurnId(entry))?.has(text));
      }
      match ||= candidates[0];
      if(match)match.used=true;
      if(agentThread&&row.role==="user")canonicalTurn=displayTurnId(match?.entry||entry);
      return{entry,match};
    });
    if(appendOnly){
      // Quiet polling never rearranges live messages or imports unanchored
      // archive receipts into the newest turn.
      const lastBoundary=recovered.findLastIndex(({entry,match})=>match&&displayRole(entry)==="user");
      if(lastBoundary<0)return{added:[],reordered:false};
      recovered.slice(lastBoundary+1).forEach(({entry,match})=>{
        if(match||isCompactionEvent(entry.value))return;
        state.displayRows.push(entry);added.push(entry);
      });
      if(added.length)replaceDisplayRows(ensureDisplayTurnIds(trimDisplayRows(state.displayRows)),true);
      return{added,reordered:false};
    }
    recovered.forEach(({entry,match},position)=>{
      if(match){
        if(match.entry.source==="history"&&match.entry.value?.historical===true&&entry.value?.historical===true
          &&JSON.stringify(match.entry.value)!==JSON.stringify(entry.value)){
          match.entry.value=entry.value;match.entry.turn_id=entry.turn_id;reordered=true;
        }
        return;
      }
      let index=recoveredInsertionIndex(state.displayRows,entry);
      // A missing old prompt belongs before the next recovered row already
      // on screen, not after the newest exchange. Resolve matches first so
      // repeated text is consumed one-for-one in canonical history order.
      if(index<0){
        const next=recovered.slice(position+1).find((row)=>row.match);
        if(next)index=state.displayRows.indexOf(next.match.entry);
      }
      if(index<0)state.displayRows.push(entry);
      else{state.displayRows.splice(index,0,entry);reordered=true;}
      added.push(entry);
    });
    // A prompt with a known turn (routines, wakes) belongs right before its
    // own turn's work. Matching by the next recognised row sent a routine
    // prompt far up the chat, because repeated tool rows (browser_status)
    // matched an older run.
    recovered.forEach(({entry,match})=>{
      const own=match?.entry||entry,turn=String(entry.value?.turn_id||"");
      if(displayRole(own)!=="user"||!turn||own.value?.steered||entry.value?.steered)return;
      const first=state.displayRows.findIndex((row)=>row!==own&&displayTurnId(row)===turn);
      const at=state.displayRows.indexOf(own);
      if(first<0||at<0||at===first-1)return;
      state.displayRows.splice(at,1);
      state.displayRows.splice(state.displayRows.indexOf(state.displayRows.find((row)=>row!==own&&displayTurnId(row)===turn)),0,own);
      reordered=true;
    });
    // Repair: a message sent mid-task used to be saved after that task's
    // later work. History knows the real order, so move it back before the
    // next history row it preceded.
    recovered.forEach(({entry,match},position)=>{
      const own=match?.entry||entry;if(displayRole(own)!=="user"||!(own.value?.steered||entry.value?.steered))return;
      const next=recovered.slice(position+1).find((row)=>row.match&&row.match.entry!==own);if(!next)return;
      const at=state.displayRows.indexOf(own),before=state.displayRows.indexOf(next.match.entry);
      if(at>before&&before>=0){state.displayRows.splice(at,1);state.displayRows.splice(before,0,own);reordered=true;}
    });
    if(state.item?.kind==="group"||canonicalRows.filter((row)=>row.role==="user").length>1){
      // Matched history gives authored order for both group and agent turns.
      // Move complete blocks with their receipts and replies. Blocks absent
      // from this bounded history snapshot retain their existing slots.
      const ranks=new Map(recovered.filter(({entry})=>displayRole(entry)==="user").map(({entry,match},index)=>[match?.entry||entry,index]));
      const blocks=[];let block=[];
      state.displayRows.forEach((entry)=>{
        if(displayRole(entry)==="user"&&block.length){blocks.push(block);block=[];}
        block.push(entry);
      });
      if(block.length)blocks.push(block);
      const ordered=blocks.filter((rows)=>ranks.has(rows[0])).sort((a,b)=>ranks.get(a[0])-ranks.get(b[0]));
      let cursor=0;
      const repaired=blocks.flatMap((rows)=>ranks.has(rows[0])?ordered[cursor++]:rows);
      if(repaired.some((entry,index)=>entry!==state.displayRows[index])){state.displayRows=repaired;reordered=true;}
    }
    const repairedReturns=repairHandoffReturnOrder(ensureDisplayTurnIds(trimDisplayRows(state.displayRows)));
    replaceDisplayRows(repairedReturns.rows,state.displayDirty||Boolean(added.length)||reordered||repairedReturns.changed);
    return{added,reordered};
  }
  function normalizeAskRecord(record) {
    const questions=Array.isArray(record.questions)&&record.questions.length?record.questions:[{header:"Recovered request",question:"Phoenix was waiting for your input when the app closed. Continue where you left off?",options:["Continue","Not now"],multi_select:false}];
    return {kind:"ask_pending",id:record.ask_id||record.id,agent:record.agent||"phoenix",questions,approval:record.approval||null,status:record.status||"pending",answer:record.answer||null,created_at:record.created_at||null,resolved_at:record.resolved_at||null};
  }
  function reconcileAsks(records) {
    let changed=false;
    (records||[]).forEach((record)=>{
      const ask=normalizeAskRecord(record);if(!ask.id)return;
      const existing=state.displayRows.filter((entry)=>entry.source==="story"&&entry.value?.kind==="ask_pending"&&(entry.value.id||entry.value.ask_id)===ask.id);
      if(existing.length){existing.forEach((entry)=>{
        const current=entry.value,resolved=!askIsPending(current),incomingPending=askIsPending(ask);
        const currentTime=Date.parse(current.resolved_at||""),incomingTime=Date.parse(ask.resolved_at||"");
        // A delayed snapshot cannot reopen an immutable resolved ask. A
        // later terminal decision needs a newer resolution timestamp.
        if(resolved&&(incomingPending||(current.status!==ask.status&&(!Number.isFinite(incomingTime)||(Number.isFinite(currentTime)&&incomingTime<=currentTime)))))return;
        if(resolved&&Number.isFinite(currentTime)&&Number.isFinite(incomingTime)&&incomingTime<currentTime)return;
        const merged={...current,...ask};
        if(resolved){merged.resolved_at=ask.resolved_at||current.resolved_at;merged.answer=ask.answer??current.answer;}
        if(!Array.isArray(record.questions)||!record.questions.length)merged.questions=current.questions||ask.questions;
        if(!record.agent)merged.agent=current.agent||ask.agent;
        if(JSON.stringify(current)!==JSON.stringify(merged)){Object.assign(current,merged);changed=true;}
      });}
      // History contains resolved questions so an existing card can be
      // settled after a restart. It must never manufacture an absent,
      // days-old card at the tail of the current turn.
      else if(askIsPending(ask)){
        // AskRecord does not carry a native turn ID. Keep its saved timestamp
        // and explicit provenance rather than attaching it to today's tail.
        ask.recovered_unplaced=true;state.displayRows.push({source:"story",value:ask,turn_id:"",recovered_ask_unplaced:true});changed=true;
      }
    });
    const pruned=pruneLateResolvedAskGhosts(state.displayRows);if(pruned.length!==state.displayRows.length)changed=true;
    replaceDisplayRows(repairRecoveredRowOrder(ensureDisplayTurnIds(trimDisplayRows(pruned))),state.displayDirty||changed);
    return changed;
  }
  function renderDisplayEntry(entry) {
    if(state.item?.kind==="group"&&entry?.source==="story"&&["steer","steer_delivered","brief"].includes(entry.value?.kind))return;
    const previous=state.renderingTurnId,role=displayRole(entry);state.renderingTurnId=entry.turn_id||"";
    if(isAuthoredBoundaryEntry(entry)&&(!entry.value?.queued_id||entry.value?.queued_canonical))state.activeTurnId=entry.turn_id||"";
    try { if(entry?.source==="story")renderStory(entry.value,true); else if(entry?.source==="history")renderHistory(entry.value); }
    finally { state.renderingTurnId=previous; }
  }
  function formatClock(value) {
    if (value == null || value === "") return "";
    const date = new Date(value); if (!Number.isFinite(date.getTime())) return "";
    return date.toLocaleTimeString([], { hour:"numeric", minute:"2-digit" });
  }
  // Walk back from the feed tail to the previous message and report the work
  // cluster this answer belongs to. The cluster header itself becomes the
  // persistent Thought disclosure; there is no second summary row.
  function precedingWorkCluster(agent) {
    let cursor = lastConversationRow();
    while (cursor && !cursor.classList.contains("user-message") && !cursor.classList.contains("agent-message")) {
      if (cursor.classList.contains("work-cluster") && (!agent||canonicalAgentId(cursor.dataset.agent)===canonicalAgentId(agent)) && cursor.querySelector(".work-tool,.agent-update,.commentary-line,.ask-history-row,.handoff-chain,.context-compaction")) return cursor;
      cursor = cursor.previousElementSibling;
    }
    return null;
  }
  function attachWorkToggle(node,cluster){
    if(!node||!cluster?.isConnected)return;
    const header=node.querySelector(".message-content > header");if(!header||header.querySelector(".answer-work-toggle"))return;
    const steps=[...cluster.querySelectorAll(".work-tool:not(.failed)")].reduce((sum,row)=>sum+(Number(row.dataset.repeatCount)||1),0);
    const button=document.createElement("button");button.type="button";button.className="answer-work-toggle";
    const open=toolActivityView();
    button.setAttribute("aria-expanded",String(open));button.title="Show or hide the work behind this answer";
    button.innerHTML=`<span>${steps?`${steps} step${steps===1?"":"s"}`:"Thinking"}</span><svg viewBox="0 0 20 20" aria-hidden="true"><path d="m6.5 8 3.5 3.5L13.5 8"/></svg>`;
    header.append(button);cluster.classList.toggle("work-folded",!open);setWorkDisclosure(cluster,open);
    button.addEventListener("click",(event)=>{event.preventDefault();event.stopPropagation();const open=cluster.classList.toggle("work-folded")===false;setWorkDisclosure(cluster,open);button.setAttribute("aria-expanded",String(open));});
  }
  function revealUnresolvedWork() {
    const cluster=precedingWorkCluster();
    if(!cluster)return null;
    const tools=cluster.querySelector(".work-tools");if(tools)tools.hidden=false;
    return cluster;
  }
  function foldRestoredWork() {
    document.querySelectorAll(".work-cluster").forEach((cluster)=>{
      settleWorkCluster(cluster,false);
    });
  }
  function syncRestoredWorkVisibility() {
    if(latestTurnIsTerminal())foldRestoredWork();
    else revealUnresolvedWork();
  }
  function copyIcon() { return '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="6" y="3" width="10" height="11" rx="2"/><rect x="3" y="6" width="10" height="11" rx="2"/></svg>'; }
  function deleteIcon() { return '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M4 6h12M8 3h4l1 3H7l1-3ZM6 6l.7 11h6.6L14 6M8.5 9.2v4.7M11.5 9.2v4.7"/></svg>'; }
  function formatElapsed(value){const total=Math.max(0,Math.round((Number(value)||0)/1000)),minutes=Math.floor(total/60),seconds=total%60;return minutes?`${minutes}m ${seconds}s`:`${seconds}s`;}
  // "replace in ../../x/src/app/page.tsx" and "../../x/src/app/page.tsx" are
  // the same file: drop the tool verb and any leading "./" or "../" hops.
  function editedPath(value){return String(value||"").trim().replace(/^(?:replace(?:d)? in|edit(?:ed)?|wrote|write|creat(?:e|ed)|patch(?:ed)?|updat(?:e|ed))\s+/i,"").replace(/^(?:\.\.?\/)+/,"")||"Edited file";}
  function clusterEditRows(cluster){
    if(!cluster)return[];const changes=new Map();
    cluster.querySelectorAll(".work-tool:not(.running)").forEach((row)=>{if(!EDIT_TOOLS.has(String(row.dataset.tool||"").toLowerCase()))return;const path=editedPath(row.dataset.toolTarget||row.querySelector(".work-tool-copy small")?.textContent||row.dataset.toolLabel||"Edited file"),diff=row.dataset.toolDiff||"";let additions=0,deletions=0;String(diff).split("\n").forEach((line)=>{if(line.startsWith("+")&&!line.startsWith("+++"))additions+=1;else if(line.startsWith("-")&&!line.startsWith("---"))deletions+=1;});const prior=changes.get(path)||{path,additions:0,deletions:0,known:false};prior.additions+=additions;prior.deletions+=deletions;prior.known||=Boolean(diff);changes.set(path,prior);});
    return[...changes.values()];
  }
  function turnEditSummaryMarkup(cluster){const rows=clusterEditRows(cluster);if(!rows.length)return"";const additions=rows.reduce((sum,row)=>sum+row.additions,0),deletions=rows.reduce((sum,row)=>sum+row.deletions,0),anyKnown=rows.some((row)=>row.known),counts=(row)=>row.known?`<b>+${row.additions}</b><i>−${row.deletions}</i>`:"";return`<section class="turn-edit-summary"><header><span class="turn-edit-copy"><strong>Edited ${rows.length} file${rows.length===1?"":"s"}</strong>${anyKnown?`<small><b>+${additions}</b><i>−${deletions}</i></small>`:""}</span></header><div class="turn-edit-files">${rows.map((row,index)=>`<div class="turn-edit-file${index>2?" extra":""}" ${index>2?"hidden":""}><span>${escape(row.path)}</span><code>${counts(row)}</code></div>`).join("")}</div>${rows.length>3?`<button type="button" class="turn-edit-more" data-show-more-edits>Show ${rows.length-3} more file${rows.length-3===1?"":"s"}⌄</button>`:""}</section>`;}
  // An answer receipt proves a response was delivered, not that its goal was
  // achieved. Keep this footer neutral, including on restored partial answers.
  function completionMetaMarkup(meta){const clock=formatClock(meta?.created_at),elapsed=meta?.elapsed_ms!=null?formatElapsed(meta.elapsed_ms):"";if(!clock&&!elapsed)return"";return`<span class="completion-meta"><span>${elapsed?`Responded in ${escape(elapsed)}`:"Responded"}${clock?` · ${escape(clock)}`:""}</span></span>`;}
  function renderAnswer(text, agent = null, suppliedMeta = null) {
    if (isCompactionText(text)) return null;
    if(runtimeFailureSummary(text))return renderRuntimeFailure(text,agent,suppliedMeta);
    // Final prose is user-owned output, not runtime metadata. Keep it byte-for-
    // byte faithful except for one exact leading speaker tag that matches this
    // coworker's private runtime role (for example `[school_coach]`).
    const visibleText=visibleAnswerText(text,agent);
    if(!visibleText)return null;
    const profile = agentProfile(agent || state.item?.id || "phoenix");
    const key=answerKey(visibleText),meta=suppliedMeta||state.answerMeta.get(key)||(state.painting?{created_at:null,elapsed_ms:null}:{created_at:new Date().toISOString(),elapsed_ms:state.turnStartedAt?Date.now()-state.turnStartedAt:null});
    const turn=state.renderingTurnId||state.activeTurnId||"";
    const waiting=suppliedMeta?.awaiting_input||state.displayRows.some((entry)=>displayRole(entry)==="answer"&&displayTurnId(entry)===turn&&entry.value?.awaiting_input&&answerKey(entry.value.markdown||entry.value.text||"")===key);
    // A final reply remains public even when the agent also asks a question.
    if(waiting){const update=renderAgentUpdate(agent||state.item?.id||"phoenix",visibleText,false,true,true);if(update){update.classList.add("awaiting-input-update");PhoenixConversationUpdates.finalize(update);if(!state.painting)syncMessageGroups();}return update;}
    // Recover histories written by the old late-helper integration pass. A
    // status explicitly reporting no change cannot replace this turn's answer.
    if(/^Nothing new(?:[.:]|$)/i.test(visibleText)){
      const prior=[...$("conversationFeed").querySelectorAll(".agent-message")].reverse().find((node)=>
        node.dataset.turnId===turn&&!node.classList.contains("superseded-answer")&&
        node.querySelector(".answer-footer")&&node.querySelector("header strong")?.textContent===(profile?.display_name||currentName()));
      if(prior)return prior;
    }
    const workCluster=precedingWorkCluster(agent||state.item?.id||"phoenix");
    if(workCluster){settleWorkCluster(workCluster,false,meta);}
    const node=feedNode("message-row agent-message", `${avatar(profile)}<div class="message-content" data-slot="message-content"><header><strong>${escape(profile?.display_name || currentName())}</strong></header><div class="markdown">${relayedImagesMarkup(visibleText)}${markdown(visibleText)}</div>${turnEditSummaryMarkup(workCluster)}<footer class="answer-footer"><button type="button" data-copy-answer aria-label="Copy answer" title="Copy answer">${copyIcon()}</button><button type="button" class="turn-delete-button" data-delete-agent-turn aria-label="Permanently delete this complete agent turn" title="Delete agent turn">${deleteIcon()}</button>${completionMetaMarkup(meta)}</footer></div>`,{slot:"message",from:"assistant",speaker:canonicalAgentId(agent||state.item?.id||"phoenix")});
    node.dataset.answerKey=key;
    PhoenixConversationUpdates.finalize(node);
    attachWorkToggle(node,workCluster);
    // One task, one answer: when a coworker's later pass (after a coworker
    // reported back) answers again in the same task, the newest answer stands
    // and the earlier one is hidden.
    if(state.item?.kind!=="group"&&node.dataset.turnId){
      // Walk back only to the nearest message addressed to this agent (the
      // user's, a coworker's request, an answer): an answer before that
      // replied to something else and must stay visible.
      const name=profile?.display_name||currentName(),earlier=[];
      for(let other=node.previousElementSibling;other;other=other.previousElementSibling){
        if(other.classList.contains("user-message"))break;
        if(other.classList.contains("agent-message")&&!other.classList.contains("commentary-message")&&!other.classList.contains("decision-request")&&!other.classList.contains("outgoing-talk")&&!other.classList.contains("superseded-answer")
          &&other.dataset.turnId===node.dataset.turnId&&other.querySelector("header strong")?.textContent===name)earlier.push(other);
      }
      earlier.forEach((other)=>other.classList.add("superseded-answer"));
    }
    if(!state.painting)syncMessageGroups();
    hydrateRelayedImages(node);
    if(!state.painting){state.pendingAnswer={node,cluster:workCluster,text:visibleText,key,meta};resolveWords(node.querySelector(".markdown"));}
    return node;
  }
  // Words resolve out of the dark instead of popping in. This is pure CSS —
  // one staggered animation per word, no timers and no rAF — and it deliberately
  // avoids `filter:blur`, which is what dropped this webview to single-digit
  // frames before. A soft text-shadow reads as the same smear for a fraction of
  // the cost, and the wrappers are unwrapped once the animation ends so long
  // threads never carry hundreds of animated spans.
  const RESOLVE_WORD_CAP = 420;
  function resolveWords(root) {
    if(!root||document.documentElement.dataset.motion==="minimal")return;
    if(window.matchMedia("(prefers-reduced-motion: reduce)").matches)return;
    const walker=document.createTreeWalker(root,NodeFilter.SHOW_TEXT,{
      acceptNode:(node)=>node.nodeValue.trim()&&!node.parentElement.closest("pre,code")
        ?NodeFilter.FILTER_ACCEPT:NodeFilter.FILTER_REJECT,
    });
    const texts=[];for(let node=walker.nextNode();node;node=walker.nextNode())texts.push(node);
    let index=0;
    texts.forEach((node)=>{
      if(index>=RESOLVE_WORD_CAP)return;
      const fragment=document.createDocumentFragment();
      node.nodeValue.split(/(\s+)/).forEach((piece)=>{
        if(!piece)return;
        if(!piece.trim()||index>=RESOLVE_WORD_CAP){fragment.append(piece);return;}
        const span=document.createElement("span");
        span.className="resolve-word";
        span.style.animationDelay=`${Math.min(index*16,2600)}ms`;
        span.textContent=piece;
        fragment.append(span);
        index+=1;
      });
      node.replaceWith(fragment);
    });
    root.addEventListener("animationend",(event)=>{
      const span=event.target.closest?.(".resolve-word");
      if(span)span.replaceWith(span.textContent);
    });
  }
  async function settleAnswerMeta() {
    const answer=state.pendingAnswer;if(!answer)return;
    const elapsed=Math.max(0,Date.now()-(state.turnStartedAt||Date.now()));
    const meta={created_at:answer.meta?.created_at||new Date().toISOString(),elapsed_ms:elapsed};
    state.answerMeta.set(answer.key,meta);
    if(answer.cluster?.isConnected)settleWorkCluster(answer.cluster,false,meta);
    const completion=answer.node?.querySelector(".completion-meta"),footer=answer.node?.querySelector(".answer-footer");if(footer){if(completion)completion.outerHTML=completionMetaMarkup(meta);else footer.insertAdjacentHTML("beforeend",completionMetaMarkup(meta));}
    // The hash-keyed feed entry is fragile (identical answers collide, and it is
    // never written when a turn settles in a thread you are not looking at), so
    // the duration also rides along on the journal row that repaints the answer.
    const journaled=[...state.displayRows].reverse().find((entry)=>(entry.source==="history"?entry.value?.role:entry.value?.kind)==="answer");
    if(journaled){journaled.value.meta=meta;state.displayDirty=true;scheduleDisplayPersist(true);}
    const updates={[answer.key]:JSON.stringify(meta)};
    if(!preview&&state.sessionId)ui.invoke("feeds_patch",{sessionId:state.sessionId,owner:canvasConversationOwner(),updates}).catch(()=>{});
  }
  async function copyCodeBlock(button) {
    const figure=button.closest(".code-block"),lines=[...(figure?.querySelectorAll(".cb-line")||[])];
    const text=lines.length?lines.map((line)=>line.textContent.replace(/^ $/,"")).join("\n"):(figure?.querySelector(".code-source code")?.innerText||"");if(!text)return;
    try { await navigator.clipboard.writeText(text); }
    catch { const area=document.createElement("textarea");area.value=text;area.style.position="fixed";area.style.opacity="0";document.body.append(area);area.select();document.execCommand("copy");area.remove(); }
    button.innerHTML=window.PhoenixAgentKit.icon("check");button.classList.add("copied");button.setAttribute("aria-label","Copied");
    setTimeout(()=>{button.innerHTML=window.PhoenixAgentKit.icon("copy");button.classList.remove("copied");button.setAttribute("aria-label","Copy code");},1600);
  }
  async function copyAnswer(button) {
    const node=button.closest(".agent-message"),text=node?.querySelector(".markdown")?.innerText||"";if(!text)return;
    try { await navigator.clipboard.writeText(text); }
    catch { const area=document.createElement("textarea");area.value=text;area.style.position="fixed";area.style.opacity="0";document.body.append(area);area.select();document.execCommand("copy");area.remove(); }
    button.innerHTML=icons.check;button.classList.add("copied");setTimeout(()=>{button.innerHTML=copyIcon();button.classList.remove("copied");},1200);
  }
  // A provider ships its whole reasoning summary as ONE event, and it usually
  // interleaves tiny planning headings with the useful prose ("**Exploring the
  // codebase**\n\nI need to…"). Phoenix shows the prose, so every standalone
  // heading line is dropped — not only the leading one.
  const PROGRESS_HEADING = /^\s*(?:\*\*([^*\n]{2,100})\*\*|#{1,4}\s+([^\n]{2,100}))\s*$/u;
  function normalizedProgressText(text) {
    const headings = [];
    const body = String(text || "")
      .replace(/\r\n/g, "\n")
      .split("\n")
      .filter((line) => {
        const heading = line.match(PROGRESS_HEADING);
        if (!heading) return true;
        headings.push((heading[1] || heading[2] || "").trim());
        return false;
      })
      .join("\n")
      .replace(/\n{3,}/g, "\n\n")
      .trim();
    // A round whose reasoning was ONLY a heading still deserves a line, so the
    // heading comes back as ordinary prose rather than leaving the round silent.
    return body || headings.filter(Boolean).join(" · ");
  }
  function progressFingerprint(text) {
    return normalizedProgressText(text).toLocaleLowerCase().replace(/[^\p{L}\p{N}]+/gu," ").trim();
  }
  function tooSimilarProgress(previous,next) {
    if(!previous||!next)return false;
    if(previous===next||previous.includes(next)||next.includes(previous))return true;
    const a=new Set(previous.split(" ").filter((word)=>word.length>2)),b=new Set(next.split(" ").filter((word)=>word.length>2));
    if(!a.size||!b.size)return false;
    let shared=0;for(const word of a)if(b.has(word))shared+=1;
    return shared/Math.min(a.size,b.size)>=.78;
  }
  function thoughtBubbleMarkup() {
    return '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M4.2 4.7h11.6a2 2 0 0 1 2 2v5.8a2 2 0 0 1-2 2H9l-3.8 2.7.9-2.7H4.2a2 2 0 0 1-2-2V6.7a2 2 0 0 1 2-2Z"/><circle cx="6.5" cy="9.6" r=".7"/><circle cx="10" cy="9.6" r=".7"/><circle cx="13.5" cy="9.6" r=".7"/></svg>';
  }
  function reasoningParts(text) {
    const raw=String(text||"").replace(/\r\n/g,"\n"),headings=[];
    const detail=raw.split("\n").filter((line)=>{const heading=line.match(PROGRESS_HEADING);if(!heading)return true;headings.push((heading[1]||heading[2]||"").trim());return false;}).join("\n").replace(/\n{3,}/g,"\n\n").trim();
    if(headings.length)return{summary:safeRuntimeCopy(headings[0]),detail:safeRuntimeCopy(detail)};
    const cleaned=safeRuntimeCopy(normalizedProgressText(raw));
    const summaryLead=/^(?:Planning|Checking|Reading|Reviewing|Inspecting|Searching|Browsing|Preparing|Drafting|Writing|Editing|Running|Executing|Calling|Requesting|Navigating|Verifying|Comparing|Analyzing|Assessing|Evaluating|Confirming|Testing|Rebuilding|Launching|Opening|Loading|Fetching|Recalling|Prioritizing|Investigating|Mapping|Tracing|Validating|Waiting|Continuing|Finishing)\b/i;
    const isSummary=!cleaned.includes("\n")&&cleaned.length<=110&&summaryLead.test(cleaned)&&!/[.!?][\])}'\"]?$/.test(cleaned);
    return isSummary?{summary:cleaned,detail:""}:{summary:"",detail:cleaned};
  }
  function expandedReasoningSummary(summary, detail) {
    // The disclosure title is a short heading. The full commentary belongs
    // in its body; repeating it in both places doubles the reading burden.
    const heading = safeRuntimeCopy(summary).trim();
    if (heading && heading !== "Thinking") return heading;
    const plain = safeRuntimeCopy(detail).replace(/[*_`#>]+/g, " ").replace(/\s+/g, " ").trim();
    if (!plain) return heading;
    if (plain.length <= 90) return plain;
    const lead = plain.slice(0, 87).replace(/\s+\S*$/, "");
    return `${lead}…`;
  }
  function setReasoningGroupRunning(group,running) {
    if(!group)return;
    group.classList.toggle("running",Boolean(running));
    const copy=group.querySelector(".reasoning-subgroup-label");
    if(copy)copy.textContent=group.dataset.summary||(running?"Thinking":"Thought");
    syncActivityCursor(group.closest(".work-cluster"),running?group:null);
  }
  function setReasoningClusterRunning(group,running) {
    if(!group)return;
    group.classList.toggle("running",Boolean(running));
    const copy=group.querySelector(":scope > summary .reasoning-cluster-label");
    if(copy)copy.textContent=running?"Thinking":"Thought";
    syncActivityCursor(group.closest(".work-cluster"));
  }
  // A live turn has one activity cursor. It is attached to the newest current
  // activity and moves between Thinking/Browsing/tool groups; old rows never
  // retain a second animated cube.
  // Group live pals: the leader in a large animated circle with up to four
  // pals around it. Seats are role-based and stable (sort_order, then
  // agent_id), never reordered by who is speaking. As the free centre of the
  // header narrows, the inner pair docks on the leader's lower rim (stage 2),
  // then the outer pair docks too and the inner pair slides lower (stage 3).
  // Thresholds carry hysteresis so a width hovering at a boundary does not
  // flicker between layouts.
  const PAL_SEATS=["inner-left","inner-right","outer-left","outer-right"],PAL_HYSTERESIS=16;
  const palState={stage:1,signature:"",observer:null,needs:{1:180,2:120}};
  function groupPalRoster(groupId){
    const directory=ui.state.view?.directory;if(!directory)return null;
    const rows=(directory.members||[]).filter((member)=>member.group_id===groupId&&member.present!==false)
      .sort((a,b)=>(Number(a.sort_order)||0)-(Number(b.sort_order)||0)||String(a.agent_id).localeCompare(String(b.agent_id)));
    const ids=[...new Set(rows.map((member)=>member.agent_id))];if(!ids.length)return null;
    const group=(directory.groups||[]).find((row)=>row.group_id===groupId),wanted=group?.leader_agent_id;
    const leader=wanted&&ids.includes(wanted)?wanted:ids.includes("phoenix")?"phoenix":ids[0];
    const pals=ids.filter((id)=>id!==leader);
    return{leader,seated:pals.slice(0,PAL_SEATS.length),extra:Math.max(0,pals.length-PAL_SEATS.length)};
  }
  function palFreeWidth(header){
    const box=header.getBoundingClientRect(),centre=box.left+box.width/2;let left=box.left,right=box.right;
    for(const node of header.children){
      if(node.id==="groupPals"||node.classList.contains("header-spacer")||node.hidden||!node.getClientRects().length)continue;
      const rect=node.getBoundingClientRect();if(rect.bottom<=box.top+2||rect.top>=box.top+box.height*.6)continue;
      if(rect.right<=centre)left=Math.max(left,rect.right);else if(rect.left>=centre)right=Math.min(right,rect.left);else{left=Math.max(left,rect.right);}
    }
    return Math.max(0,2*Math.min(centre-left,right-centre));
  }
  function palStageFor(width,current){
    const needs=palState.needs;let stage=current;
    while(stage<3&&width<needs[stage])stage+=1;
    while(stage>1&&width>=needs[stage-1]+PAL_HYSTERESIS)stage-=1;
    return stage;
  }
  function layoutGroupPals(){
    const host=$("groupPals"),header=$("conversationHeader");if(!host||!header)return;
    const stage=palStageFor(palFreeWidth(header),palState.stage);
    if(stage!==palState.stage||host.dataset.stage!==String(stage)){palState.stage=stage;host.dataset.stage=String(stage);}
    const leader=host.querySelector('[data-seat="leader"]'),body=$("conversationBody");
    if(leader&&body&&leader.getBoundingClientRect().height){
      const rect=leader.getBoundingClientRect(),margin=parseFloat(getComputedStyle(body).marginTop)||0;
      const offset=Math.round(rect.top+rect.height/2-body.getBoundingClientRect().top+margin);
      $("conversationStage").style.setProperty('--room-transcript-offset',`${offset}px`);
      $("conversationStage").style.setProperty('--room-transcript-fade',`${Math.round(rect.height)}px`);
    }
  }
  function syncGroupPals(activity=ui.activityFor?.(state.item)){
    const header=$("conversationHeader");if(!header)return;
    let host=$("groupPals");
    const roster=state.item?.kind==="group"?groupPalRoster(state.item.id):null;
    if(!roster){if(host){host.remove();header.classList.remove("has-group-pals");palState.signature="";}return;}
    if(!host){
      host=document.createElement("div");host.id="groupPals";host.className="group-pals";host.setAttribute("role","group");host.dataset.stage=String(palState.stage);
      header.append(host);header.classList.add("has-group-pals");
      if(!palState.observer&&typeof ResizeObserver==="function"){palState.observer=new ResizeObserver(()=>layoutGroupPals());palState.observer.observe(header);}
    }
    const signature=JSON.stringify([state.item.id,roster]);
    if(signature!==palState.signature){
      palState.signature=signature;
      const seat=(id,name)=>{const profile=agentProfile(id);return`<span class="pal-seat" data-seat="${name}" data-agent="${escape(id)}" title="${escape(agentLabel(id))}${name==="leader"?" · leader":""}" style="--agent:${escape(profile?.color||"#77736d")}"><span class="pal-face">${ui.avatarSvg(profile)}</span></span>`;};
      host.innerHTML=seat(roster.leader,"leader")+roster.seated.map((id,index)=>seat(id,PAL_SEATS[index])).join("")+(roster.extra?`<span class="pal-more" title="${roster.extra} more">+${roster.extra}</span>`:"");
      host.setAttribute("aria-label",`Group lead ${agentLabel(roster.leader)} with ${roster.seated.length+roster.extra} coworker${roster.seated.length+roster.extra===1?"":"s"}`);
      host.dataset.count=String(roster.seated.length);host.dataset.more=roster.extra?"true":"false";
      const style=getComputedStyle(host),wide=parseFloat(style.getPropertyValue("--pal-wide-space"))||180,docked=parseFloat(style.getPropertyValue("--pal-docked-space"))||120;
      const more=parseFloat(style.getPropertyValue("--pal-more-space"));
      palState.needs={1:wide+(roster.extra?(more||64):0),2:docked+(roster.extra?(more||30):0)};
    }
    const live=activity&&LIVE_CONVERSATION_STATUSES.has(activity.status),speaking=new Set((live?activity.active_agent_ids||[]:[]).map(canonicalAgentId));
    host.dataset.live=String(Boolean(live));
    for(const node of host.querySelectorAll(".pal-seat"))node.classList.toggle("speaking",speaking.has(canonicalAgentId(node.dataset.agent)));
    layoutGroupPals();requestAnimationFrame(layoutGroupPals);
  }
  function syncTeamPresence(activity=ui.activityFor?.(state.item)){
    syncGroupPals(activity);
    if(state.item?.kind==="agent"){for(const row of handoffRows())syncHandoffLiveLabel(row,activity);return;}
    if(state.item?.kind!=="group"||!activity)return;
    const feed=$("conversationFeed"),active=new Set((LIVE_CONVERSATION_STATUSES.has(activity.status)?activity.active_agent_ids||[]:[]).map(canonicalAgentId));
    const turn=state.activeTurnId||[...feed.querySelectorAll(".user-message")].at(-1)?.dataset.turnId;
    for(const row of feed.querySelectorAll(".team-work-row")){
      const current=row.dataset.turnId===turn,worker=isEphemeralVolumeAgent(row.dataset.agent);
      const workers=[...state.ephemeralWorkers.values()].filter(w=>w.conversationKey===conversationKey()&&(!w.turnId||w.turnId===turn));
      const running=!!row.querySelector(".work-tool.running,.context-compaction.running");
      const live=current&&row.closest('.team-work-block')?.dataset.interrupted!=='true'&&LIVE_CONVERSATION_STATUSES.has(activity.status)&&(worker?(running||workers.length>0):active.has(canonicalAgentId(row.dataset.agent)));
      const terminal=["done","failed","cancelled","blocked","waiting_user","continued"].includes(row.dataset.teamState);
      row.dataset.runtimeKnown="true";row.dataset.runtimeActive=String(live&&!terminal);
      row.classList.toggle("live",live&&!terminal);
      if(worker)row.dataset.activeWorkers=String(current?workers.length:0);
      syncTeamWorkSummary(row);
    }
    for(const row of handoffRows())syncHandoffLiveLabel(row,activity);
    for(const block of feed.querySelectorAll('.team-work-block'))syncTeamWorkBlock(block);
  }
  function syncWorkProgress(cluster){
    if(!cluster)return;
    const block=cluster.closest(".team-work-block"),body=block?.querySelector(".team-work-body"),owner=state.item?.kind==="group"?initiatingAgentForTurn({turn_id:cluster.dataset.turnId}):state.item?.id;
    const ownsAnswer=canonicalAgentId(cluster.dataset.agent)===canonicalAgentId(owner),live=cluster.classList.contains("live"),collapsed=body?.hidden||!cluster.classList.contains("group-work-expanded");
    const host=body?.hidden?block:cluster,other=host===block?cluster:block;
    if(ownsAnswer)other?.querySelector(":scope > .work-progress")?.remove();
    if(!ownsAnswer||!live||!collapsed||!cluster.dataset.latestProgress){cluster.querySelector(":scope > .work-progress")?.remove();if(ownsAnswer)block?.querySelector(":scope > .work-progress")?.remove();return;}
    let note=host.querySelector(":scope > .work-progress");
    if(!note){note=document.createElement("div");note.className="work-progress markdown";(host===block?block.querySelector(":scope > header"):cluster.querySelector(".group-work-agent")).after(note);}
    const text=cluster.dataset.latestProgress;if(note.dataset.progress!==text){note.dataset.progress=text;note.innerHTML=markdown(text);}
  }
  function syncTeamWorkSummary(cluster){
    if(!cluster?.classList.contains("group-work-cluster"))return;
    const header=cluster.querySelector(".group-work-agent"),detail=header?.querySelector("small");if(!detail)return;
    const hasActivity=Boolean(cluster.querySelector(".work-tool,.agent-update,.commentary-line,.reasoning-subgroup,.context-compaction,.coworker-return,.ask-history-row,.handoff-chain"));
    const worker=isEphemeralVolumeAgent(cluster.dataset.agent),live=cluster.classList.contains("live"),current=cluster.querySelector(".activity-current .trace-subgroup-label"),count=[...cluster.querySelectorAll(".work-tool")].reduce((sum,row)=>sum+(Number(row.dataset.repeatCount)||1),0);
    const failures=cluster.querySelectorAll(".work-tool.failed,.context-compaction[data-status=failed]"),unconfirmed=cluster.querySelectorAll(".work-tool[data-state=unknown]").length;
    const running=cluster.querySelector(".work-tool.running"),labels={queued:"Queued",waiting_user:"Waiting for you",blocked:"Blocked",failed:"Failed",done:"Replied",returned:"Returned work",cancelled:"Cancelled",stopped:"Stopped",continued:"Continued",inactive:"Unconfirmed"};
    let status=labels[cluster.dataset.teamState]||(cluster.classList.contains("stopped")?"Stopped":live?(cluster.querySelector(".context-compaction.running")?"Compacting context":running?current?.textContent||"Using a tool":current?.textContent||"Thinking"):"Activity recorded");
    if(cluster.closest('.team-work-block')?.dataset.interrupted==='true'&&!["waiting_user","blocked","failed","done","cancelled","continued"].includes(cluster.dataset.teamState))status="Stopped";
    if(!live&&!cluster.classList.contains('stopped')&&!labels[cluster.dataset.teamState]&&handoffRows().some(row=>row.dataset.turnId===cluster.dataset.turnId&&row.dataset.handoffTo===cluster.dataset.agent&&!handoffIsSettled(row)))status="Awaiting result";
    if(!live&&!hasActivity&&!labels[cluster.dataset.teamState])status="No activity recorded";
    if(failures.length)status=`${live?"Working · ":""}${failures.length} failed call${failures.length===1?"":"s"}`;
    else if(unconfirmed)status=`${status} · ${unconfirmed} unconfirmed call${unconfirmed===1?"":"s"}`;
    cluster.dataset.attention=String(failures.length>0||unconfirmed>0||["blocked","failed","waiting_user"].includes(cluster.dataset.teamState));
    let attention=cluster.querySelector(":scope > .work-attention");
    if(failures.length){
      if(!attention){attention=document.createElement("p");attention.className="work-attention";cluster.querySelector(".group-work-agent").after(attention);}
      const failed=failures[failures.length-1],detail=failed.dataset.copyText||failed.querySelector("small")?.textContent||"";
      attention.textContent=runtimeFailureSummary(detail)||humanFailureDetail(detail)||"An action failed. Open activity for details.";
    }else attention?.remove();
    if(worker){
      header.querySelector('.group-work-avatar').innerHTML='<svg class="worker-pool-mark" viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/></svg>';
      header.querySelector("strong").textContent="Worker pool";header.setAttribute("aria-label","Worker pool activity");
      const jobs=[...cluster.querySelectorAll('[data-worker-batch-size]')].reduce((sum,node)=>sum+Number(node.dataset.workerBatchSize||0),0),active=Number(cluster.dataset.activeWorkers)||0;
      if(active&&!running)status=`${active} active`;
      detail.textContent=`${status}${jobs?` · ${jobs} jobs`:""}${count?` · ${count} calls total`:""}`;
      header.title="Combined activity from temporary workers; the call count is shared across their jobs.";
    }else{
      detail.textContent=`${status}${count?` · ${count} call${count===1?"":"s"}`:""}`;
      header.title=running?`${agentLabel(cluster.dataset.agent)}: ${running.querySelector('.work-tool-name')?.textContent||status}${running.querySelector('.work-tool-copy small')?.textContent?` · ${running.querySelector('.work-tool-copy small').textContent}`:""}`:`${agentLabel(cluster.dataset.agent)}: ${status}`;
    }
    syncWorkProgress(cluster);
    syncTeamWorkBlock(cluster.closest('.team-work-block'));
  }
  function renderWorkerBatchNotice(event){
    const match=String(event.text||"").match(/^Volume batch `([a-z0-9]+)` started: (\d+) independent jobs/);
    if(!match||state.item?.kind!=="group")return false;
    const row=ensureWorkCluster("volume_worker"),list=row.querySelector(".work-tools");
    if(!row.querySelector(`[data-worker-batch-id="${match[1]}"]`)){
      const receipt=document.createElement("div");receipt.className="worker-batch-receipt";receipt.dataset.workerBatchId=match[1];receipt.dataset.workerBatchSize=match[2];receipt.textContent=`Batch of ${match[2]} jobs`;list.append(receipt);
    }
    row.classList.add("has-tools");syncTeamWorkSummary(row);return true;
  }
  function syncActivityCursor(cluster,preferred=null) {
    if(!cluster?.isConnected)return;
    cluster.querySelectorAll(".tool-current").forEach(node=>node.classList.remove("tool-current"));
    cluster.querySelectorAll(".activity-current").forEach((node)=>node.classList.remove("activity-current"));
    const selectable=(node)=>node?.isConnected&&node.closest(".work-cluster")===cluster&&node.classList.contains("running");
    let current=selectable(preferred)?preferred:null;
    if(!current){
      const candidates=[...cluster.querySelectorAll(".reasoning-subgroup.running,.trace-subgroup.running,.context-compaction.running")];
      current=candidates.reverse().find(selectable)||null;
    }
    if(!current){syncTeamWorkSummary(cluster);return;}
    current.classList.add("activity-current");
    if(current.matches(".trace-subgroup"))[...current.querySelectorAll(".work-tool.running")].at(-1)?.classList.add("tool-current");
    const sequence=current.closest(".reasoning-cluster");
    if(sequence){sequence.classList.add("activity-current");setReasoningClusterRunningState(sequence,true);}
    syncTeamWorkSummary(cluster);
  }
  function setReasoningClusterRunningState(group,running){
    group?.classList.toggle("running",Boolean(running));
    const copy=group?.querySelector(":scope > summary .reasoning-cluster-label");
    if(copy)copy.textContent=running?"Thinking":"Thought";
  }
  function updateReasoningCluster(group) {
    if(!group)return;
    const count=group.querySelectorAll(":scope > .reasoning-cluster-body > .reasoning-subgroup").length;
    const badge=group.querySelector(":scope > summary .reasoning-cluster-count");
    if(badge){badge.textContent=String(count);badge.hidden=count<2;}
  }
  function settleReasoningSubgroups(cluster) {
    clearTimeout(state.reasoningCursorTimer);state.reasoningCursorTimer=0;
    prunePendingReasoning(cluster);
    cluster?.querySelectorAll(".reasoning-subgroup.running").forEach((group)=>setReasoningGroupRunning(group,false));
    cluster?.querySelectorAll(".reasoning-cluster.running").forEach((group)=>setReasoningClusterRunning(group,false));
    cluster?.querySelectorAll(":scope .work-tools > .reasoning-subgroup, :scope .work-tools > .reasoning-cluster").forEach((group)=>group.dataset.closed="true");
    syncActivityCursor(cluster);
  }
  function createReasoningSubgroup() {
    const group=document.createElement("details");
    group.className="reasoning-subgroup";
    group.innerHTML=`<summary aria-expanded="false"><span class="reasoning-subgroup-icon"><span class="reasoning-subgroup-rest">${thoughtBubbleMarkup()}</span><span class="reasoning-subgroup-cursor">${loaderMarkup("helix")}</span></span><strong class="reasoning-subgroup-label">Thought</strong><svg class="reasoning-subgroup-chevron" viewBox="0 0 20 20" aria-hidden="true"><path d="m6 8 4 4 4-4"/></svg></summary><div class="reasoning-subgroup-body"></div>`;
    group.addEventListener("toggle",()=>group.querySelector(":scope>summary")?.setAttribute("aria-expanded",String(group.open)));
    return group;
  }
  function createReasoningCluster() {
    const group=document.createElement("details");
    group.className="reasoning-cluster";
    group.innerHTML=`<summary aria-expanded="false"><span class="reasoning-cluster-icon"><span class="reasoning-cluster-rest">${thoughtBubbleMarkup()}</span><span class="reasoning-cluster-cursor">${loaderMarkup("helix")}</span></span><strong class="reasoning-cluster-label">Thought</strong><small class="reasoning-cluster-count" hidden>0</small><svg class="reasoning-cluster-chevron" viewBox="0 0 20 20" aria-hidden="true"><path d="m6 8 4 4 4-4"/></svg></summary><div class="reasoning-cluster-body"></div>`;
    group.addEventListener("toggle",()=>group.querySelector(":scope>summary")?.setAttribute("aria-expanded",String(group.open)));
    return group;
  }
  function ensureReasoningSubgroup(cluster) {
    const list=cluster.querySelector(".work-tools");
    const last=list.lastElementChild;
    let sequence=null;
    if(last?.classList.contains("reasoning-cluster")&&last.dataset.closed!=="true"){
      sequence=last;
      sequence.querySelectorAll(".reasoning-subgroup.running").forEach((group)=>setReasoningGroupRunning(group,false));
    }else if(last?.classList.contains("reasoning-subgroup")&&last.dataset.closed!=="true"){
      setReasoningGroupRunning(last,false);
      sequence=createReasoningCluster();
      list.insertBefore(sequence,last);
      sequence.querySelector(".reasoning-cluster-body").append(last);
    }
    const group=createReasoningSubgroup();
    if(sequence){
      sequence.querySelector(".reasoning-cluster-body").append(group);
      setReasoningClusterRunning(sequence,!state.painting&&cluster.classList.contains("live"));
      updateReasoningCluster(sequence);
    }else list.append(group);
    setReasoningGroupRunning(group,!state.painting&&cluster.classList.contains("live"));
    return group;
  }
  function currentReasoningSubgroup(cluster) {
    const last=cluster?.querySelector(".work-tools")?.lastElementChild;
    if(last?.classList.contains("reasoning-subgroup"))return last;
    if(last?.classList.contains("reasoning-cluster")&&last.dataset.closed!=="true")return last.querySelector(":scope > .reasoning-cluster-body > .reasoning-subgroup:last-child");
    return null;
  }
  function syncReasoningDisclosure(group){
    if(!group)return;
    const hasDetail=Boolean(group.querySelector(".reasoning-subgroup-body")?.children.length);
    group.classList.toggle("summary-only",!hasDetail);
    if(!hasDetail)group.open=false;
    group.querySelector(":scope>summary")?.setAttribute("aria-expanded",String(hasDetail&&group.open));
    const chevron=group.querySelector(".reasoning-subgroup-chevron");if(chevron)chevron.hidden=!hasDetail;
  }
  function prunePendingReasoning(cluster){
    cluster?.querySelectorAll(".reasoning-subgroup.pending-activity").forEach((group)=>{
      const sequence=group.closest(".reasoning-cluster");group.remove();
      if(sequence){updateReasoningCluster(sequence);if(!sequence.querySelector(".reasoning-subgroup"))sequence.remove();}
    });
  }
  function ensureLiveThinkingPlaceholder(cluster){
    if(state.painting||!state.working||!cluster?.classList.contains("live")||Number(cluster.dataset.live)>0)return null;
    let group=cluster.querySelector(".reasoning-subgroup.pending-activity");
    if(!group){
      group=ensureReasoningSubgroup(cluster);group.classList.add("pending-activity");
      group.dataset.summary="Thinking";group.dataset.awaitingDetail="true";syncReasoningDisclosure(group);
    }
    setReasoningGroupRunning(group,true);syncActivityCursor(cluster,group);return group;
  }
  function clearProviderRetry(){
    if(state.painting)return;
    clearInterval(state.providerRetryTimer);state.providerRetryTimer=0;state.providerRetryDeadline=0;
    const group=state.providerRetryGroup;state.providerRetryGroup=null;if(!group?.isConnected)return;
    group.querySelector(".provider-retry-line")?.remove();group.classList.remove("provider-retry");
    if(group.dataset.providerRetryOnly==="true"){
      delete group.dataset.providerRetryOnly;group.classList.add("pending-activity");group.dataset.summary="Thinking";group.dataset.awaitingDetail="true";group.open=false;
    }else{
      group.dataset.summary=group.dataset.providerRetrySummary||group.dataset.summary||"Thinking";group.open=group.dataset.providerRetryOpen==="true";
    }
    delete group.dataset.providerRetrySummary;delete group.dataset.providerRetryOpen;setReasoningGroupRunning(group,state.working);syncReasoningDisclosure(group);
  }
  function renderProviderRetry(text,agent){
    const retry=providerRetryParts(text);if(!retry)return null;
    let group=state.providerRetryGroup?.isConnected?state.providerRetryGroup:null,cluster=ensureWorkCluster(agent||state.item?.id||"phoenix");
    if(!group){
      group=cluster.querySelector(".reasoning-subgroup.pending-activity")||ensureLiveThinkingPlaceholder(cluster)||currentReasoningSubgroup(cluster)||ensureReasoningSubgroup(cluster);state.providerRetryGroup=group;
      group.dataset.providerRetryOnly=String(!group.querySelector(".reasoning-subgroup-body")?.children.length);group.dataset.providerRetrySummary=group.dataset.summary||"Thinking";group.dataset.providerRetryOpen=String(group.open);
    }
    group.classList.remove("pending-activity","summary-only");group.classList.add("provider-retry");group.dataset.awaitingDetail="false";group.dataset.summary=group.dataset.providerRetrySummary||"Thinking";group.open=true;
    const body=group.querySelector(".reasoning-subgroup-body");let line=body.querySelector(".provider-retry-line");if(!line){line=document.createElement("div");line.className="provider-retry-line";line.innerHTML='<strong></strong><span></span>';body.append(line);}
    line.querySelector("strong").textContent=retry.message;state.providerRetryDeadline=Date.now()+retry.seconds*1000;
    const tick=()=>{const remaining=Math.max(0,Math.ceil((state.providerRetryDeadline-Date.now())/1000));line.querySelector("span").textContent=`${remaining?`Retrying in ${remaining}s`:"Retrying now…"} · attempt ${retry.attempt}`;};tick();clearInterval(state.providerRetryTimer);state.providerRetryTimer=setInterval(tick,250);
    setReasoningGroupRunning(group,true);syncReasoningDisclosure(group);syncActivityCursor(cluster,group);cluster.classList.add("has-progress","live");cluster.querySelector(".work-tools").hidden=false;scrollLatest();return group;
  }
  function armReasoningCursor(group,delay=5000) {
    clearTimeout(state.reasoningCursorTimer);state.reasoningCursorTimer=0;
    if(state.painting||state.working||!group)return;
    state.reasoningCursorTimer=setTimeout(()=>{
      state.reasoningCursorTimer=0;
      if(!group.isConnected||!group.classList.contains("running"))return;
      setReasoningGroupRunning(group,false);
      const sequence=group.closest(".reasoning-cluster");if(sequence)setReasoningClusterRunning(sequence,false);
    },delay);
  }
  // A provider reasoning summary ("**Checking photo workflow**\n\n…") is
  // live status, not conversation. Legacy history stored them as commentary;
  // this recognises them so they never repaint as transcript rows.
  function isReasoningSummaryText(text){
    return /^\s*(?:\*\*[^*\n]{2,100}\*\*|#{1,4}\s+[^\n]{2,100})\s*(?:\n|$)/u.test(String(text||""));
  }
  // Thinking only relabels the live working cube while this turn is running.
  // It never adds a row, and it disappears when real activity replaces it.
  function renderThinking(_agent,text) {
    if(!state.painting&&state.working&&!isCompactionText(text))clearProviderRetry();
  }
  // Only user_update intentionally publishes an intermediate message.
  // Ordinary model prose and reasoning never become chat bubbles.
  function renderAgentUpdate(agent,text,muted=false,explicit=false,publicReply=false){
    if(runtimeFailureSummary(text))return renderRuntimeFailure(text,agent);
    if(isCompactionText(text))return null;
    clearProviderRetry();
    if(!explicit)return null;
    const cleaned=safeRuntimeCopy(String(text||"")).trim();
    if(!cleaned)return null;
    clearProviderRetry();
    const fingerprint=progressFingerprint(cleaned),speaker=canonicalAgentId(agent||state.item?.id||"phoenix"),turnId=state.renderingTurnId||state.activeTurnId||state.displayRows.at(-1)?.turn_id||"";
    // A live receipt and its canonical history row can describe the same
    // update. Deduplicate within its turn even if a tool row intervened.
    const existing=[...$("conversationFeed").querySelectorAll(":scope > .commentary-message")].find(node=>node.dataset.progressFingerprint===fingerprint&&node.dataset.speaker===speaker&&node.dataset.turnId===turnId&&(node.dataset.publicReply==="true")===publicReply);
    if(existing)return publicReply?existing:null;
    markHandoffsWorking(agent);
    const cluster=ensureWorkCluster(agent||state.item?.id||"phoenix");
    delete cluster.dataset.latestProgress;syncWorkProgress(cluster);
    const profile=agentProfile(agent||state.item?.id||"phoenix");
    const html=avatar(profile)+'<div class="message-content" data-slot="message-content"><header><strong>'+escape(profile?.display_name||currentName())+'</strong></header><div class="markdown">'+markdown(cleaned)+'</div></div>';
    const node=feedNode("message-row agent-message commentary-message"+(muted?" muted":""),html,{agent:agent||"",slot:"message",from:"assistant",speaker:canonicalAgentId(agent||state.item?.id||"phoenix"),progressFingerprint:fingerprint});
    if(publicReply)node.dataset.publicReply="true";
    else PhoenixConversationUpdates.acceptUpdate(node);
    cluster.classList.add("has-progress");syncTeamWorkSummary(cluster);
    if(!state.painting)syncMessageGroups();
    scrollLatest();
    return node;
  }
  function renderCommentary(_agent, _text, _muted = false) { return null; }
  function appendReasoningLine(group,agent,kind,text) {
    if(!group||!text)return null;
    const body=group.querySelector(".reasoning-subgroup-body");if(!body)return null;
    const node=document.createElement("div");node.className=`commentary-line ${kind}`;node.innerHTML=`<div class="markdown">${markdown(text)}</div>`;
    node.dataset.agent=agent||"";node.dataset.kind=kind;body.append(node);syncReasoningDisclosure(group);return node;
  }
  function loaderMarkup(variant,size=16){return window.PhoenixAgentKit.loader(variant,size);}
  function pixelLoaderMarkup() {
    const delays=[90,0,90,180,90,180,270,180,270];
    return `<span class="pixel-loader pixel-drive" aria-hidden="true">${delays.map((delay)=>`<i style="--pixel-delay:${delay}ms"></i>`).join("")}</span>`;
  }
  function workClusterMarkup() {
    return `<button type="button" class="turn-delete-button work-delete-button" data-delete-agent-turn aria-label="Permanently delete this complete agent turn" title="Delete agent turn">${deleteIcon()}</button><button type="button" class="group-work-agent" aria-expanded="false" hidden><span class="group-work-avatar"></span><span><strong></strong><small></small></span><span class="group-work-live">${loaderMarkup("bars",14)}</span><svg viewBox="0 0 20 20" aria-hidden="true"><path d="m6.5 8 3.5 3.5L13.5 8"/></svg></button><div class="work-trace"><span class="work-trace-line" aria-hidden="true"></span><div class="work-tools"></div></div>`;
  }
  function savedWorkDisclosure(key){
    if(!key)return undefined;
    if(state.groupWorkDisclosure.has(key))return state.groupWorkDisclosure.get(key);
    try{const saved=localStorage.getItem(`phoenix-work-disclosure:${key}`);if(saved==="true"||saved==="false"){const expanded=saved==="true";state.groupWorkDisclosure.set(key,expanded);return expanded;}}catch{}
    return undefined;
  }
  function rememberWorkDisclosure(key,expanded){
    if(!key)return;
    state.groupWorkDisclosure.set(key,expanded);
    try{localStorage.setItem(`phoenix-work-disclosure:${key}`,String(expanded));}catch{}
  }
  function setWorkDisclosure(cluster,expanded){
    cluster.classList.toggle("group-work-expanded",expanded);
    const header=cluster.querySelector(".group-work-agent"),trace=cluster.querySelector(".work-trace");
    if(trace){trace.id ||= `work-trace-${++state.workDisclosureSequence}`;trace.hidden=!expanded;header?.setAttribute("aria-controls",trace.id);}
    header?.setAttribute("aria-expanded",String(expanded));
  }
  function toggleGroupTurnWork(cluster){
    if(!cluster)return;
    const expanded=!cluster.classList.contains("group-work-expanded");cluster.dataset.userDisclosure="true";
    setWorkDisclosure(cluster,expanded);
    rememberWorkDisclosure(cluster.dataset.disclosureKey,expanded);
    syncWorkProgress(cluster);
  }
  function decorateGroupWorkCluster(cluster,agent){
    if(!cluster)return cluster;
    const groupOwner=String(agent||"").startsWith("group:"),profile=groupOwner?currentProfile():knownAgentProfile(agent),header=cluster.querySelector(".group-work-agent"),name=groupOwner?currentName():(profile?.display_name||agentLabel(agent)||currentName());
    cluster.classList.add("group-work-cluster");
    if(!cluster.dataset.disclosureReady){
      const key=cluster.dataset.turnId?`${conversationKey()}:${cluster.dataset.turnId}:${agent}`:"";
      if(key)cluster.dataset.disclosureKey=key;
      const saved=savedWorkDisclosure(key),expanded=saved??toolActivityView();
      setWorkDisclosure(cluster,expanded);
      if(saved!==undefined)cluster.dataset.userDisclosure="true";
      cluster.dataset.disclosureReady="true";
    }cluster.style.setProperty("--agent-accent",ui.profileColor(profile||currentProfile()));
    if(!state.painting&&state.working){cluster.classList.add("live");cluster.dataset.startedAt ||= String(state.turnStartedAt||Date.now());}
    if(header){header.hidden=false;header.setAttribute("aria-label",`${name} activity`);header.title=`${name} activity`;header.querySelector(".group-work-avatar").innerHTML=ui.avatarSvg(profile||{agent_id:agent,display_name:name,color:ui.profileColor(currentProfile())});header.querySelector("strong").textContent=name;const detail=header.querySelector("small");if(detail&&!detail.textContent.startsWith("→"))detail.textContent=cluster.classList.contains("live")?"Working":"";}
    if(cluster.classList.contains("team-work-row")){
      const status=[...$("conversationFeed").querySelectorAll(".group-execution-strip")].find(row=>row.dataset.turnId===cluster.dataset.turnId)?.querySelector(`[data-group-status-agent="${CSS.escape(agent)}"]`);
      if(status)cluster.dataset.teamState=status.dataset.state;
      if(cluster.classList.contains("live")&&["done","failed","cancelled","continued"].includes(cluster.dataset.teamState))delete cluster.dataset.teamState;
    }
    syncTeamWorkSummary(cluster);
    return cluster;
  }
  function ensureTeamWorkBlock(turnId) {
    if(state.item?.kind!=="group"||!turnId)return null;
    const feed=$("conversationFeed");
    let block=[...feed.querySelectorAll(':scope > .team-work-block')].find(node=>node.dataset.turnId===turnId);
    if(block)return block;
    // Keep turn ownership for receipts, without another disclosure around
    // each coworker's existing activity button.
    block=feedNode('team-work-block','<div class="team-work-body"></div>',{turnId});
    return block;
  }
  function syncTeamWorkBlock(block) {
    if(!block)return;
    const members=new Map(),turn=block.dataset.turnId;
    for(const node of block.querySelectorAll('[data-group-status-agent]'))members.set(node.dataset.groupStatusAgent,node.dataset.state);
    for(const row of block.querySelectorAll('.team-work-row')){
      let status=row.dataset.teamState||(row.classList.contains('stopped')?'cancelled':row.classList.contains('live')?'working':'recorded');
      if(status==='working'&&(turn!==state.activeTurnId||(row.dataset.runtimeKnown==='true'&&row.dataset.runtimeActive!=='true')))status='waiting';
      if(!members.has(row.dataset.agent)||status!=='recorded')members.set(row.dataset.agent,status);
    }
    for(const row of block.querySelectorAll('.handoff-chain')){
      const agent=row.dataset.handoffTo;
      if(!members.has(agent))members.set(agent,row.dataset.handoffState==='inactive'?'inactive':handoffIsSettled(row)?row.dataset.handoffState:'waiting');
      else if(!handoffIsSettled(row)&&['done','recorded','continued'].includes(members.get(agent)))members.set(agent,'waiting');
    }
    const activity=ui.activityFor?.(state.item),active=activity?new Set((LIVE_CONVERSATION_STATUSES.has(activity.status)?activity.active_agent_ids||[]:[]).map(canonicalAgentId)):null;
    for(const [agent,status] of members){
      if(status!=='working')continue;
      const workerLive=isEphemeralVolumeAgent(agent)&&block.querySelector(`.team-work-row[data-agent="${CSS.escape(agent)}"][data-runtime-active="true"]`);
      if(turn!==state.activeTurnId||!state.working||(active&&!active.has(agent)&&!workerLive))members.set(agent,'waiting');
    }
    if(block.dataset.interrupted==='true')for(const [agent,status] of members){if(['queued','working','waiting','recorded'].includes(status))members.set(agent,'stopped');}
    const values=[...members.values()],count=status=>values.filter(value=>status.includes(value)).length;
    const working=count(['working']),waiting=count(['waiting_user']),blocked=count(['blocked','failed']);
    const failures=block.querySelectorAll(".work-tool.failed,.context-compaction[data-status=failed]").length;
    block.dataset.live=String(working>0);block.dataset.attention=String(waiting+blocked+failures>0);
    const substance=block.querySelector(".team-work-body .work-tool,.team-work-body .handoff-chain,.team-work-body .coworker-return,.team-work-body .context-compaction");
    const pending=state.working&&turn===state.activeTurnId&&count(["queued","waiting"]);
    block.hidden=!(working||waiting||blocked||pending||substance);
  }
  function placeInTeamWorkBlock(node) {
    const block=ensureTeamWorkBlock(node?.dataset.turnId);
    if(!block)return;
    block.querySelector('.team-work-body').append(node);
    syncTeamWorkBlock(block);
    placeHandoffBeforeAnswer(node);
  }
  function ensureWorkCluster(agent) {
    agent=canonicalAgentId(agent)||agent;
    const feed = $("conversationFeed");
    const ownerTurn=state.renderingTurnId||state.activeTurnId;
    const sameTurn=(node)=>!ownerTurn||node?.dataset.turnId===ownerTurn;
    if(state.recoveredWorkCluster?.isConnected&&sameTurn(state.recoveredWorkCluster)&&canonicalAgentId(state.recoveredWorkCluster.dataset.agent)===canonicalAgentId(agent))return state.recoveredWorkCluster;
    if(state.item?.kind==="group"){
      const boundary=[...feed.querySelectorAll(':scope > .user-message:not([data-queued-pending="true"])')].at(-1);
      const inRequest=node=>ownerTurn?sameTurn(node):(!boundary||Boolean(boundary.compareDocumentPosition(node)&Node.DOCUMENT_POSITION_FOLLOWING));
      const roster=[...feed.querySelectorAll(".team-work-row")].filter(inRequest);
      const existing=roster.find(node=>canonicalAgentId(node.dataset.agent)===agent);
      if(existing){if(state.painting)state.replayWorkCluster=existing;return decorateGroupWorkCluster(existing,agent);}
      const node=feedNode("work-cluster team-work-row",workClusterMarkup());
      node.dataset.agent=agent||"";node.dataset.live="0";node.dataset.startedAt=String(state.turnStartedAt||Date.now());
      if(ownerTurn)placeInTeamWorkBlock(node);
      else if(roster.length)roster.at(-1).after(node);
      if(state.painting)state.replayWorkCluster=node;
      return decorateGroupWorkCluster(node,agent);
    }

    // Work after a message (an answer to Tibo's question, say) belongs below
    // that message, never back in the block above it.
    const tailFree=(node)=>{let after=node?.nextElementSibling;while(after&&after.classList.contains("commentary-line"))after=after.nextElementSibling;return !after||!after.classList.contains("message-row");};
    if(state.painting&&state.replayWorkCluster?.isConnected&&sameTurn(state.replayWorkCluster)&&tailFree(state.replayWorkCluster)){
      const current=canonicalAgentId(state.replayWorkCluster.dataset.agent||""),wanted=canonicalAgentId(agent||"");
      // One shared group turn can interleave several coworkers. Reusing the
      // replay cursor across a speaker change merged Theo's thoughts into
      // another coworker's tool block and let the last speaker overwrite the
      // header/avatar. Keep contiguous work per coworker instead.
      if(!current||!wanted||current===wanted)return decorateGroupWorkCluster(state.replayWorkCluster,agent);
      state.replayWorkCluster=null;
    }
    // One user turn owns one trace. Questions and approvals can be rendered
    // between tool events, so DOM adjacency is not a reliable turn boundary.
    // Reuse the live turn cluster instead of creating a second "other action"
    // block when work resumes after the user answers.
    if (state.turnStatus?.isConnected && state.working && sameTurn(state.turnStatus) && lastConversationRow()===state.turnStatus) {
      const currentAgent=String(state.turnStatus.dataset.agent||""),wanted=String(agent||"");
      if(!currentAgent||!wanted||canonicalAgentId(currentAgent)===canonicalAgentId(wanted))return decorateGroupWorkCluster(state.turnStatus,agent);
    }
    if (state.turnStatus?.isConnected && sameTurn(state.turnStatus) && lastConversationRow()===state.turnStatus && canonicalAgentId(state.turnStatus.dataset.agent)===canonicalAgentId(agent) && state.turnStatus.classList.contains("turn-pending") && !state.turnStatus.querySelector(".work-tool")) {
      state.turnStatus.dataset.agent = agent || "";
      state.turnStatus.classList.remove("turn-pending");
      return decorateGroupWorkCluster(state.turnStatus,agent);
    }
    const last = [...feed.querySelectorAll(".work-cluster")].at(-1);
    if (last && sameTurn(last) && (!last.dataset.agent || canonicalAgentId(last.dataset.agent) === canonicalAgentId(agent))) {
      let after = last.nextElementSibling;
      while (after && after.classList.contains("commentary-line")) after = after.nextElementSibling;
      if (!after) return decorateGroupWorkCluster(last,agent);
    }
    // Two turns of one coworker can run at once (a scheduled routine beside
    // the user's request); their saved steps interleave. Rejoin this turn's
    // own work block instead of opening a new header at every switch, unless
    // this turn has already shown a message after that block.
    if (ownerTurn) {
      const ownBlock = [...feed.querySelectorAll(":scope > .work-cluster")].reverse()
        .find((node) => node.dataset.turnId === ownerTurn && canonicalAgentId(node.dataset.agent || "") === canonicalAgentId(agent || ""));
      if (ownBlock) {
        let later = ownBlock.nextElementSibling, spoke = false;
        for (; later; later = later.nextElementSibling) if (later.classList.contains("message-row") && (later.dataset.turnId === ownerTurn || later.classList.contains("user-message"))) { spoke = true; break; }
        if (!spoke) { if (state.painting) state.replayWorkCluster = ownBlock; return decorateGroupWorkCluster(ownBlock, agent); }
      }
    }
    const node = feedNode("work-cluster", workClusterMarkup());
    node.dataset.agent = agent || "";
    node.dataset.live = "0";
    node.dataset.startedAt=String(state.turnStartedAt||Date.now());
    if(state.painting)state.replayWorkCluster=node;
    return decorateGroupWorkCluster(node,agent);
  }
  function beginTurnActivity(prompt, agent=state.item?.kind==="group"?`group:${state.item.id}`:(targetAgent() || "phoenix")) {
    if(state.item?.kind==="group"&&String(agent).startsWith("group:")){
      const active=ui.activityFor(state.item)?.active_agent_ids||state.activeGroupAgentIds;
      [...new Set((active||[]).map(canonicalAgentId).filter(Boolean))].forEach(id=>beginTurnActivity(prompt,id));
      return;
    }
    const feed = $("conversationFeed");
    const cluster = ensureWorkCluster(agent);
    cluster.classList.add("live", "turn-pending");
    cluster.dataset.startedAt=String(state.turnStartedAt||Date.now());
    cluster.dataset.live = "0";
    const trace=cluster.querySelector(".work-tools");if(trace)trace.hidden=false;
    state.turnStatus = cluster;
    state.turnUsageSeen=false;
    ensureLiveThinkingPlaceholder(cluster);
    stopTurnMood();
    state.turnFocusUntil = performance.now() + 450;
    feed.classList.add("turn-active");
    updateTaskHeadline("Thinking through the request", true);
    const focusPrompt=()=>{
      if (!prompt?.isConnected || !state.working) return;
      const highest = Math.max(0, feed.scrollHeight - feed.clientHeight);
      feed.scrollTop = Math.min(highest, Math.max(0, prompt.offsetTop - 18));
      updatePromptRailActive();
    };
    // Do it now (forcing current layout) and once on the next paint. The old
    // nested-frame-only path raced with feedNode's pending scroll-to-latest.
    focusPrompt();
    requestAnimationFrame(focusPrompt);
  }
  function stopTurnMood() {}
  function settleWorkCluster(cluster, preserveWork = false, suppliedMeta = null) {
    if (!cluster?.isConnected) return;
    cluster.classList.remove("live", "turn-pending", "tools-running");
    cluster.classList.add("settled");
    delete cluster.dataset.latestProgress;syncWorkProgress(cluster);
    if(cluster.dataset.userDisclosure!=="true")setWorkDisclosure(cluster,toolActivityView());
    // A terminal turn must not leave a row shimmering forever. Without the
    // matching completion receipt, however, stopping the animation is the
    // only fact we know; painting a green check would invent tool success.
    cluster.querySelectorAll(".work-tool.running").forEach((row) => {
      row.classList.remove("running");
      row.dataset.state=preserveWork?"cancelled":"unknown";
      const copy=preserveWork?"Stopped":"Ended without receipt",kit=window.PhoenixAgentKit;
      row.querySelectorAll(".tr-status.s-running,.tool-status.running").forEach(status=>{
        status.className=`tr-status ${preserveWork?"s-cancelled":"s-unknown"}`;
        status.innerHTML=`${kit.icon(preserveWork?"ban":"clock")}<span>${copy}</span>`;
      });
      const editState=row.querySelector(".fd-state");
      if(editState){editState.setAttribute("aria-label",copy);editState.innerHTML=kit.icon(preserveWork?"ban":"clock");}
    });
    cluster.querySelectorAll('.context-compaction.running').forEach(row=>{
      row.classList.remove('running','activity-current');row.dataset.status='interrupted';
      row.querySelector('strong').textContent='Context preparation ended';
      row.querySelector('small').textContent='The run ended without a compaction completion receipt';
    });
    settleReasoningSubgroups(cluster);
    const groupDetail=cluster.querySelector(".group-work-agent small");if(groupDetail&&!groupDetail.textContent.startsWith("→"))groupDetail.textContent=preserveWork?"Stopped":"";
    [...state.activeTools].forEach(([key, entry]) => { if (entry.cluster === cluster) state.activeTools.delete(key); });
    const tools = cluster.querySelector(".work-tools");
    if (cluster.querySelector(".work-tool,.agent-update,.commentary-line,.ask-history-row,.handoff-chain,.context-compaction,.coworker-return")) {
      const started=Number(cluster.dataset.startedAt)||0;
      const elapsed=Number(suppliedMeta?.elapsed_ms)||Number(cluster.dataset.elapsedMs)||(!state.painting&&started?Math.max(0,Date.now()-started):0);
      cluster.dataset.elapsedMs=String(elapsed);
      cluster.querySelectorAll(".trace-subgroup").forEach((group)=>{group.dataset.running="0";group.classList.remove("running","activity-current");updateTraceSubgroup(group);});
      cluster.classList.toggle("stopped", preserveWork);
      if (tools) tools.hidden = false;
    } else if(!cluster.classList.contains("team-work-row"))cluster.remove();
    syncTeamWorkSummary(cluster);
  }
  function settleAgentWork(agent, preserveWork = false) {
    const wanted=canonicalAgentId(agent||"");
    document.querySelectorAll(".work-cluster").forEach((cluster)=>{
      if(canonicalAgentId(cluster.dataset.agent||"")===wanted)settleWorkCluster(cluster,preserveWork);
    });
    syncWorkShimmer();
  }
  function finishTurnActivity(preserveWork = false) {
    clearProviderRetry();
    stopTurnMood();
    const block=[...$("conversationFeed").querySelectorAll('.team-work-block')].find(node=>node.dataset.turnId===state.activeTurnId);
    if(preserveWork&&block){
      block.dataset.interrupted='true';
      // Project unfinished members as stopped without rewriting their journal
      // or claiming a cancellation/completion receipt that never arrived.
      for(const member of block.querySelectorAll('[data-group-status-agent]')){
        if(['queued','working'].includes(member.dataset.state))updateGroupMemberStatus(member,'stopped','The run ended before this work returned.');
      }
    }
    const cluster = state.turnStatus;
    settleWorkCluster(cluster, preserveWork);
    document.querySelectorAll(".work-cluster.live,.work-cluster.tools-running").forEach((node)=>settleWorkCluster(node,preserveWork));
    state.turnStatus = null;
    if(block){
      for(const strip of block.querySelectorAll('.group-execution-strip'))updateGroupStatusSummary(strip);
      for(const row of block.querySelectorAll('.handoff-chain'))syncHandoffLiveLabel(row);
      syncOwnerHandoffUpdate(block.dataset.turnId);
    }
    $("conversationFeed").classList.remove("turn-active");
    syncComposerFade();
  }
  function syncWorkShimmer() {
    const active=[...state.activeTools.values()],running=new Set(active.map((entry)=>entry.cluster));
    state.shimmerClusters.forEach((cluster)=>{if(!running.has(cluster))cluster.classList.remove("tools-running");});
    running.forEach((cluster)=>cluster.classList.add("tools-running"));
    state.shimmerClusters=running;
  }
  function toolResultDetail(event) {
    if(event.ok===false)return humanFailureDetail(event.detail,event.tool);
    const detail=safeRuntimeCopy(event.detail||"");
    return detail&&detail!==humanTarget(event.target,event.tool)?detail:"";
  }
  function generatedImagePath(event) {
    if(!/^(?:image_gen|image_generate)$/i.test(String(event.tool||""))||event.ok===false)return"";
    const match=String(event.detail||"").match(/Image generated:\s+([^\s]+\.(?:png|jpe?g|webp|avif))/i);if(!match)return"";
    const path=match[1].replace(/[),.;]+$/g,"");
    if(path.startsWith("/"))return path;
    return state.workspace?`${state.workspace.replace(/\/$/,"")}/${path}`:path;
  }
  async function hydrateGeneratedImage(row,path) {
    const surface=row.querySelector(".tool-image-preview");if(!surface||!path)return;
    try{
      const source=preview?ui.phoenixLogoSource():await ui.invoke("image_data_url",{path});
      if(!surface.isConnected)return;surface.innerHTML=`<img src="${escape(source)}" alt="Generated image"><span><strong>Generated image</strong><small>${escape(path.split("/").at(-1)||"Image")}</small></span>`;surface.dataset.inspectImagePath=path;surface._inspectionSource=source;surface.setAttribute("role","button");surface.setAttribute("tabindex","0");surface.setAttribute("aria-label","Open generated image in activity sidebar");row.classList.add("has-image");scrollLatest();
    }catch{surface.innerHTML="<span class=\"image-preview-unavailable\">Preview unavailable</span>";}
  }
  function orbitLoaderMarkup() {
    const delays=[0,110,220,770,null,330,660,550,440];
    return `<span class="pixel-loader pixel-orbit" aria-hidden="true">${delays.map((delay)=>`<i${delay==null?' class="pixel-empty"':` style="--pixel-delay:${delay}ms"`}></i>`).join("")}</span>`;
  }
  function parseToolPayload(value) {
    if(!value)return null;
    if(typeof value==="object")return value;
    try{return JSON.parse(String(value));}catch{return null;}
  }
  function webSearchQuery(event) {
    const payload=parseToolPayload(event.target);
    const first=Array.isArray(payload?.queries)?payload.queries[0]:null;
    return String(payload?.query||payload?.q||first?.query||first?.q||first?.use_case||"").trim().slice(0,180);
  }
  function webSources(event) {
    const found=[];
    const add=(url,label="")=>{
      try{
        const parsed=new URL(String(url));if(!/^https?:$/.test(parsed.protocol))return;
        const href=parsed.href,host=parsed.hostname.replace(/^www\./,"");
        if(found.some((item)=>item.host===host))return;
        found.push({href,host,label:String(label||host).trim().slice(0,100)});
      }catch{}
    };
    const walk=(value,depth=0)=>{
      if(depth>5||value==null)return;
      if(typeof value==="string"){
        for(const match of value.matchAll(/https?:\/\/[^\s<>"'\])},]+/g))add(match[0]);
        const parsed=parseToolPayload(value);if(parsed&&parsed!==value)walk(parsed,depth+1);
        return;
      }
      if(Array.isArray(value)){value.slice(0,40).forEach((item)=>walk(item,depth+1));return;}
      if(typeof value!=="object")return;
      const direct=value.url||value.href||value.link||value.source_url||value.canonical_url;
      if(direct)add(direct,value.title||value.name||value.label||"");
      Object.values(value).slice(0,60).forEach((item)=>walk(item,depth+1));
    };
    walk(event.detail);walk(event.result);walk(event.output);
    return found.slice(0,10);
  }
  function faviconMarkup(source, className="source-avatar") {
    const url=`https://${source.host}/favicon.ico`;
    return `<img class="${className}" src="${escape(url)}" alt="" loading="lazy" referrerpolicy="no-referrer" onerror="this.hidden=true">`;
  }
  function toolIconMarkup(event,visual) {
    const name=String(event.tool||"").toLowerCase();
    if(visual.image)return `<img class="tool-brand-image" src="${visual.image}" alt="">`;
    if(name==="web_search"){
      const sources=webSources(event);
      return sources.length?`<span class="source-avatar-stack">${sources.slice(0,3).map((source)=>faviconMarkup(source)).join("")}</span>`:'<span class="search-source-dots"><i></i><i></i><i></i></span>';
    }
    return visual.svg;
  }
  function webSourceUrlLabel(source){
    try{const value=new URL(source.href);const path=value.pathname==="/"?"":value.pathname.replace(/\/$/,"");return `${value.host.replace(/^www\./,"")}${path}`;}
    catch{return source.host||source.label||String(source.href||"");}
  }
  function webSearchResultMarkup(event) {
    if(String(event.tool||"").toLowerCase()!=="web_search"||event.kind==="tool_start")return"";
    const query=webSearchQuery(event),sources=webSources(event);
    if(!query&&!sources.length)return"";
    // One line per source — site mark then the url, never a card. Three are
    // visible and the remainder collapses behind "View all". <details> keeps
    // that behaviour without a JS binding, so it survives feed re-renders.
    const line=(source)=>`<a href="${escape(source.href)}" target="_blank" rel="noreferrer" title="${escape(source.label||source.href)}"><span class="web-source-logo">${faviconMarkup(source)}</span><strong>${escape(webSourceUrlLabel(source))}</strong></a>`;
    const rest=sources.slice(3);
    return `<div class="web-search-results">${query?`<div class="web-search-query"><span>Search</span><strong>${escape(query)}</strong></div>`:""}${sources.slice(0,3).map(line).join("")}${rest.length?`<details class="web-search-more"><summary>View all ${sources.length}</summary>${rest.map(line).join("")}</details>`:""}</div>`;
  }
  function toolStatusMarkup(running,failed) {
    if(running)return`<span class="tool-status running" aria-label="Running">Running</span>`;
    if(failed)return`<span class="tool-status failed">${icons.warn}<span>Failed</span></span>`;
    return`<span class="tool-status success">${icons.check}<span>Done</span></span>`;
  }
  function detailedConversationView() { return document.documentElement.dataset.conversationView==="detailed"; }
  function toolActivityView() { return ["compact","detailed"].includes(document.documentElement.dataset.conversationView); }
  let conversationDetailMode=null;
  function setToolRowDisclosure(row,open) {
    const summary=row?.querySelector(".work-tool-summary"),detail=row?.querySelector(".work-tool-detail"),hasDetail=Boolean(detail);
    const expanded=Boolean(open&&hasDetail);row?.classList.toggle("open",expanded);summary?.setAttribute("aria-expanded",String(expanded));if(detail)detail.hidden=!expanded;
  }
  function syncConversationDetail() {
    const feed=$("conversationFeed"),mode=document.documentElement.dataset.conversationView;
    if(mode!==conversationDetailMode){
      conversationDetailMode=mode;const visible=toolActivityView();
      feed?.querySelectorAll(".trace-subgroup").forEach((group)=>{group.open=detailedConversationView();});
      feed?.querySelectorAll(".work-cluster").forEach((cluster)=>{
        if(visible)cluster.classList.remove("work-folded");
        setWorkDisclosure(cluster,visible);
        const tools=cluster.querySelector(".work-tools");if(tools&&visible)tools.hidden=false;
      });
      feed?.querySelectorAll(".answer-work-toggle").forEach((button)=>button.setAttribute("aria-expanded",String(visible)));
    }
    feed?.querySelectorAll(".work-tool").forEach((row)=>setToolRowDisclosure(row,detailedConversationView()));
  }
  // Tool rows follow beUI's ToolResult (and FileDiff for edits): a one-line
  // trigger — kind icon, title, mono tool/target, status, chevron — over a
  // rounded output well with copy and a status footer.
  function toolKind(tool){const name=String(tool||"").toLowerCase();if(name==="bash"||/^(?:shell|terminal|run_)/.test(name))return"terminal";if(/^(?:browser_|web_|composio_|mcp__|http|fetch|crawl|scrape)/.test(name))return"request";return"custom";}
  function toolStatusCopy(status){return status==="running"?"Running":status==="error"?"Failed":status==="cancelled"?"Cancelled":"Completed";}
  function toolStatusIcon(status){const kit=window.PhoenixAgentKit;return status==="running"?kit.icon("loader","spin"):status==="error"?kit.icon("circleX"):status==="cancelled"?kit.icon("ban"):kit.icon("circleCheck");}
  function diffCounts(diff){let additions=0,deletions=0;String(diff||"").split("\n").forEach((line)=>{if(line.startsWith("+")&&!line.startsWith("+++"))additions+=1;else if(line.startsWith("-")&&!line.startsWith("---"))deletions+=1;});return{additions,deletions};}
  function fileDiffMarkup(diff){
    const rows=window.PhoenixAgentKit.parseDiff(diff);
    return`<div class="fd-lines"><span class="sr-only">File changes</span>${rows.map((row)=>`<div class="fd-line ${row.type}"><span class="fd-num">${row.oldLine??""}</span><span class="fd-num">${row.newLine??""}</span><span class="fd-sign">${row.type==="added"?"+":row.type==="removed"?"−":""}</span><span class="fd-code">${highlightCode(escape(row.content))||" "}</span></div>`).join("")}</div>`;
  }
  function paintToolRow(row, event, label, running = false) {
    const kit=window.PhoenixAgentKit,wasOpen=row.classList.contains("open"),target=humanTarget(event.target,event.tool),visual=toolVisual(event.tool),failure=event.ok===false,detail=toolResultDetail(event),diff=event.diff?safeRuntimeCopy(event.diff):"",imagePath=generatedImagePath(event),webResults=webSearchResultMarkup(event),visibleDetail=webResults?"":detail,hasDetail=Boolean(visibleDetail||diff||imagePath||webResults),open=hasDetail&&(wasOpen||detailedConversationView());
    const status=running?"running":failure?"error":"success",isEdit=EDIT_TOOLS.has(String(event.tool||"").toLowerCase());
    row.className=`work-tool ${isEdit?"file-diff":"tool-result"} tone-${visual.tone}${running?" running":""}${failure?" failed":""}${open?" open":""}`;row.dataset.state=status;
    row.dataset.toolCallId=String(event.call_id||event.tool_call_id||"");row.dataset.tool=String(event.tool||"");row.dataset.toolLabel=label;row.dataset.toolTarget=target;row.dataset.toolDiff=diff;row.dataset.copyText=visibleDetail||diff||"";
    const chevron=hasDetail?kit.icon("chevronDown","tr-chevron"):"",copy=visibleDetail||diff?`<button type="button" class="tool-detail-copy tr-action" aria-label="${isEdit?"Copy diff":"Copy result"}" title="${isEdit?"Copy diff":"Copy result"}">${kit.icon("copy")}</button>`:"";
    if(isEdit){
      const {additions,deletions}=diffCounts(diff);
      row.innerHTML=`<button type="button" class="work-tool-summary fd-trigger" aria-expanded="${open}" ${hasDetail?"":"data-empty=\"true\""}>${kit.icon("fileCode","fd-icon")}<span class="work-tool-copy fd-file"><strong class="work-tool-name sr-only">${escape(label)}</strong><small>${escape(target||label)}</small></span><span class="fd-counts">${additions?`<b>+${additions}</b>`:""}${deletions?`<i>−${deletions}</i>`:""}</span><span class="fd-state" aria-label="${running?"Applying changes":failure?"Edit failed":"Changes applied"}">${running?kit.icon("loader","spin"):failure?kit.icon("x"):kit.icon("check")}</span>${chevron}</button>${hasDetail?`<div class="work-tool-detail tr-body" ${open?"":"hidden"}><div class="tr-well">${diff?`<div class="tr-viewport fd-viewport">${fileDiffMarkup(diff)}</div>`:""}${visibleDetail&&!diff?`<div class="tr-viewport"><pre class="tr-output">${escape(visibleDetail)}</pre></div>`:""}${copy?`<div class="tr-foot fd-foot">${copy}</div>`:""}</div></div>`:""}`;
      return row;
    }
    const kindIcon={terminal:"terminal",request:"braces",custom:"wrench"}[toolKind(event.tool)];
    row.innerHTML=`<button type="button" class="work-tool-summary tr-trigger" aria-expanded="${open}" ${hasDetail?"":"data-empty=\"true\""}><span class="work-tool-icon tr-icon" aria-hidden="true">${kit.icon(kindIcon)}</span><span class="work-tool-copy tr-text"><strong class="work-tool-name tr-title">${escape(label)}</strong>${target?`<small class="tr-tool">${escape(target)}</small>`:`<small class="tr-tool">${escape(String(event.tool||""))}</small>`}</span><span class="tr-status s-${status}">${toolStatusIcon(status)}<span>${toolStatusCopy(status)}</span></span>${chevron}</button>${hasDetail?`<div class="work-tool-detail tr-body" ${open?"":"hidden"}><div class="tr-well"><div class="tr-viewport">${imagePath?`<div class="tool-image-preview" data-image-path="${escape(imagePath)}"><span>Loading preview…</span></div>`:""}${webResults}${visibleDetail?`<pre class="tr-output">${escape(visibleDetail)}</pre>`:""}${diff?`<pre class="tr-output">${escape(diff)}</pre>`:""}</div>${copy?`<div class="tr-foot">${copy}<span class="tr-foot-status">${toolStatusCopy(status)}</span></div>`:""}</div></div>`:""}`;
    if(imagePath)hydrateGeneratedImage(row,imagePath);
    return row;
  }
  function appendCompletedToolRow(list,event,label) {
    // Reconstructed history can contain the same successful receipt from the
    // story journal and canonical session. Exact repeats add no information;
    // retain their count on one row instead of making a wall of tool chrome.
    const signature=answerKey(JSON.stringify([event.tool||"",event.target||"",event.ok!==false,event.detail||"",label]));
    const existing=event.ok!==false?[...list.querySelectorAll(".work-tool:not(.running):not(.failed)")].find((row)=>row.dataset.toolSignature===signature):null;
    if(existing){
      const count=(Number(existing.dataset.repeatCount)||1)+1;existing.dataset.repeatCount=String(count);
      let badge=existing.querySelector(".work-tool-count");
      if(!badge){badge=document.createElement("b");badge.className="work-tool-count";existing.querySelector(".work-tool-name")?.append(badge);}
      badge.textContent=`×${count}`;
      return existing;
    }
    const row=paintToolRow(document.createElement("div"),event,label);
    row.dataset.toolSignature=signature;row.dataset.repeatCount="1";list.append(row);return row;
  }
  function traceCategory(tool) {
    const name=String(tool||"").toLowerCase();
    if(name==="web_search")return"search";
    if(/^(?:browser_|web_fetch|crawl|scrape)/.test(name))return"browser";
    if(/^(?:read|write|str_replace|apply_patch|bash|glob|codebase_search|index_codebase)/.test(name))return"coding";
    if(/^(?:composio_|mcp__)/.test(name))return"connected";
    return"tools";
  }
  function traceGroupIcon(category) {
    if(category==="browser")return`<img src="${CHROME_ICON}" alt="">`;
    if(category==="search")return icons.search;
    if(category==="coding")return'<svg viewBox="0 0 20 20"><path d="M6.5 5 2.8 10l3.7 5M13.5 5l3.7 5-3.7 5M11.5 3.5l-3 13"/></svg>';
    if(category==="connected")return'<svg viewBox="0 0 20 20"><path d="M7.5 6.5 9 5a3 3 0 0 1 4.2 4.2l-1.4 1.4M12.5 13.5 11 15a3 3 0 0 1-4.2-4.2l1.4-1.4"/></svg>';
    return icons.work;
  }
  function codingTraceLabel(group) {
    const names=new Set([...group.querySelectorAll(".work-tool")].map((row)=>String(row.dataset.tool||"").toLowerCase()));
    const parts=[];
    if([...names].some((name)=>/^(?:read)$/.test(name)))parts.push("read files");
    if([...names].some((name)=>/^(?:write|str_replace|apply_patch)$/.test(name)))parts.push("edited files");
    if(names.has("bash"))parts.push("ran commands");
    if([...names].some((name)=>/^(?:glob|codebase_search|index_codebase)$/.test(name)))parts.push("searched code");
    if(!parts.length)return"Ran tools";
    const sentence=parts.join(", ");return sentence[0].toUpperCase()+sentence.slice(1);
  }
  function updateTraceSubgroup(group) {
    if(!group)return;
    const category=group.dataset.traceCategory,running=Number(group.dataset.running)||0,count=group.querySelectorAll(":scope > .trace-subgroup-body > .work-tool").length;
    const labels={browser:running?"Browsing":"Browsed",search:running?"Searching the web":"Searched the web",connected:running?"Working in connected apps":"Used connected apps",tools:running?"Running tools":`Ran ${count} tool${count===1?"":"s"}`};
    const label=category==="coding"?(running?"Running tools":codingTraceLabel(group)):labels[category];
    group.classList.toggle("running",running>0);
    const copy=group.querySelector(".trace-subgroup-label");if(copy)copy.textContent=label;
    // The count is what makes a group read as "4 tool calls" rather than an
    // anonymous row, so it stays visible for every group that holds work.
    const badge=group.querySelector(".trace-subgroup-count");if(badge){badge.textContent=String(count);badge.hidden=count<1;}
    // Streaming updates must not touch disclosure state. Flipping `open` for
    // every start/result made the whole trace flash expanded for one frame and
    // then collapse again whenever a new row arrived.
    syncActivityCursor(group.closest(".work-cluster"),running>0?group:null);
  }
  function ensureTraceSubgroup(cluster,tool) {
    const category=traceCategory(tool),list=cluster.querySelector(".work-tools");
    // A trace group is a contiguous run in the timeline, not a global bucket.
    // Reusing the first Browser group pulled later browser calls upward across
    // intervening reasoning, making the displayed order contradict the run.
    const last=list.lastElementChild;
    let group=last?.classList.contains("trace-subgroup")&&last.dataset.traceCategory===category?last:null;
    if(group)return group;
    group=document.createElement("details");group.className=`trace-subgroup trace-${category}`;group.dataset.traceCategory=category;group.dataset.running="0";group.open=detailedConversationView();
    group.innerHTML=`<summary><span class="trace-subgroup-icon" aria-hidden="true"><span class="trace-subgroup-rest">${traceGroupIcon(category)}</span><span class="trace-subgroup-cursor">${loaderMarkup("dot-matrix",14)}</span></span><strong class="trace-subgroup-label">${category==="coding"?"Ran tools":"Working"}</strong><small class="trace-subgroup-count" hidden>0</small><svg class="trace-subgroup-chevron" viewBox="0 0 20 20" aria-hidden="true"><path d="m6 8 4 4 4-4"/></svg></summary><div class="trace-subgroup-body"></div>`;
    list.append(group);updateTraceSubgroup(group);return group;
  }
  // A reaction is the message, not work. Rendering its receipt as a tool row
  // ("Ran 1 tool → react") would be louder than the reply it replaced, so the
  // row is swallowed and only the emoji lands on the prompt it answers.
  function renderReaction(event) {
    if(event.kind!=="tool"||event.ok===false)return true;
    const emoji=String(event.detail||event.target||"").match(/reacted\s+(\S+)/)?.[1];
    if(!emoji)return true;
    const prompt=[...document.querySelectorAll("#conversationFeed .user-message")].at(-1);
    if(!prompt)return true;
    let tray=prompt.querySelector(".message-reactions");
    if(!tray){tray=document.createElement("div");tray.className="message-reactions";prompt.querySelector(".message-content")?.append(tray)||prompt.append(tray);}
    const existing=[...tray.children].find((pill)=>pill.dataset.emoji===emoji);
    if(existing){const count=(Number(existing.dataset.count)||1)+1;existing.dataset.count=String(count);existing.querySelector("small").textContent=count>1?String(count):"";return true;}
    const pill=document.createElement("span");
    pill.className="reaction-pill";pill.dataset.emoji=emoji;pill.dataset.count="1";
    pill.innerHTML=`<b>${escape(emoji)}</b><small></small>`;
    tray.append(pill);
    scrollLatest();
    return true;
  }
  function renderTool(event, replay = false) {
    clearProviderRetry();
    const toolName=String(event.tool||"").toLowerCase();
    // response_validation is an internal retry guard, not an action the user
    // asked Phoenix to perform. Surfacing it as a failed tool fabricated a
    // scary “Adjusted the approach” error after otherwise successful work.
    if(toolName==="response_validation")return;
    if(toolName==="user_update"){
      if(event.kind==="tool_start"||event.ok===false)return;
      return renderAgentUpdate(event.agent,event.detail||"",false,true);
    }
    if(toolName==="react")return void renderReaction(event);
    // Only a live action moves the agent cursor. A repaint or a journal replay
    // re-renders old browser steps; animating those moved the cursor (and the
    // native page's cursor overlay) while the agent was idle.
    if(!replay&&!state.painting&&(traceCategory(toolName)==="browser"||/^computer_(?:click|act|app_|window_)/.test(toolName)))animateBrowserCursor(event);
    const agent = canonicalAgentId(event.agent || state.item?.id || "phoenix");
    markHandoffsWorking(agent);
    const cluster = ensureWorkCluster(agent);
    settleReasoningSubgroups(cluster);
    const key = `${agent}:${event.tool}:${event.target}`;
    const label = humanTool(event.tool,event.kind==="tool_start",event.target);
    const list = cluster.querySelector(".work-tools");
    if (event.kind === "tool_start") {
      const group=ensureTraceSubgroup(cluster,event.tool),groupBody=group.querySelector(".trace-subgroup-body");
      cluster.classList.add("live", "has-tools");
      list.hidden = false;
      cluster.dataset.live = String((Number(cluster.dataset.live) || 0) + 1);
      // The row lands the moment the call is MADE and shimmers where it sits,
      // so the stream reads in the order the work actually happened.
      const row = groupBody.appendChild(paintToolRow(document.createElement("div"), event, label, true));
      group.dataset.running=String((Number(group.dataset.running)||0)+1);updateTraceSubgroup(group);
      syncActivityCursor(cluster,group);
      state.activeTools.set(key, { row, cluster, group, agent, tool:event.tool });
      syncWorkShimmer();
      updateTaskHeadline(`${label}${event.target ? ` · ${humanTarget(event.target)}` : ""}`, true);
      scrollLatest();
      return;
    }
    let live = state.activeTools.get(key);
    if(!live){
      const wantedAgent=canonicalAgentId(agent),wantedTool=String(event.tool||"").toLowerCase();
      const fallback=[...state.activeTools.entries()].reverse().find(([,entry])=>canonicalAgentId(entry.agent)===wantedAgent&&String(entry.tool||"").toLowerCase()===wantedTool);
      if(fallback){state.activeTools.delete(fallback[0]);live=fallback[1];}
    }
    if (live) {
      state.activeTools.delete(key);
      const remaining = Math.max(0, (Number(live.cluster.dataset.live) || 1) - 1);
      live.cluster.dataset.live = String(remaining);
      if (!remaining && !state.working) live.cluster.classList.remove("live");
      if(live.group?.isConnected){live.group.dataset.running=String(Math.max(0,(Number(live.group.dataset.running)||1)-1));updateTraceSubgroup(live.group);}
    }
    const group=live?.group||ensureTraceSubgroup(cluster,event.tool),groupBody=group.querySelector(".trace-subgroup-body");
    state.toolRows.push(event);
    list.hidden = false;
    // Settle the row its start event already placed. Repainted history and any
    // result that arrives without a start still append their own row.
    if (live?.row?.isConnected) paintToolRow(live.row, event, label);
    else appendCompletedToolRow(groupBody,event,label);
    updateTraceSubgroup(live?.group||group);
    cluster.classList.add("has-tools");
    if(!state.painting&&state.working&&Number(cluster.dataset.live)===0)ensureLiveThinkingPlaceholder(cluster);
    else syncActivityCursor(cluster);
    if (!cluster.classList.contains("live") && !state.activeTools.size) {
      document.querySelectorAll(".commentary-line.live").forEach((line) => line.classList.remove("live"));
    }
    syncWorkShimmer();
    syncTeamPresence();
  }
  function compactionIconMarkup(){return '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M4 4.5h8.5L16 8v7.5H4Z"/><path d="M12.5 4.5V8H16M7 11h6M8.5 14h3"/></svg>';}
  function renderContextCompaction(event,replay=false){
    // Housekeeping after the task already answered is not worth a new block.
    const answeredTurn=String(event.turn_id||state.renderingTurnId||state.activeTurnId||"");
    if(answeredTurn&&[...$("conversationFeed").querySelectorAll(":scope > .agent-message")].some((node)=>node.dataset.turnId===answeredTurn))return null;
    const agent=canonicalAgentId(event.agent||state.item?.id||"phoenix"),cluster=ensureWorkCluster(agent),list=cluster.querySelector(".work-tools"),group=state.item?.kind==="group",host=list,status=String(event.status||"completed").toLowerCase();
    const turnId=String(event.turn_id||state.renderingTurnId||state.activeTurnId||"");
    let row=[...host.querySelectorAll(`:scope > .context-compaction[data-agent-key="${CSS.escape(agent)}"], :scope > .work-trace .context-compaction[data-agent-key="${CSS.escape(agent)}"]`)].reverse().find((candidate)=>candidate.dataset.status==="started"&&candidate.dataset.turnId===turnId);
    if(!row){row=document.createElement("div");row.className="context-compaction";row.dataset.agentKey=agent;row.dataset.turnId=turnId;row.innerHTML=`<span class="context-compaction-icon"><span class="context-compaction-rest">${compactionIconMarkup()}</span><span class="context-compaction-cursor">${loaderMarkup("dots",16)}</span></span><span><strong></strong><small></small></span>`;host.append(row);}
    // Historical edges are not live-process evidence. In particular, an old
    // start without a completion receipt must not become a successful fold.
    const name=agentLabel(event.agent||agent),historical=replay||state.painting,running=status==="started"&&!historical,failed=status==="failed",unconfirmed=status==="started"&&!running;
    row.classList.toggle("running",running);row.classList.toggle("failed",failed);row.dataset.status=status;
    row.querySelector("strong").textContent=group?`${name} context ${running?"compacting":failed?"compaction stopped":unconfirmed?"preparation started":"compacted"}`:running?"Context compacting":failed?"Context compaction stopped":unconfirmed?"Context preparation started":"Context compacted";
    const before=Number(event.before_tokens)||0,after=Number(event.after_tokens)||0,folded=Number(event.folded_messages)||0,detail=[];
    if(before&&after)detail.push(`${formatTokenCount(before)} → ${formatTokenCount(after)}`);if(folded)detail.push(`${folded} older message${folded===1?"":"s"} folded`);row.querySelector("small").textContent=detail.join(" · ")||(running?"Keeping the useful work while making room":failed?"Compaction did not complete":unconfirmed?"Awaiting a completion update":"Conversation context was folded");
    cluster.classList.add("has-tools");if(running){cluster.classList.add("live");list.hidden=false;syncActivityCursor(cluster,row);}else{row.classList.remove("activity-current");syncActivityCursor(cluster);if(!historical&&status==="completed"&&after&&Number(event.limit)>1)updateContext(after,event.limit,state.sessionId,event.agent);}
    scrollLatest();return row;
  }
  function humanTool(tool, running = false, target = "") {
    const name=String(tool||"").toLowerCase(),rawTarget=String(target||"").toLowerCase();
    if(name==="image_gen"||name==="image_generate")return running?"Creating an image":"Created an image";
    if(name==="image_analyze")return running?"Inspecting an image":"Inspected an image";
    if(name==="design_reference"&&/(?:^|[\"/])taste(?:[\"/]|$)/.test(rawTarget))return running?"Reading Taste direction":"Applied Taste direction";
    if(name==="design_reference")return running?"Reading design direction":"Applied design direction";
    const special = { teach_workflow: "Invited you to teach", routine: "Used a learned workflow", skill:"Used a skill", ask_for_login: "Requested a login", ask_user:"Asked you a question", credential_generate: "Created a secure password", account_manage:"Prepared an account", response_validation:"Adjusted the approach", codebase_search: "Explored codebase", index_codebase: "Indexed codebase", web_search: running?"Searching the web":"Searched the web", web_fetch:running?"Reading a web page":"Read a web page", composio_search:"Searched connected apps", composio_execute:"Used a connected app", browser_navigate: running?"Opening in Chrome":"Opened in Chrome", browser_extract:running?"Reading in Chrome":"Read in Chrome", browser_act:running?"Using Chrome":"Used Chrome", computer_use: "Used computer", computer_act:"Used computer", computer_window_act:"Used computer", computer_click:"Used computer", computer_capture_window:"Looked at the app", computer_screenshot:"Looked at the screen", computer_focus_window:"Switched to the app", computer_list_windows:"Checked open apps", computer_status:"Checked computer access", computer_app_read:"Read the app", computer_app_inspect:"Inspected the app", computer_app_locate:"Found a control", computer_app_targets:"Found controls", talk: "Talked with a coworker", spawn: "Delegated work", work: "Checked company work", cron: "Updated a schedule", image_gen: "Created an image", image_analyze:"Inspected an image", glob: "Found files", read: "Read a file", bash: "Ran commands", write: "Wrote a file", str_replace: "Edited files" };
    return special[tool] || String(tool || "Working").replace(/^mcp__/, "").replaceAll("_", " ").replace(/^./, (c) => c.toUpperCase());
  }
  function activityLabelForTool(tool) {
    const name=String(tool||"").toLowerCase();
    if(name==="web_search")return"Searching the web";
    if(/^(?:browser_|web_fetch|crawl|scrape)/.test(name))return"Browsing";
    if(/^(?:image_gen|image_generate)/.test(name))return"Creating an image";
    if(/^(?:image_analyze|ui_snap)/.test(name))return"Inspecting the image";
    if(/^(?:read|write|str_replace|bash|glob|codebase_search|index_codebase)/.test(name))return"Running tools";
    if(/^(?:composio_|mcp__)/.test(name))return"Working in connected apps";
    return humanTool(tool,true);
  }
  function toolVisual(tool) {
    const name=String(tool||"").toLowerCase();
    if(/teach_workflow|routine|skill|design_reference/.test(name))return{tone:"teach",svg:'<svg viewBox="0 0 20 20"><path d="M10 2.8 11.6 7l4.3 1.6-4.3 1.6-1.6 4.3-1.6-4.3-4.3-1.6L8.4 7 10 2.8Z"/><path d="M15.3 13.1l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7.7-1.8Z"/></svg>'};
    if(/credential|password|login|account|vault|secret/.test(name))return{tone:"secure",svg:'<svg viewBox="0 0 20 20"><path d="M10 2.7 16 5v4.2c0 4.1-2.1 6.7-6 8.1-3.9-1.4-6-4-6-8.1V5l6-2.3Z"/><circle cx="10" cy="9" r="1.7"/><path d="M10 10.7v2.5"/></svg>'};
    if(/cron|schedule|remind|notification/.test(name))return{tone:"time",svg:'<svg viewBox="0 0 20 20"><circle cx="10" cy="10.5" r="6.5"/><path d="M10 6.5v4.2l2.8 1.7M6 2.8 3.6 5.2M14 2.8l2.4 2.4"/></svg>'};
    if(/image|design|ui_snap|screenshot|vision/.test(name))return{tone:"visual",svg:'<svg viewBox="0 0 20 20"><rect x="3" y="4" width="14" height="12" rx="2"/><circle cx="8" cy="8" r="1.5"/><path d="m5 14 3.6-3.5 2.5 2.2 1.8-1.7 2.1 3"/></svg>'};
    if(/computer_|computer_use/.test(name))return{tone:"desktop",svg:'<svg viewBox="0 0 20 20"><rect x="2.8" y="3.5" width="14.4" height="10.5" rx="2"/><path d="M7 17h6M10 14v3M5.5 7.2h4M5.5 10h2.5"/></svg>'};
    if(/browser|web_fetch|crawl|scrape/.test(name))return{tone:"browse",image:CHROME_ICON,svg:""};
    if(name==="web_search")return{tone:"browse",svg:""};
    if(/search|glob|index|codebase/.test(name))return{tone:"code",svg:'<svg viewBox="0 0 20 20"><path d="m7 5-4 5 4 5M11 5l4 5-4 5"/><circle cx="15" cy="15" r="2.3"/></svg>'};
    if(/write|edit|replace|patch|apply/.test(name))return{tone:"write",svg:'<svg viewBox="0 0 20 20"><path d="m4 14-.7 3 3-.7L15 7.6 12.4 5 4 14Z"/><path d="m11.8 5.6 2.6 2.6"/></svg>'};
    if(/read|file/.test(name))return{tone:"read",svg:'<svg viewBox="0 0 20 20"><path d="M5 3h7l3 3v11H5Z"/><path d="M12 3v4h4M8 10h4M8 13h4"/></svg>'};
    if(/bash|shell|terminal|command/.test(name))return{tone:"shell",svg:'<svg viewBox="0 0 20 20"><rect x="3" y="4" width="14" height="12" rx="2"/><path d="m6 8 2 2-2 2M10 13h4"/></svg>'};
    if(/talk|spawn|delegate|agent/.test(name))return{tone:"team",svg:'<svg viewBox="0 0 20 20"><circle cx="7" cy="7" r="2.5"/><circle cx="14" cy="8" r="2"/><path d="M2.8 16c.5-3 1.9-4.5 4.2-4.5s3.7 1.5 4.2 4.5M11.5 12c2.7-.4 4.5.9 5 3.5"/></svg>'};
    if(/memory|recall|knowledge/.test(name))return{tone:"memory",svg:'<svg viewBox="0 0 20 20"><path d="M7 4a3 3 0 0 0-3 3c0 .7.2 1.3.6 1.8A3.5 3.5 0 0 0 7 15h1V4H7ZM13 4a3 3 0 0 1 3 3c0 .7-.2 1.3-.6 1.8A3.5 3.5 0 0 1 13 15h-1V4h1Z"/><path d="M8 7H6M12 9h2M8 12H6"/></svg>'};
    return{tone:"default",svg:'<span class="tool-dot" aria-hidden="true"></span>'};
  }
  function humanTarget(target,tool="") {
    const raw=String(target||"").trim();if(!raw)return"";
    const toolName=String(tool||"").toLowerCase();
    if(/^(?:ask_user|ask_for_login|credential_generate|account_manage|response_validation|image_gen|image_generate|image_analyze|design_reference)$/.test(toolName))return"";
    try{const value=JSON.parse(raw);if(value&&typeof value==="object"&&!Array.isArray(value)){
      if(/composio.*search|web_search/.test(toolName))return"";
      const hint=value.pattern||value.query||value.subject||value.path||value.url||value.target||value.app;
      if(hint!=null&&typeof hint!=="object")return String(hint).slice(0,120);
      return"";
    }}catch{}
    // Internal payloads and identifiers are diagnostics, not conversation UI.
    if(/^[\[{]/.test(raw)||/["'](?:queries|session_id|profile_id|credential_id|tool_id)["']\s*:/.test(raw))return"";
    const delegated=raw.match(/^spawn\s*(?:→|->)\s*([a-z0-9_-]+)\s*:\s*(.*)$/i);if(delegated)return`${agentLabel(delegated[1])} · ${delegated[2]}`;
    const runtime=raw.match(/^(computer_use|browser|librarian|indexer|vision|image)(?:\s*:\s*(.*))?$/i);if(runtime)return`${agentLabel(runtime[1])}${runtime[2]?` · ${runtime[2]}`:""}`;
    return raw.length>120?`${raw.slice(0,117)}…`:raw;
  }
  function humanLegacyText(value) {
    return String(value || "").trim()
      .replace(/^\[background return\]\s*/i, "")
      .replace(/\s*(?:—|-)\s*background return\s*$/i, "")
      .replace(/\bbackground specialist\b/gi, "coworker")
      .replace(/\bspecialist\b/gi, "coworker");
  }
  function renderReceipt(event) {
    const cluster = ensureWorkCluster(event.agent || "phoenix");
    if (!state.working) cluster.classList.remove("live");
  }
  function renderCard(event, kind) {
    const ok = event.ok !== false;
    const from = event.from || event.agent || "Phoenix";
    if(!ok){const raw=String(event.diagnostics||event.body||event.text||event.subject||"This action did not finish.");const summary=runtimeFailureSummary(raw)||humanFailureDetail(event.body||event.text)||safeRuntimeCopy(event.subject)||"This action did not finish.";const node=feedNode("message-row agent-message runtime-error",`${avatar(agentProfile(from))}<div class="message-content"><header><strong>${escape(agentLabel(from))}</strong></header><details class="runtime-error-details"><summary><span>${escape(summary)}</span><span class="runtime-error-help" aria-hidden="true">?</span><span class="sr-only"> Error details</span></summary><pre></pre></details></div>`,{agentId:from,from:"assistant",slot:"message"});node.querySelector("pre").textContent=raw;return node;}
    const subject=safeRuntimeCopy(event.subject || "Update") || "Update",body=safeRuntimeCopy(event.body || event.text || "") || "Phoenix could not provide more detail.";
    feedNode(`result-card ${ok ? "ok" : "failed"}`, `<header><span>${ok ? icons.check : icons.warn}</span><strong>${escape(agentLabel(from))}</strong><small>${escape(kind)}</small></header><h3>${escape(subject)}</h3><div class="markdown">${markdown(body)}</div>`);
  }
  function agentChipMarkup(agent, extraClass="") {
    const canonical=canonicalAgentId(agent)||agent,profile=agentProfile(canonical),label=agentLabel(canonical),color=profile?.color||"var(--ember)";
    return `<button type="button" class="agent-metal-chip ${extraClass}" data-open-agent="${escape(profile?.agent_id||canonical||"")}" style="--agent-chip:${escape(color)}" title="Open ${escape(label)}"><span class="chip-shine" aria-hidden="true"></span><span class="agent-chip-avatar">${ui.avatarSvg(profile)}</span><strong>${escape(label)}</strong></button>`;
  }
  function delegatedAgent(event) {
    const from=String(event.from||event.agent||""),to=String(event.to||"");
    const hub=(value)=>/^(?:phoenix|orchestrator)$/i.test(value);
    if(to&&!hub(to))return to;
    if(from&&!hub(from))return from;
    return to||from||"phoenix";
  }
  function handoffExplicitId(event) {
    const value=event?.handoff_id??event?.delegation_id??event?.event_id??event?.work_id??event?.job_id??event?.id;
    return value===undefined||value===null||String(value).trim()===""?"":`event:${answerKey(String(value)).slice(7)}`;
  }
  function returnExplicitId(event) {
    const value=event?.reply_to??event?.handoff_id??event?.delegation_id??event?.event_id??event?.work_id??event?.job_id??event?.id??event?.causation_id;
    return value===undefined||value===null||String(value).trim()===""?"":`event:${answerKey(String(value)).slice(7)}`;
  }
  function handoffSubject(value) {
    const visible=humanLegacyText(safeRuntimeCopy(value)).replace(/\s+/g," ").trim();
    return visible.length>104?`${visible.slice(0,101).trimEnd()}…`:visible;
  }
  function handoffSubjectKey(value) { return normalizedAgentTalkSubject(humanLegacyText(value)).replace(/\s+/g," ").trim().toLowerCase(); }
  function handoffIdentity(event) {
    const from=canonicalAgentId(event.requester||event.from||state.item?.id||"phoenix")||"phoenix";
    const to=canonicalAgentId(event.receiver||event.to||delegatedAgent(event))||delegatedAgent(event);
    const subjectKey=handoffSubjectKey(event.subject||event.task||event.text);
    const explicit=handoffExplicitId(event);
    return{from,to,subjectKey,id:explicit||`semantic:${answerKey(`${from}|${to}|${subjectKey}`).slice(7)}`};
  }
  function handoffState(value) {
    const stateValue=String(value||"").trim().toLowerCase();
    if(stateValue==="inactive")return"inactive";
    if(stateValue==="returned")return"returned";
    if(/cancelled|canceled/.test(stateValue))return"canceled";
    if(/blocked|failed|error|needs.?attention/.test(stateValue))return"blocked";
    if(/done|complete|completed|returned|finished|success/.test(stateValue))return"done";
    if(/working|running|active|started|executing/.test(stateValue))return"working";
    return"queued";
  }
  function handoffStateLabel(value) { return({queued:"Queued",working:"Working",done:"Done",returned:"Returned work",blocked:"Blocked",canceled:"Canceled",inactive:"No return recorded"})[value]||"Queued"; }
  function handoffIsSettled(row) { return["done","returned","blocked","canceled"].includes(row?.dataset.handoffState); }
  function handoffRows() { return[...$("conversationFeed").querySelectorAll(".handoff-chain[data-handoff-id]")]; }
  function handoffProgressState(row){
    if(handoffIsSettled(row))return row.dataset.handoffState;
    if(row?.dataset.handoffState==='inactive')return'inactive';
    const block=row.closest('.team-work-block');
    const member=[...block?.querySelectorAll('[data-group-status-agent]')||[]].find(node=>node.dataset.groupStatusAgent===row.dataset.handoffTo);
    const status=member?.dataset.state;
    if(['waiting_user','blocked','failed','cancelled','stopped','continued'].includes(status))return status;
    if(block?.dataset.interrupted==='true')return 'stopped';
    // A member finishing is not a receipt for this particular delegation.
    return status==='queued'?'queued':'waiting';
  }
  function syncHandoffLiveLabel(row,activity=ui.activityFor?.(state.item)){
    if(!row||handoffIsSettled(row))return;
    if(state.item?.kind==="agent"){
      // One-to-one chat: the handoff is live while its receiver works.
      const receiver=agentProfile(row.dataset.handoffTo),theirs=receiver&&ui.activityFor?.({kind:"agent",id:receiver.agent_id});
      if(!theirs||!LIVE_CONVERSATION_STATUSES.has(theirs.status))return;
      // An older unfinished handoff to the same coworker was taken over by it.
      const newest=handoffRows().filter(other=>other.dataset.handoffTo===row.dataset.handoffTo).at(-1),label=newest===row?"Working":"Continued below";
      const status=row.querySelector(".handoff-status");if(status)status.textContent=label;
      const summary=row.closest(".group-work-cluster")?.querySelector(".group-work-agent small");if(summary)summary.textContent=`asked ${row.dataset.handoffToLabel} · ${label==="Working"?`${row.dataset.handoffToLabel} working`:label}`;
      row.setAttribute("aria-label",`${row.dataset.handoffFromLabel} handed work to ${row.dataset.handoffToLabel}. ${label}`);
      return;
    }
    if(!activity||state.item?.kind!=="group")return;
    const turn=state.activeTurnId,active=row.dataset.turnId===turn&&LIVE_CONVERSATION_STATUSES.has(activity.status)&&(activity.active_agent_ids||[]).map(canonicalAgentId).includes(row.dataset.handoffTo);
    const work=[...$("conversationFeed").querySelectorAll(".team-work-row")].find(n=>n.dataset.turnId===row.dataset.turnId&&n.dataset.agent===row.dataset.handoffTo);
    const progress=handoffProgressState(row),labels={queued:'Queued',waiting_user:'Waiting for you',blocked:'Blocked',failed:'Failed',cancelled:'Canceled',stopped:'Stopped',continued:'Continued below',inactive:'No return recorded'};
    const label=labels[progress]||(active?(work?.querySelector(".work-tool.running")?"Working":"Active · awaiting update"):"Awaiting result");
    const status=row.querySelector(".handoff-status");if(status)status.textContent=label;
    row.setAttribute("aria-label",`${row.dataset.handoffFromLabel} handed work to ${row.dataset.handoffToLabel}. ${label}`);
  }
  function setHandoffPresentation(row,nextState,summary="") {
    if(!row)return;
    const current=row.dataset.handoffState||"queued",terminal=handoffIsSettled(row);
    // Replayed delegation rows cannot move a completed handoff back to queued.
    const stateValue=terminal&&!['done','returned','blocked','canceled'].includes(nextState)?current:nextState;
    row.dataset.handoffState=stateValue;
    row.classList.toggle("complete",stateValue==="done");row.classList.toggle("failed",stateValue==="blocked");
    const status=row.querySelector(".handoff-status");if(status)status.textContent=handoffStateLabel(stateValue);
    const receiver=row.querySelector(".handoff-receiver .agent-metal-chip");
    receiver?.classList.remove("complete","failed");if(stateValue==="done")receiver?.classList.add("complete");if(stateValue==="blocked")receiver?.classList.add("failed");
    const copy=handoffSubject(summary);const result=row.querySelector(".handoff-result");
    if(result&&copy){result.hidden=false;result.textContent=copy;result.title=humanLegacyText(safeRuntimeCopy(summary));}
    const groupSummary=row.closest(".group-work-cluster")?.querySelector(".group-work-agent small");if(groupSummary){const status=handoffStateLabel(stateValue);groupSummary.textContent=`asked ${row.dataset.handoffToLabel} · ${/^working$/i.test(status)?`${row.dataset.handoffToLabel} working`:status}`;}
    row.setAttribute("aria-label",`${row.dataset.handoffFromLabel} handed work to ${row.dataset.handoffToLabel}. ${handoffStateLabel(stateValue)}${copy?`: ${copy}`:""}`);
    syncHandoffLiveLabel(row);syncActivitySummary();
  }
  function returnMatch(event) {
    const receiver=canonicalAgentId(event.receiver||event.agent||event.from||event.to||"phoenix"),explicit=returnExplicitId(event),subjectKey=handoffSubjectKey(event.subject);
    const candidates=handoffRows().filter((row)=>row.dataset.handoffTo===receiver);
    // A durable correlation id is authoritative. If its handoff is outside
    // the retained window, keep the result pending instead of settling some
    // newer job merely because it has the same coworker or a similar title.
    if(explicit)return[...candidates].reverse().find((row)=>row.dataset.handoffId===explicit)||null;
    if(subjectKey){const exact=candidates.filter((row)=>row.dataset.handoffSubject===subjectKey);if(exact.length)return[...exact].reverse().find((row)=>!handoffIsSettled(row))||exact.at(-1);}
    // A body-only/renamed return may still settle the newest open delegation
    // to this coworker. It must never repaint a historical settled row.
    return[...candidates].reverse().find((row)=>!handoffIsSettled(row))||null;
  }
  function applyReturnToHandoff(row,event) {
    const ok=returnSucceeded(event),canceled=handoffState(event.status)==='canceled',raw=event.body||event.text||event.result||event.subject||"",summary=ok?"":canceled?"This work was canceled.":internalReturnFailure(raw);
    const returned=ok&&event.historical===true&&event.status==="returned"&&event.ok==null;
    setHandoffPresentation(row,returned?"returned":ok?"done":canceled?"canceled":"blocked",summary||event.subject);
    const result=row.querySelector('.handoff-result');if(ok&&result){result.hidden=true;result.textContent="";result.removeAttribute('title');}
    let details=row.querySelector('.handoff-error-details');
    if(ok){details?.remove();}else{
      if(!details){details=document.createElement('details');details.className='runtime-error-details handoff-error-details';details.innerHTML='<summary aria-label="Error details"><span class="runtime-error-help" aria-hidden="true">?</span></summary><pre></pre>';row.querySelector('.handoff-copy').append(details);}
      details.querySelector('pre').textContent=String(raw);
    }
    const receiver=row.querySelector(".handoff-receiver .agent-metal-chip");
    if(receiver){receiver.title=handoffSubject(summary||event.subject);receiver.setAttribute("aria-label",`${row.dataset.handoffToLabel} ${returned?"returned work":ok?"finished":canceled?"was canceled":"is blocked"}`);}
    syncOwnerHandoffUpdate(row.dataset.turnId);
  }
  function returnSucceeded(event) {
    return event.ok!==false&&!['blocked','failed','error','canceled','cancelled'].includes(String(event.status||'').toLowerCase())
      &&!runtimeFailureSummary(event.body||event.text||event.result||'')
      &&! /\bturn (?:failed|hit an internal error)\b/i.test(event.subject||'');
  }
  function pendingReturnFor(row) {
    const lastIndex=(predicate)=>{for(let index=state.pendingHandoffReturns.length-1;index>=0;index-=1){if(predicate(state.pendingHandoffReturns[index]))return index;}return-1;};
    const explicitIndex=lastIndex((event)=>returnExplicitId(event)&&returnExplicitId(event)===row.dataset.handoffId);
    const subjectIndex=lastIndex((event)=>!returnExplicitId(event)&&canonicalAgentId(event.agent||event.from)===row.dataset.handoffTo&&handoffSubjectKey(event.subject)&&handoffSubjectKey(event.subject)===row.dataset.handoffSubject);
    const receiverIndex=lastIndex((event)=>!returnExplicitId(event)&&canonicalAgentId(event.agent||event.from)===row.dataset.handoffTo);
    const index=explicitIndex>=0?explicitIndex:subjectIndex>=0?subjectIndex:receiverIndex;
    return index<0?null:state.pendingHandoffReturns.splice(index,1)[0];
  }
  function placeHandoffBeforeAnswer(row) {
    const cluster=row.closest(".team-work-block")||row.closest(".work-cluster"),feed=$("conversationFeed");if(!cluster)return;
    const turnId=cluster.dataset.turnId||"",answers=[...feed.querySelectorAll(":scope > .agent-message,:scope > .group-message:not(.handoff-checkpoint)")];
    const answer=(turnId?answers.filter((node)=>node.dataset.turnId===turnId).at(-1):state.pendingAnswer?.node)||null;
    if(answer&&(answer.compareDocumentPosition(cluster)&Node.DOCUMENT_POSITION_FOLLOWING))feed.insertBefore(cluster,answer);
  }
  function markHandoffsWorking(agent) {
    const receiver=canonicalAgentId(agent);if(!receiver)return;
    handoffRows().filter((row)=>row.dataset.handoffTo===receiver&&!handoffIsSettled(row)).forEach((row)=>setHandoffPresentation(row,"working"));
  }
  // Running tokens the agent in this conversation has spent on the current
  // turn, shown in the composer beside the voice button.
  // How full this conversation's context window is, in tokens, against the
  // window chosen on the Context slider.
  // Until this window gets a live sample, show the gateway's last saved
  // reading for the conversation instead of an empty gauge.
  async function restoreSavedContextUsage(sessionId){
    if(preview||!sessionId||state.usageBySession.has(sessionId))return;
    try{const saved=await ui.invoke("context_usage_get",{sessionId});
      if(saved&&Number(saved.used)>0&&!state.usageBySession.has(sessionId))updateContext(saved.used,saved.limit,sessionId);}catch{}
  }
  function paintContextTokens(){
    const chip=$("composerTokens");if(!chip)return;
    const window=Number(selectedModelContext().effective)||Number(state.usage?.limit)||0,used=Number(state.usage?.limit)>1?Number(state.usage?.used)||0:0;
    chip.hidden=!window;chip.textContent=`${formatTokenCount(used)} / ${formatContextWindow(window)}`;
    chip.title=`${used.toLocaleString()} of ${window.toLocaleString()} tokens in context`;
  }
  function handoffTraceHost(event) {
    const cluster=ensureWorkCluster(event.agent||event.from||state.item?.id||"phoenix");
    settleReasoningSubgroups(cluster);
    const list=cluster.querySelector(".work-tools");list.hidden=false;return{cluster,list};
  }
  function renderReturn(event, allowStandalone=false) {
    const ok=returnSucceeded(event),from=canonicalAgentId(event.agent||event.from||"phoenix");
    const turn=event.turn_id||state.renderingTurnId||state.activeTurnId;
    document.querySelectorAll('.team-work-row').forEach(cluster=>{if(cluster.dataset.agent===from&&cluster.dataset.turnId===turn)cluster.dataset.teamState=ok?'done':handoffState(event.status)==='canceled'?'cancelled':'blocked';});
    settleAgentWork(from,!ok);
    const row=returnMatch(event);
    if(row){applyReturnToHandoff(row,event);return row;}
    // Keep unmatched receipts until the handoff arrives. Their full text is
    // retained by the collapsed return disclosure, never as a final reply.
    const pendingKey=`${returnExplicitId(event)}|${from}|${handoffSubjectKey(event.subject)}|${event.ok!==false}`;
    if(!state.pendingHandoffReturns.some((entry)=>`${returnExplicitId(entry)}|${canonicalAgentId(entry.agent||entry.from)}|${handoffSubjectKey(entry.subject)}|${entry.ok!==false}`===pendingKey))state.pendingHandoffReturns.push({...event});
    if(allowStandalone)renderCard({from,subject:event.subject||"Needs attention",body:internalReturnFailure(event.body||event.text),ok:false},"blocked");
    return null;
  }
  function renderFailure(event) {
    const agent=event.agent||"phoenix";
    const detail=internalReturnFailure(event.text);
    renderReturn({agent,ok:false,subject:"Needs attention",body:detail},true);
  }
  function renderHandoff(event) {
    // volume_work's anonymous readers are one internal batch operation, not
    // visible coworkers with independent durable handoff lifecycles.
    if(isEphemeralVolumeHandoff(event))return null;
    // Older gateways published failed peer replies as another queued task.
    // Their causation id points to the assignment being answered.
    if(event.background===true&&event.causation_id&&/^[\w-]+ turn (?:failed|hit an internal error)$/i.test(event.subject||'')){
      return renderReturn({...event,agent:event.from,receiver:event.from,requester:event.to,reply_to:event.causation_id,ok:false,status:'blocked',body:event.body||event.subject});
    }
    const identity=handoffIdentity(event),subject=handoffSubject(event.subject||event.task||event.text)||"Delegated work";
    if(!involvesConversationAgent(identity.from,identity.to))return null;
    let chain=handoffRows().find((row)=>row.dataset.handoffId===identity.id);
    // In a direct conversation the whole turn is one block: handoffs sit
    // inside the owner's work trace instead of closing it, which used to
    // split one turn into a new "Phoenix" block after every delegation.
    const inlineHandoff=state.item?.kind!=="group";
    if(!chain){
      if(inlineHandoff){chain=document.createElement("article");chain.className="handoff-chain handoff-inline";const turnId=state.renderingTurnId||state.activeTurnId||"";if(turnId)chain.dataset.turnId=turnId;}
      else{settleAgentWork(identity.from);state.turnStatus=null;state.replayWorkCluster=null;chain=feedNode("handoff-chain", "");}
      if(event.background===true)chain.dataset.background="true";
      const fromLabel=agentLabel(identity.from),toLabel=agentLabel(identity.to);
      Object.assign(chain.dataset,{handoffId:identity.id,handoffFrom:identity.from,handoffTo:identity.to,handoffSubject:identity.subjectKey,handoffFromLabel:fromLabel,handoffToLabel:toLabel});
      chain.innerHTML=`<span class="handoff-endpoint handoff-requester">${agentChipMarkup(identity.from)}</span><span class="handoff-arrow" aria-hidden="true">asked</span><span class="handoff-endpoint handoff-receiver" data-agent-key="${escape(identity.to)}">${agentChipMarkup(identity.to)}</span><span class="handoff-copy"><span class="handoff-task" title="${escape(humanLegacyText(safeRuntimeCopy(event.subject||event.task||event.text)))}">${escape(subject)}</span><span class="handoff-meta"><strong class="handoff-status"></strong><span class="handoff-result" hidden></span></span></span>`;
      if(inlineHandoff)handoffTraceHost({agent:identity.from}).list.append(chain);
      else placeInTeamWorkBlock(chain);
    }
    setHandoffPresentation(chain,handoffState(event.status||event.state));
    const pending=pendingReturnFor(chain);if(pending)applyReturnToHandoff(chain,pending);
    placeHandoffBeforeAnswer(chain);
    syncOwnerHandoffUpdate(chain.dataset.turnId);

    if(!state.painting)scrollLatest();
    return chain;
  }
  // A coworker's own chat shows the work it asked for or received. A
  // handoff between two other coworkers (Leon asking Robin inside a job Tibo
  // gave Leon) belongs to their chats, not this one. Group chats show all.
  function involvesConversationAgent(from,to){
    if(state.item?.kind!=="agent")return true;
    const owner=canonicalAgentId(state.item.id||"");
    return !owner||canonicalAgentId(from||"")===owner||canonicalAgentId(to||"")===owner;
  }
  function renderAgentContextMessage(event) {
    const meta=agentContextMessageMeta(event)||{priority:"normal"},from=canonicalAgentId(event.from||"phoenix")||"phoenix",to=canonicalAgentId(event.to||"agent")||event.to||"agent",subject=handoffSubject(event.subject)||"Context message",body=normalizedAgentTalkBody(event.text||event.body||""),id=handoffExplicitId(event)||`context:${answerKey(`${from}|${to}|${subject}|${body}`).slice(7)}`;
    if(!involvesConversationAgent(from,to))return null;
    // In the receiver's one-to-one chat a coworker's message is a message on
    // the right, "from Tibo". The sender's chat keeps it inside its work.
    if(state.item?.kind==="agent"){
      const owner=canonicalAgentId(state.item.id),text=normalizedAgentTalkBody(String(event.text||event.body||"").replace(/^\s*<!--[\s\S]*?-->\s*/,""))||subject,key=answerKey(`context|${from}|${to}|${text}`);
      if(to===owner&&from!==owner)return renderAgentRequest(event,from,text,key);
    }
    const {list}=handoffTraceHost({agent:event.agent||event.from||state.item?.id||"phoenix"});
    let row=list.querySelector(`.agent-context-message[data-context-message-id="${CSS.escape(id)}"]`);
    if(!row){
      row=document.createElement("div");row.className="agent-context-message";row.dataset.contextMessageId=id;
      row.innerHTML=`<span class="handoff-endpoint">${agentChipMarkup(from)}</span><span class="context-message-arrow" aria-hidden="true">messaged</span><span class="handoff-endpoint">${agentChipMarkup(to)}</span><span class="context-message-copy"><strong title="${escape(subject)}">${escape(subject)}</strong><small>Message${meta.priority&&meta.priority!=="normal"?` · ${escape(meta.priority)}`:""}</small></span>`;
      row.title=body||subject;row.setAttribute("aria-label",`${agentLabel(from)} sent ${agentLabel(to)} a ${meta.priority} priority context message: ${subject}`);list.append(row);
    }
    if(!state.painting)scrollLatest();
    return row;
  }
  function syncOwnerHandoffUpdate(turn){
    if(state.item?.kind!=="group"||!turn)return;
    const block=[...$("conversationFeed").querySelectorAll('.team-work-block')].find(node=>node.dataset.turnId===turn);syncTeamWorkBlock(block);
    const feed=$("conversationFeed"),rows=handoffRows().filter(n=>n.dataset.turnId===turn);
    if(!rows.length)return;
    const owner=initiatingAgentForTurn({turn_id:turn}),assigned=rows.filter(n=>n.dataset.handoffFrom===owner);
    if(!assigned.length)return;
    let node=[...feed.querySelectorAll('.handoff-checkpoint')].find(n=>n.dataset.turnId===turn);
    const final=[...feed.querySelectorAll('.group-message:not(.handoff-checkpoint)')].some(n=>n.dataset.turnId===turn&&n.dataset.agentId===owner);
    if(final){node?.remove();return;}
    const names=values=>{const unique=[...new Set(values)];return unique.length<2?unique[0]||"":`${unique.slice(0,-1).join(", ")} and ${unique.at(-1)}`;};
    const matching=statuses=>assigned.filter(row=>statuses.includes(handoffProgressState(row))).map(row=>row.dataset.handoffToLabel);
    const pending=matching(['waiting','queued']),blocked=matching(['blocked','failed']),waiting=matching(['waiting_user']),stopped=matching(['stopped','canceled','cancelled']),continued=matching(['continued']),inactive=matching(['inactive']);
    const updates=[waiting.length?`${names(waiting)} ${waiting.length===1?'needs':'need'} your input.`:'',blocked.length?`${names(blocked)} reported a blocker.`:'',stopped.length?`${names(stopped)} stopped before returning.`:'',continued.length?`${names(continued)} continued below.`:'',pending.length?`Waiting for ${names(pending)} to return.`:'',inactive.length?`No return is recorded for ${names(inactive)}.`:''].filter(Boolean);
    const copy=`I’ve handed work to ${names(assigned.map(n=>n.dataset.handoffToLabel))}. ${updates.join(' ')||"Their results are in."}`;
    if(!node){node=feedNode("message-row group-message handoff-checkpoint",`${avatar(agentProfile(owner))}<div class="message-content"><header><strong>${escape(agentLabel(owner))}</strong><small>Handoff update</small></header><div class="markdown"></div></div>`,{agentId:owner,slot:"message",from:"assistant",handoffCheckpoint:turn});node.dataset.turnId=turn;}
    const message=node.querySelector('.markdown');message.setAttribute('role','status');message.setAttribute('aria-atomic','true');
    if(message.textContent!==copy)message.textContent=copy;
    const roster=block||[...feed.querySelectorAll('.team-work-row')].find(n=>n.dataset.turnId===turn);if(roster)roster.before(node);
  }
  function renderGroupMessage(event) {
    const speaker=canonicalAgentId(event.agent_id||event.agent_name||event.agent);
    if(runtimeFailureSummary(event.markdown))return renderRuntimeFailure(event.markdown,speaker);
    if(speaker!==initiatingAgentForTurn(event)){
      renderReturn({...event,agent:speaker,body:event.markdown});
      return renderIncomingAgentTalk({...event,from:speaker,body:event.markdown});
    }
    if(event.reply_to||event.causation_id)renderReturn({agent:event.agent_id||event.agent_name,receiver:event.agent_id||event.agent_name,reply_to:event.reply_to,causation_id:event.causation_id,subject:event.subject||"Group reply",body:event.markdown,ok:true});
    const snapshot=event.agent_snapshot&&typeof event.agent_snapshot==="object"?event.agent_snapshot:null;
    const live=agentProfile(event.agent_id),p=snapshot?{...live,...snapshot,agent_id:event.agent_id||snapshot.agent_id,metadata_json:JSON.stringify({avatar:snapshot.avatar||{}})}:live;
    const color=p?.color||"var(--ember)";
    const text=visibleAnswerText(event.markdown,event.agent_id),node=feedNode("message-row group-message", `${avatar(p)}<div class="message-content" data-slot="message-content"><header><strong>${escape(event.agent_name || p?.display_name)}</strong></header><div class="markdown">${relayedImagesMarkup(text)}${markdown(text)}</div></div>`,{agentId:event.agent_id||"",speaker,messageId:event.message_id||"",slot:"message",from:"assistant"});
    PhoenixConversationUpdates.finalize(node);
    node.style.setProperty("--speaker",color);syncOwnerHandoffUpdate(node.dataset.turnId);hydrateRelayedImages(node);
  }
  function renderGroupMemberStatus(event) {
    const turnId=String(event.turn_id||state.activeTurnId||"group-turn"),feed=$("conversationFeed");
    let strip=feed.querySelector(`.group-execution-strip[data-turn-id="${CSS.escape(turnId)}"]`);
    if(!strip&&event.turn_id&&turnId!==state.activeTurnId&&turnId!==state.renderingTurnId&&!feed.querySelector(`[data-turn-id="${CSS.escape(turnId)}"]`))return;
    if(!strip){strip=feedNode("group-execution-strip",'<header><strong>Team activity</strong><small aria-live="polite"></small><button type="button" class="group-status-toggle" aria-label="Show team details" aria-expanded="false" hidden>⌄</button></header><div class="group-execution-members"></div>',{turnId});strip.dataset.turnId=turnId;const toggle=strip.querySelector(".group-status-toggle");toggle.onclick=()=>{const expanded=strip.classList.toggle("expanded");toggle.setAttribute("aria-expanded",String(expanded));toggle.setAttribute("aria-label",expanded?"Hide team details":"Show team details");};}
    const agentId=canonicalAgentId(event.agent_id||event.agent_name)||String(event.agent_id||event.agent_name||"agent"),profile=agentProfile(agentId),host=strip.querySelector(".group-execution-members");
    let member=host.querySelector(`[data-group-status-agent="${CSS.escape(agentId)}"]`);
    if(!member){member=document.createElement("div");member.className="group-execution-member";member.dataset.groupStatusAgent=agentId;member.innerHTML=`<span class="mini-avatar">${ui.avatarSvg(profile)}</span><span><strong>${escape(event.agent_name||agentLabel(agentId))}</strong><small></small></span><b></b>`;host.append(member);}
    updateGroupMemberStatus(member,event.state,event.detail);
    placeInTeamWorkBlock(strip);
    const work=[...feed.querySelectorAll(".team-work-row")].find(row=>row.dataset.turnId===turnId&&canonicalAgentId(row.dataset.agent)===agentId);
    if(work){work.dataset.teamState=String(event.state||"");if(["done","failed","cancelled","blocked","waiting_user"].includes(event.state))settleWorkCluster(work,event.state==="cancelled");syncTeamWorkSummary(work);}

    updateGroupStatusSummary(strip);
    syncTeamWorkBlock(strip.closest('.team-work-block'));
    for(const row of handoffRows().filter(row=>row.dataset.turnId===turnId))syncHandoffLiveLabel(row);
    syncOwnerHandoffUpdate(turnId);
    if(!state.painting)reconcileGroupStatusContinuations();
  }
  function updateGroupMemberStatus(member,status,detail){
    const value=String(status||"queued"),labels={queued:"Queued",working:"Working",waiting_user:"Waiting for you",blocked:"Blocked",done:"Replied",continued:"Continued below",failed:"Failed",cancelled:"Cancelled",stopped:"Stopped",inactive:"Unconfirmed"};
    const rawDetail=String(detail||""),friendlyDetails={"Execution lane acquired":"","Started working":"","Ready for execution":"","Contribution saved":""};
    member.dataset.state=value;member.querySelector("small").textContent=friendlyDetails[rawDetail]??rawDetail;member.querySelector("b").textContent=labels[value]||value;
  }
  function updateGroupStatusSummary(strip){
    const host=strip.querySelector(".group-execution-members");
    const members=[...host.children],working=members.filter((row)=>row.dataset.state==="working").length,waiting=members.filter((row)=>row.dataset.state==="waiting_user").length,queued=members.filter((row)=>row.dataset.state==="queued").length;
    const continued=members.filter(row=>row.dataset.state==="continued").length,blocked=members.filter(row=>["blocked","failed"].includes(row.dataset.state)).length,inactive=members.filter(row=>row.dataset.state==="inactive").length;
    const cancelled=members.filter(row=>row.dataset.state==="cancelled").length,stopped=members.filter(row=>row.dataset.state==="stopped").length,settled=members.length>0&&members.every(row=>["done","continued","cancelled","stopped"].includes(row.dataset.state));
    for(const member of members){
      const row=[...$("conversationFeed").querySelectorAll(".team-work-row")].find(node=>node.dataset.turnId===strip.dataset.turnId&&canonicalAgentId(node.dataset.agent)===member.dataset.groupStatusAgent);
      if(row){row.dataset.teamState=member.dataset.state;syncTeamWorkSummary(row);}
    }
    strip.classList.toggle("is-settled",settled);strip.hidden=members.every(row=>["working","done","continued","cancelled"].includes(row.dataset.state));strip.querySelector(".group-status-toggle").hidden=!settled;
    strip.querySelector("header small").textContent=waiting?`${waiting} waiting for you${working?` · ${working} still working`:""}`:working?`${working} working${queued?` · ${queued} queued`:""}`:queued?`${queued} queued`:blocked?`${blocked} need attention`:continued?`${continued} continued below`:stopped?`${stopped} stopped`:cancelled?`${cancelled} cancelled`:inactive?`${inactive} without a confirmed result`:`${members.filter((row)=>row.dataset.state==="done").length} replied`;
  }
  function reconcileGroupStatusContinuations(){
    // Reconcile visible historical chips from typed provenance only. Never
    // infer a continuation from prose, the selected agent, or the newest turn.
    // This is a display projection; do not rewrite the original event history.
    const askTurns=new Map(),parents=new Map(),feed=$("conversationFeed");
    for(const entry of state.displayRows){
      const value=entry.value,turn=displayTurnId(entry),id=value?.id||value?.ask_id;
      if(value?.kind==="ask_pending"&&id&&turn)askTurns.set(id,askTurns.has(id)&&askTurns.get(id)!==turn?null:turn);
    }
    for(const entry of state.displayRows){
      const origin=entry.value?.origin,turn=displayTurnId(entry);
      const parent=origin?.kind==="group_continuation"?origin.original_turn_id:origin?.kind==="ask_answer"?askTurns.get(origin.ask_id):null;
      if(turn&&parent&&turn!==parent)parents.set(turn,parent);
    }
    const touched=new Set();
    for(const entry of state.displayRows){
      const event=entry.value;if(event?.kind!=="group_member_status")continue;
      const agentId=canonicalAgentId(event.agent_id||event.agent_name),seen=new Set();
      let parent=parents.get(displayTurnId(entry));
      while(parent&&!seen.has(parent)){
        seen.add(parent);
        // Older unloaded turns stay unloaded: a late update must never create
        // an old team card at the bottom of the current conversation.
        const strip=feed.querySelector(`.group-execution-strip[data-turn-id="${CSS.escape(parent)}"]`);
        const member=strip?.querySelector(`[data-group-status-agent="${CSS.escape(agentId)}"]`);
        if(member){
          const terminal=["done","failed","cancelled"].includes(event.state);
          updateGroupMemberStatus(member,terminal?event.state:"continued",terminal?"Settled by the continuation below":"This task moved to the continuation below");
          touched.add(strip);
        }
        parent=parents.get(parent);
      }
    }
    touched.forEach(updateGroupStatusSummary);
  }

  function claimQueuedUser(event){
    const text=String(event.text||""),turnId=String(event.turn_id||""),index=state.displayRows.findIndex((entry)=>displayRole(entry)==="user"&&entry.value?.queued_id&&!entry.value?.queued_canonical&&((turnId&&entry.turn_id===turnId)||entry.value.queued_request===text||entry.value.text===humanMentions(text)));
    let entry,prompt;
    if(index>=0){
      [entry]=state.displayRows.splice(index,1);entry.value.queued_canonical=true;state.displayRows.push(entry);
      prompt=$("conversationFeed").querySelector(`.user-message[data-queued-id="${CSS.escape(String(entry.value.queued_id))}"]`);
      if(prompt)prompt.dataset.queuedPending="false";
    }else{
      const draft=state.queuedDrafts.get(turnId)||[...state.queuedDrafts.values()].find((candidate)=>candidate.requestText===text);
      if(!draft)return false;
      entry={source:"history",turn_id:turnId||draft.turnId,value:{role:"user",turn_id:turnId||draft.turnId,text:draft.displayText,initiating_agent_id:draft.initiatingAgentId,attachments:draft.files,queued_id:draft.queueId,queued_request:draft.requestText,queued_canonical:true}};
      state.displayRows.push(entry);
      prompt=renderUser(draft.displayText,draft.files,{id:draft.queueId,canonical:true});
      state.queuedDrafts.delete(draft.turnId);
    }
    state.activeTurnId=entry.turn_id||"";replaceDisplayRows(trimDisplayRows(state.displayRows),true);scheduleDisplayPersist(true);
    // This boundary may arrive a few milliseconds before the old foreground
    // socket's Done callback. It still owns the next turn: show its cube now
    // and keep the old socket cleanup from stopping this new activity.
    finishTurnActivity(false);state.queuedWakeTurnId=entry.turn_id||"";state.turnStartedAt=Date.now();setWorking(true);beginTurnActivity(prompt);
    renderQueue();return true;
  }
  function claimCanonicalUser(event){
    const turnId=String(event?.turn_id||"");if(!turnId)return false;
    const existing=state.displayRows.find((entry)=>displayRole(entry)==="user"&&displayTurnId(entry)===turnId);
    if(!existing)return false;
    existing.value.canonical_request=String(event.text||"");existing.value.queued_canonical=true;state.activeTurnId=turnId;state.displayDirty=true;scheduleDisplayPersist(true);return true;
  }
  function normalizeGroupOperationalAgent(event){
    if(state.item?.kind!=="group"||!event||!["narration","commentary","reasoning","thinking","tool_start","tool","receipt","settled","usage","context_compaction"].includes(event.kind))return event;
    const active=state.activeGroupAgentIds.map(canonicalAgentId).filter(Boolean),reported=canonicalAgentId(event.agent||"");
    // A room transport can report the coordinator role while its one selected
    // coworker owns the turn. Never paint that operational trace as Phoenix;
    // delegated coworkers keep their own explicit identities.
    if(active.length===1&&!reported)return{...event,agent:active[0]};
    return event;
  }
  const LIVE_ACTIVITY_STORY_KINDS=new Set(["narration","commentary","reasoning","thinking","tool_start","tool","context_compaction"]);
  function wakeVisibleTurnForStory(event,replay=false){
    // Turns can be started by the right-side workspace, a routine, a queued
    // follow-up, or another Phoenix surface. In those cases this conversation
    // owns only the journal subscription—not the foreground Turn socket—so the
    // normal submit path never calls beginTurnActivity. Treat the first live
    // work event as that missing process edge. Replays stay inert and a late
    // receipt cannot resurrect a turn that already has an answer/settlement.
    if(replay||state.painting||state.working||!LIVE_ACTIVITY_STORY_KINDS.has(event?.kind)||latestTurnIsTerminal())return false;
    const prompts=[...$("conversationFeed").querySelectorAll(":scope > .user-message:not([data-queued-pending=\"true\"])")];
    const prompt=prompts.at(-1)||null;
    state.pinToLatest=true;
    state.turnStartedAt=Date.now();
    setWorking(true);
    beginTurnActivity(prompt,event.agent||targetAgent()||"phoenix");
    return true;
  }
  function fluffyContext(replay = false) { return { sessionId:state.sessionId, owner:canvasConversationOwner(), replay, painting:state.painting, midProgress:state.working }; }
  function consumeFluffyWire(value, context = fluffyContext()) {
    if (value?.StoryReplay) window.PhoenixFluffies?.wireInput(value.StoryReplay, { ...context, replay:true });
    if (value?.Story) window.PhoenixFluffies?.wireInput(value.Story, context);
  }
  function renderRecoveredStory(event){
    const feed=$("conversationFeed"),turnId=ownedStoryTurn(event)||state.activeTurnId,agent=canonicalAgentId(event.agent_id||event.agent||state.item?.id||"phoenix");
    const answer=[...feed.querySelectorAll(':scope > .agent-message,:scope > .group-message')].find(node=>node.dataset.turnId===turnId&&node.dataset.publicReply==='true'&&(state.item?.kind!=='group'||node.dataset.speaker===agent));
    if(!answer)return false;
    const bookmark=conversationScrollBookmark(),existing=new Set(feed.children);
    const trace=[...feed.querySelectorAll('.work-cluster')].reverse().find(node=>node.dataset.turnId===turnId&&canonicalAgentId(node.dataset.agent)===agent&&(node.compareDocumentPosition(answer)&Node.DOCUMENT_POSITION_FOLLOWING));
    const keys=['painting','renderingTurnId','replayWorkCluster','turnStatus','activeTools','toolRows','shimmerClusters','recoveredWorkCluster'];
    const saved=Object.fromEntries(keys.map(key=>[key,state[key]]));
    state.painting=true;state.renderingTurnId=turnId;state.replayWorkCluster=trace;state.recoveredWorkCluster=trace;state.turnStatus=trace;
    state.activeTools=new Map();state.toolRows=[];state.shimmerClusters=new Set();
    try{
      const pending=event.kind==='tool'&&trace?[...trace.querySelectorAll('.work-tool[data-state="unknown"],.work-tool.running')].filter(row=>row.dataset.tool===String(event.tool||'')&&row.dataset.toolTarget===humanTarget(event.target,event.tool)):[];
      const callId=String(event.call_id||event.tool_call_id||''),matching=callId?pending.find(row=>row.dataset.toolCallId===callId):pending.length===1?pending[0]:null;
      if(matching){paintToolRow(matching,event,humanTool(event.tool,false,event.target));updateTraceSubgroup(matching.closest('.trace-subgroup'));}
      else renderStory(event,true);
      for(const node of [...feed.children])if(node!==conversationTail&&!existing.has(node))feed.insertBefore(node,answer);
      const recovered=[...feed.querySelectorAll('.work-cluster')].filter(node=>node.dataset.turnId===turnId&&canonicalAgentId(node.dataset.agent)===agent);
      recovered.forEach(node=>settleWorkCluster(node,false));attachWorkToggle(answer,trace||recovered.at(-1));
    }finally{Object.assign(state,saved);syncMessageGroups();restoreConversationScroll(bookmark);}
    return true;
  }
  function renderStory(event, replay = false) {
    if (!event?.kind) return;
    window.PhoenixFluffies?.wireInput(event, fluffyContext(replay));
    if (event.kind === "execution_ended") return;
    event=normalizeGroupOperationalAgent(event);
    if(!state.painting&&event.kind==="user"&&ownedStoryTurn(event)&&ownedStoryHasBoundary(event)){
      const boundaryIndex=(id)=>state.displayRows.findIndex((row)=>isAuthoredBoundaryEntry(row)&&displayTurnId(row)===id);
      if(boundaryIndex(ownedStoryTurn(event))<boundaryIndex(state.activeTurnId))return;
    }
    if(!state.painting&&replay&&event.kind==="user"&&ownedStoryTurn(event)&&state.activeTurnId&&ownedStoryTurn(event)!==state.activeTurnId&&!ownedStoryHasBoundary(event)){
      deferOwnedStory(event,replay);return;
    }
    if(event.kind==="subagent_lifecycle"){consumeVolumeWorkerLifecycle({Story:event});return;}
    if(event.kind==="handoff"&&isEphemeralVolumeHandoff(event))return;
    // A replay is history, never a fresh process edge. Repainting an old
    // tool_start used to create an unmatched `.running` row, which made every
    // old tool shimmer forever after switching conversations or relaunching.
    // The durable completed receipt is rendered separately; a genuinely live
    // reconnect uses the activity status and one general thinking signal.
    // Replaying an old start as live made completed tools shimmer forever.
    // But suppressing every replayed start also hid the tool Iris was
    // currently using whenever the user switched away and came back. The
    // company activity registry is the process-owned truth: only restore a
    // running row when that exact conversation is still live.
    if(state.item?.kind==="group"&&["steer","steer_delivered","brief"].includes(event.kind))return;
    if(state.item?.kind==="group"&&["tool_start","tool"].includes(event.kind)&&/^(talk|delegate|spawn)$/i.test(String(event.tool||"")))return;
    // A group turn is already projected as one stable GroupMessage per
    // contributor. The runner's compatibility aggregate repeats every answer
    // in one giant assistant bubble and must never enter the room transcript.
    if(state.item?.kind==="group"&&event.kind==="answer")return;
    if (isCompactionEvent(event) && !["settled", "usage", "tool_start", "steer_delivered"].includes(event.kind)) return;
    if(event.kind==="user"&&(claimQueuedUser(event)||(!state.painting&&!replay&&claimSteeredUser(event))||(!state.painting&&claimCanonicalUser(event))))return;
    // Direct threads persist only their owner's operational trace. Nested
    // coworker events arrive on the parent transport too, but retaining them
    // here leaks private tools and lets foreign lifecycle rows affect reloads.
    if(!storyVisibleInConversation(event))return;
    const providerRetry=event.kind==="notice"?providerRetryParts(event.text):null;
    const durable=DURABLE_STORY_KINDS.has(event.kind)&&!(event.kind==="commentary"&&isReasoningSummaryText(event.text))&&!providerRetry&&!(event.kind==="notice"&&!visibleNotice(event.text))&&!(event.kind==="user"&&isInternalRuntimeText(event.text)&&!askAnswerPrompt(event));
    // While repainting the persisted display journal, the matching row is the
    // row being painted. Only suppress duplicate StoryReplay traffic after the
    // initial paint. The old unconditional check erased reasoning and completed
    // tool receipts every time the user left and returned to a conversation.
    if(replay&&durable&&!state.painting){const candidate={source:"story",value:event,turn_id:event.turn_id||state.activeTurnId||""};if(state.displayRows.some((entry)=>entry.value?.historical!==true&&equivalentDisplayRows(entry,candidate)))return;}
    let placement="painted";
    if(durable&&!state.painting){placement=appendDisplay("story",event,["ask_pending","answer"].includes(event.kind),replay);if(!placement)return;
      if(ownedStoryTurn(event)&&ownedStoryTurn(event)!==state.activeTurnId){
        if(event.kind==="group_member_status")renderGroupMemberStatus({...event,turn_id:ownedStoryTurn(event)});
        else repaintOwnedTurn(ownedStoryTurn(event));
        return;
      }
      if(placement==="before_answer"){
      // Late receipts belong to finished work and cannot reopen its browser.
      if(!renderRecoveredStory(event)){if(replay)state.replayNeedsRepaint=true;else repaintOwnedTurn(ownedStoryTurn(event)||state.activeTurnId);}
      return;
    }}
    // Handoffs, returns, and inline questions remain visible collaboration
    // events because storyVisibleInConversation scopes only operational rows.
    // Replayed starts are historical edges, never proof that an individual
    // tool is still running. Restore only the generic live cursor from the
    // authoritative directory status; otherwise re-entering a thread revives
    // old "Running tools" rows after a stop or completed turn.
    // A turn started outside this window (a client, a routine, or before this
    // window subscribed mid-turn) has no local boundary, so every live event
    // used to be dropped and the thread looked frozen. While the directory
    // says this conversation is working and this window owns no turn, the
    // first live event of an unknown turn is the turn in progress: adopt it.
    if(!replay&&!state.painting&&!state.turnSocket&&ownedStoryTurn(event)&&ownedStoryTurn(event)!==state.activeTurnId&&!ownedStoryHasBoundary(event)&&LIVE_ACTIVITY_STORY_KINDS.has(event.kind)&&LIVE_CONVERSATION_STATUSES.has(ui.activityFor(state.item)?.status))state.activeTurnId=ownedStoryTurn(event);
    if(ownedStoryTurn(event)&&ownedStoryTurn(event)!==state.activeTurnId&&!state.painting)return;
    if(replay&&event.kind==="tool_start"){syncSelectedLiveActivity();return;}
    wakeVisibleTurnForStory(event,replay);
    switch (event.kind) {
      case "user": if (!state.turnSocket) {
        // A message queued behind a running turn is echoed when it finally
        // runs; the prompt the user sent is already on screen.
        if(event.turn_id&&[...$("conversationFeed").querySelectorAll(":scope > .user-message")].some((node)=>node.dataset.turnId===event.turn_id))break;
        const answerPrompt=renderGroupContinuationTurn(event)||renderAskAnswerTurn(event);
        if(answerPrompt&&!replay&&!state.painting&&!state.working){
          // A late popup answer is a real authored continuation. Its boundary
          // arrives before reasoning/tools, so begin the cube and stop state
          // here instead of leaving a blank pause that looks spontaneous.
          finishTurnActivity(false);state.queuedWakeTurnId=event.turn_id||"";state.turnStartedAt=Date.now();setWorking(true);beginTurnActivity(answerPrompt,event.origin?.agent_id||targetAgent()||"phoenix");
        }else if(!answerPrompt&&!isInternalRuntimeText(event.text)){if(!renderScheduledTurn(event))renderUser(event.text);}
      } break;
      case "narration": if(!isReasoningSummaryText(event.text))renderAgentUpdate(event.agent, event.text, true); break;
      case "commentary": {
        if(isReasoningSummaryText(event.text))renderThinking(event.agent, event.text);
        else { const update=renderAgentUpdate(event.agent, event.text); window.PhoenixFluffies?.visibleCommentary(event,update,fluffyContext(replay)); }
      } break;
      case "thinking": case "reasoning": renderThinking(event.agent, event.text); break;
      case "tool_start": case "tool":
        renderTool(event,replay);
        // Some background/group transports expose only the completed receipt.
        // Either edge is sufficient proof that this coworker's managed browser
        // exists and belongs in the selected conversation's right sidebar.
        if(!replay&&!state.painting&&/^browser_(?!close$)/i.test(String(event.tool||""))&&(event.kind==="tool_start"||event.ok!==false))autoRevealAgentBrowser(event);
        break;
      case "context_compaction": renderContextCompaction(event,replay); break;
      case "receipt": renderReceipt(event); break;
      case "handoff": renderHandoff(event); break;
      case "group_message": renderGroupMessage(event); break;
      case "group_member_status": renderGroupMemberStatus(event); break;
      case "return":
        // A correlated coworker result settles the earlier handoff in-place.
        // Rendering the same payload again as a peer bubble manufactured a
        // false message after the owner's final answer on every reload.
        renderReturn(event);
        renderIncomingAgentTalk({...event,from:event.agent||event.from});
        break;
      case "card": if(!isTransientProviderBoundary(`${event.subject||""} ${event.body||event.text||""}`))renderCard(event, "company update"); break;
      case "brief": feedNode("brief-card", `<strong>${escape(agentLabel(event.agent))} received</strong><div class="markdown">${markdown(event.text)}</div>`); break;
      // Steering is internal delivery plumbing. The authored user message is
      // already in the thread, while agent-to-agent communication has its own
      // compact handoff row. Rendering this created giant duplicate
      // “Phoenix steered Avery” blocks and exposed runtime routing as chat.
      case "steer": case "steer_delivered": break;
      case "failure":
        renderFailure(event);
        if(notificationEnabled("attention"))ui.notify({title:`${agentLabel(event.agent)} needs attention`,body:internalReturnFailure(event.text),item:{kind:"agent",id:event.agent},error:true,external:true});
        break;
      case "settled":
        if(replay||state.painting)break;
        // A worker finishing is not the whole group's completion. The live
        // directory or the owning turn's Done event settles shared presence.
        if(state.item?.kind==="group"){
          if(!isEphemeralVolumeAgent(event.agent))settleAgentWork(event.agent,!event.ok);
          syncTeamPresence();break;
        }
        if(event.agent&&canonicalAgentId(event.agent)!==canonicalAgentId(state.item?.id||"phoenix")){
          settleAgentWork(event.agent,!event.ok);break;
        }
        if (!state.turnSocket||state.queuedWakeTurnId) setWorking(false, !event.ok);
        state.queuedWakeTurnId="";
        settleAgentWork(event.agent, !event.ok);
        updateTaskHeadline(event.ok ? "Finished" : "Stopped", false);
        break;
      case "context": feedNode("context-card", `<details><summary>Using ${event.items.length} context item${event.items.length === 1 ? "" : "s"}</summary>${event.items.map((x) => `<div><strong>${escape(x.label)}</strong><small>${escape(x.kind)}</small><p>${escape(x.preview)}</p></div>`).join("")}</details>`); break;
      // Diffs stay in the tool rows; pasting a <pre> into the feed buried
      // the conversation under the largest block on the page.
      case "diff": break;
      case "ask_pending": renderApproval(event); break;
      case "answer":
        if(isInternalRuntimeText(event.markdown))break;
        if(runtimeFailureSummary(event.markdown)){renderRuntimeFailure(event.markdown,event.agent);break;}
        if(isTransientProviderBoundary(event.markdown)){clearProviderRetry();break;}
        if(!replay&&!state.painting)finishTurnActivity();
        {const boundary=friendlyBoundaryFailure(event.markdown);
          if(boundary)renderCard({agent:event.agent||state.item?.id||"phoenix",subject:"This run did not finish",body:boundary,ok:false},"stopped");
          else{const waiting=event.awaiting_input||state.displayRows.some((entry)=>displayRole(entry)==="answer"&&entry.value?.awaiting_input&&displayTurnId(entry)===(state.renderingTurnId||state.activeTurnId)&&answerKey(entry.value.markdown||entry.value.text||"")===answerKey(event.markdown));renderAnswer(event.markdown,null,{...event.meta,awaiting_input:waiting});if(!waiting)speakReply(event.markdown);}
        }
        break;
      case "cross_answer":
        if (replay||state.painting) break;
        {const key=`${event.session_id}:${answerKey(event.summary)}`;
          if(state.completionKeys.has(key))break;state.completionKeys.add(key);
          if (notificationEnabled("completions")) ui.notify({ title:`${event.project_name} finished`, body:event.summary, item:event.owner_kind&&event.owner_id?{kind:event.owner_kind,id:event.owner_id}:itemForConversationLabel(event.project_name), external:true,key:`completion:${key}` });
        }
        break;
      case "notice": {if(providerRetry){if(!replay&&!state.painting)renderProviderRetry(event.text,targetAgent()||state.item?.id||"phoenix");break;}if(renderWorkerBatchNotice(event))break;const text=visibleNotice(event.text);if(text)feedNode("notice-row",escape(text));break;}
      case "usage": {
        // Nested coworkers can report usage into the same story. Only mark the
        // turn as having an authoritative live sample when this event actually
        // belongs to the visible gauge. Otherwise Done.context_window must be
        // allowed to replace any persisted reading for the selected agent.
        const matchesGauge=state.item?.kind==="group"
          || agentLabel(event.agent).toLowerCase()===currentName().toLowerCase();
        if(matchesGauge){state.turnUsageSeen=true;updateContext(event.used,event.limit,state.sessionId,event.agent);}
        break;
      }
    }
    refreshTasksSoon();
  }

  function renderHistory(row) {
    if(!historyVisibleInConversation(row))return;
    const owner=row.agent||state.item?.id||"phoenix";
    const askAnswer=askAnswerPrompt(row);if(isInternalRuntimeText(row.text)&&!askAnswer)return;
    if (row.role === "user") {if(!renderGroupContinuationTurn(row)&&!renderAskAnswerTurn(row)&&!renderScheduledTurn(row)){const node=renderUser(row.text,Array.isArray(row.attachments)?row.attachments:[],row.queued_id?{id:row.queued_id,canonical:Boolean(row.queued_canonical)}:null,{steered:Boolean(row.steered)});if(node&&row.steer_id)node.dataset.steerId=row.steer_id;}}
    else if (row.role === "answer") {if(state.item?.kind==="group")return;renderAnswer(row.text, row.agent, row.meta);}
    else if (row.role === "narration" && !isReasoningSummaryText(row.text)) renderAgentUpdate(owner, row.text, true);
    else if (row.role === "commentary" && !isReasoningSummaryText(row.text)) renderAgentUpdate(owner, row.text);
    else if (row.role === "context") return;
    else if(row.role==="context_compaction")renderContextCompaction(row,true);
    else if(row.role==="group_message")renderGroupMessage(row);
    else if(row.role==="handoff")renderHandoff(row);
    else if(row.role==="return"){renderReturn(row);renderIncomingAgentTalk(row);}
    else if(row.role==="group_member_status")renderGroupMemberStatus(row);
    else if(row.role==="notice"){const text=visibleNotice(row.text);if(text)feedNode("notice-row",escape(text));}
    else if(row.role==="talk"){
      if(state.item?.kind==="group"){
        const isRoomReply=[currentName(),state.item.id].map((value)=>String(value||"").toLowerCase()).includes(String(row.to||"").toLowerCase()),legacyRound=/^discussion round\s+\d+/i.test(String(row.subject||""));
        if(!legacyRound){
          if(isCompletedAgentReturn(row)){renderReturn({...row,agent:row.from,body:row.text||row.body});renderIncomingAgentTalk(row);}
          else if(isRoomReply)renderGroupMessage({agent_id:row.from,agent_name:agentLabel(row.from),markdown:row.text||row.body||"",round:1});
          else if(agentContextMessageMeta(row))renderAgentContextMessage(row);
          else renderHandoff({...row,subject:row.subject||row.text});
        }
      }else{
        const contextMessage=agentContextMessageMeta(row);
        if(isIncomingAgentTalk(row)){
          const completed=!contextMessage&&isCompletedAgentReturn(row),matched=completed?renderReturn({...row,agent:row.from,body:row.text}):null;
          // A completed return without its (trimmed) handoff is settlement
          // metadata, not a new authored message. Showing it as a peer bubble
          // after the owner's final made already-integrated work appear late.
          if(!contextMessage)renderIncomingAgentTalk({...row,body:row.text||row.body});else renderAgentContextMessage(row);
          return;
        }
        if(contextMessage){renderAgentContextMessage(row);return;}
        const phoenixSide=[row.from,row.to].some((value)=>/^(?:phoenix|orchestrator)$/i.test(String(value||"")));
        // Historical mode-2 hub traffic was runtime plumbing, not a useful
        // coworker conversation. Keep real peer handoffs and synchronous
        // questions, but do not repaint old Phoenix↔owner acknowledgement
        // loops as a wall of arrows after this direct-routing fix.
        const ownerReturn=canonicalAgentId(row.from)===canonicalAgentId(state.item.id)&&/^(?:phoenix|orchestrator)$/i.test(String(row.to||""))&&row.ok===true;
        if(!ownerReturn&&!(row.reply_expected===false&&phoenixSide))renderHandoff({ from:row.from, to:row.to, subject:row.subject || row.text, background:row.background, handoff_id:row.handoff_id, requester:row.requester, receiver:row.receiver, status:row.status, causation_id:row.causation_id });
      }
    }
    else if (row.role === "tool") renderTool({ kind:"tool", agent:owner, tool:row.tool, target:row.target, ok:row.ok, detail:row.detail, ...(row.diff ? { diff:row.diff } : {}) });
    else if (row.role === "ask") renderApproval(row);
  }

  function mockHistory() {
    if(previewShot === "monocode-empty")return [];
    if(previewShot==="conversation-isolation"){
      const name=currentName(),agent=state.item?.id||"phoenix";
      const rows=Array.from({length:18},(_,index)=>[
        {role:"user",text:`${name} private prompt ${index+1}: keep this conversation isolated while switching agents.`},
        {role:"answer",agent,text:`${name} private answer ${index+1}. This row belongs only to ${name}. ${"Stable local context. ".repeat(5)}`},
      ]).flat();rows.splice(4,0,{role:"answer",agent:agent==="phoenix"?"school_coach":"phoenix",text:"FOREIGN TERMINAL ANSWER MUST NOT RENDER"});return rows;
    }
    if (new URLSearchParams(location.search).get("shot") === "thinking-groups") return [
      {role:"user",text:"Inspect the current experience, verify it in Chrome, and make the trace feel calm."},
      {role:"reasoning",agent:"phoenix",text:"I’m mapping the conversation renderer first so the visual change preserves the real turn lifecycle."},
      {role:"tool",agent:"phoenix",tool:"web_search",target:'{"query":"compact agent activity trace"}',ok:true,detail:'{"results":[{"title":"Interaction reference","url":"https://example.com/reference"}]}'},
      {role:"tool",agent:"phoenix",tool:"browser_navigate",target:"https://example.com/reference",ok:true},
      {role:"tool",agent:"phoenix",tool:"browser_extract",target:"Visible approval and activity patterns",ok:true},
      {role:"ask",id:"preview-trace-answer",agent:"phoenix",status:"answered",display_answers:["Keep it compact"],questions:[{header:"Trace density",question:"How compact should completed edits be?",options:["Keep it compact","Leave it open"],multi_select:false}]},
      {role:"reasoning",agent:"phoenix",text:"The browser actions belong together. I’ll keep the coding details collapsed until you ask for them."},
      {role:"tool",agent:"phoenix",tool:"read",target:"conversation.css",ok:true},
      {role:"tool",agent:"phoenix",tool:"str_replace",target:"conversation.js",ok:true,detail:"+84 -31"},
      {role:"tool",agent:"phoenix",tool:"bash",target:"node --check conversation.js",ok:true},
      {role:"talk",from:"phoenix",to:"scribe",subject:"Verify the final language",text:"Check that the settled trace stays clear and concise."},
      {role:"answer",agent:"phoenix",text:"The turn now stays together: reasoning, browsing, edits, the answered question, and Nico’s handoff all live under one durable trace.",meta:{created_at:new Date().toISOString(),elapsed_ms:31000}},
    ];
    if (new URLSearchParams(location.search).get("shot") === "work-live") return [
      {role:"user",text:"Open the account page and sign me in."},
      {role:"reasoning",agent:"phoenix",text:"**Planning visible browser use**\nThe account page is open. I’m using its accessible Sign in control directly instead of guessing coordinates."},
      {role:"tool",agent:"phoenix",tool:"computer_app_read",target:'{"app":"Zen","session_id":"internal-do-not-show"}',ok:true},
      {role:"reasoning",agent:"phoenix",text:"The form is ready; the saved account is protected, so I need you to unlock the vault here before I can fill it."},
      {role:"tool",agent:"phoenix",tool:"composio_search",target:'{"queries":[{"use_case":"find connected login tools"}]}',ok:true},
    ];
    if (new URLSearchParams(location.search).get("shot") === "vault") return [
      {role:"user",text:"Use my saved account and continue."},
      {role:"ask",id:"preview-vault",agent:"phoenix",questions:[{header:"Unlock vault",question:"The saved account is ready, but your credential vault is locked. Unlock it here so I can continue signing in.",options:["Unlock here","Not now"],multi_select:false}],approval:{action:"vault_unlock",details:{reason:"Sign in with your approved saved account"}}},
    ];
    if (new URLSearchParams(location.search).get("shot") === "question-history") return [
      {role:"user",text:"Can I teach you this browser workflow?"},
      {role:"tool",agent:"phoenix",tool:"routine",target:"find a matching workflow",ok:true,detail:"No matching workflow was saved yet."},
      {role:"ask",id:"preview-answered",agent:"phoenix",status:"answered",display_answers:["Teach now"],questions:[{header:"Teach workflow",question:"Show me how you generate an image in the browser?",options:["Teach now","Not now"],multi_select:false}],approval:{action:"teach_workflow",subject:"generate an image",approved_option:"Teach now",details:{owner_agent_id:"phoenix",scope:"agent"}}},
      {role:"user",text:"Great—keep that question and its result here after I reopen Phoenix."},
    ];
    if (["teach","teach-download"].includes(new URLSearchParams(location.search).get("shot"))) return [
      {role:"user",text:"I can show you exactly how I generate an image in ChatGPT."},
      {role:"ask",id:"preview-teach",agent:"phoenix",questions:[{header:"Teach workflow",question:"Show me how you generate an image in ChatGPT in the browser? I’ll follow along and save it as a reusable workflow.",options:["Teach now","Not now"],multi_select:false}],approval:{action:"teach_workflow",subject:"generate an image in ChatGPT",approved_option:"Teach now",details:{owner_agent_id:"phoenix",workflow_goal:"generate an image in ChatGPT",start_url:"https://chatgpt.com",scope:"agent"}}},
    ];
    if (new URLSearchParams(location.search).get("shot") === "login") return [
      {role:"user",text:"Open the launch dashboard and finish the approved setup."},
      {role:"ask",id:"preview-login",agent:"scribe",questions:[{header:"Login needed",question:"Nico needs access to example.com to finish the launch setup. How should Phoenix continue?",options:["Import from my browser","I'll log in","Create an account","Not now"],multi_select:false}],approval:{action:"login_request",subject:"example.com",approved_option:"Import from my browser",details:{site:"example.com",agent_id:"scribe",scope:"agent",methods:"import_cookies,user_login,create_account"}}},
    ];
    if (new URLSearchParams(location.search).get("shot") === "ask") return [
      {role:"user",text:"Set up the launch account and use the right company scope."},
      {role:"ask",id:"preview-account-setup",agent:"scribe",question:"Which account should Nico use?",options:["Work","Personal"],questions:[{header:"Account",question:"Which account should Nico use?",options:["Work","Personal","A separate launch account"],multi_select:false},{header:"Sharing",question:"Who should be allowed to reuse this website login?",options:["Only Nico","Launch Room","Whole company"],multi_select:false}],approval:null},
    ];
    if (new URLSearchParams(location.search).get("shot") === "image-context") return [
      {role:"user",text:"Show me the visual proof in context and call out what you checked."},
      {role:"answer",agent:"phoenix",text:"## Visual verification\n\nThe supplied Phoenix mark is rendered as a real image exactly where this proof belongs in the answer. Click it to open the named image tab and add a pinpoint comment.\n\n![Phoenix logo proof](/workspace/phoenix-logo.png)\n\n### Checked\n\n- Transparent raster asset\n- No synthetic tile behind the mark\n- Opens in the conversation image workspace"},
    ];
    if (state.item?.kind === "group") return [
      {role:"user",text:"@Iris @Leo What should make the first public Linux build?"},
      {role:"reasoning",agent:"frontend",text:"I’m checking the shared room context and the interaction details that earn trust."},
      {role:"context_compaction",agent:"frontend",status:"completed",before_tokens:251904,after_tokens:93184,folded_messages:42,limit:262144},
      {role:"talk",from:"Iris",to:"Launch Room",subject:"The UI promise",text:"## Product bar\n\n- A quiet shell\n- Immediate feedback\n- Every control connected to real behavior"},
      {role:"talk",from:"Leo",to:"Launch Room",subject:"The runtime promise",text:"### Runtime proof\n\n1. Canonical shared context\n2. A fast embedded browser that never escapes the app"},
    ];
    return [
      {role:"user",text:"Keep every useful memory and canonical thread I already have."},
      {role:"answer",text:"The existing company history remains authoritative. Phoenix links it into the rebuilt shell instead of replacing it."},
      {role:"user",text:"Make every setting real, searchable, and immediately effective."},
      {role:"answer",text:"Settings now write through one typed policy layer, with the current company or coworker scope shown before each change."},
      {role:"user",text:"Let every coworker use the full toolset and ask one another for help."},
      {role:"answer",text:"Every visible coworker receives the shared registry. Role-specific memory and skills improve judgment without hiding tools."},
      {role:"user",text:"Make browser login recovery practical instead of inventing workarounds."},
      {role:"answer",text:"Login, import, and account creation use explicit browser actions, scoped credentials, and resumable private profiles."},
      {role:"user",text:"Build this carefully and keep everything I already have."},
      {role:"reasoning",agent:state.item?.id || "phoenix",text:"Now I will recover the existing company thread and keep every useful memory attached to this conversation."},
      {role:"tool",agent:state.item?.id || "phoenix",tool:"codebase_search",target:"existing Phoenix systems",ok:true,detail:"Recovered the canonical company thread and its memory sources."},
      {role:"answer",text:`I’m ready. This is ${currentName()}’s one continuous thread—past context stays available through Phoenix’s librarian and indexer, without dumping it all into every prompt.`,meta:{created_at:new Date().toISOString(),elapsed_ms:187000}},
    ];
  }

  function rpc(request, timeout = 8000, signal = null) {
    if(signal?.aborted)return Promise.reject(new DOMException("Conversation changed","AbortError"));
    if (preview) return mockRpc(request);
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(ui.wsUrl()); let settled = false;
      const onAbort=()=>finish(reject,new DOMException("Conversation changed","AbortError"));
      const finish = (fn, value) => { if (settled) return; settled = true; clearTimeout(timer);signal?.removeEventListener("abort",onAbort);try { socket.close(); } catch {} fn(value); };
      const timer = setTimeout(() => finish(reject, new Error("Phoenix did not answer in time.")), timeout);
      signal?.addEventListener("abort",onAbort,{once:true});
      socket.onopen = () => {if(!settled)socket.send(JSON.stringify(request));};
      socket.onerror = () => finish(reject, new Error("Could not reach the Phoenix gateway."));
      socket.onmessage = (message) => { try { const value = JSON.parse(message.data); value.Error ? finish(reject,new Error(value.Error.message)) : finish(resolve,value); } catch (error) { finish(reject,error); } };
    });
  }
  function mockRpc(request) {
    if(request==="DesktopWorkspaces")return Promise.resolve({DesktopWorkspaces:[]});
    if(request.DeleteTranscriptTurn)return Promise.resolve({TranscriptDeleted:{removed_messages:3,deleted_prompt:request.DeleteTranscriptTurn.scope==="Prompt"}});
    if (request.QueuedTurns) return Promise.resolve({QueuedTurns:state.previewQueues.get(conversationIdentity()) || []});
    if (request.TodoList) return Promise.resolve({TodoList:state.tasks.length ? state.tasks : [{id:"1",text:"Connect the canonical conversation",status:"completed"},{id:"2",text:"Wire task and approval state",status:"in_progress"},{id:"3",text:"Verify the complete interaction",status:"pending"}]});
    if (request.ConversationAsks) return Promise.resolve({ConversationAsks:[]});
    if (request.GroupActivationPreview) {
      const groupId=request.GroupActivationPreview.group_id,members=(ui.state.view?.directory.members||[]).filter((member)=>member.group_id===groupId).sort((a,b)=>a.sort_order-b.sort_order),all=request.GroupActivationPreview.user_request.trim().startsWith("@everyone"),tokens=new Set([...request.GroupActivationPreview.user_request.matchAll(/(?:^|\s)@([a-z0-9_-]+)/gi)].map((match)=>match[1].toLowerCase())),leaderId=ui.state.view?.directory.groups.find((group)=>group.group_id===groupId)?.leader_agent_id,mentioned=members.map((member)=>knownAgentProfile(member.agent_id)).filter((profile)=>activatableCoworker(profile)&&(all||tokens.has(profile.agent_id.toLowerCase()))),active=mentioned.length?mentioned:[knownAgentProfile(leaderId)].filter(activatableCoworker);
      return Promise.resolve({GroupActivationPreview:{group_id:groupId,roster_fingerprint:`preview-${members.map((member)=>member.agent_id).join("-")}`,selection:all?"everyone":"explicit",active_agent_ids:active.map((profile)=>profile.agent_id),active_display_names:active.map((profile)=>profile.display_name),execution_mode:"parallel",execution_waves:active.length?[active.map((profile)=>profile.agent_id)]:[],execution_wave_display_names:active.length?[active.map((profile)=>profile.display_name)]:[]}});
    }
    if (request.Settings?.action === "models_snapshot") return Promise.resolve({Settings:{result:"models",models:{config_revision:"preview-context",lanes:[{lane:"phoenix",provider_id:"openai",model:"gpt-6.1-sol",reasoning_effort:"xhigh",context_window:1050000,max_context_window:1050000,context_window_override:null},{lane:"specialist",provider_id:"anthropic",model:"claude-opus-4-6",reasoning_effort:"high",context_window:1000000,max_context_window:1000000,context_window_override:null}],providers:[],catalog:[{id:"gpt-6.1-sol",name:"GPT-6.1 Sol",provider:"openai",context_window:1050000,effort_levels:["minimal","low","medium","high","xhigh","max"]},{id:"gpt-4.1-mini",name:"GPT-4.1 mini",provider:"openai",context_window:1047576,effort_levels:[]},{id:"claude-opus-4-6",name:"Claude Opus 4.6",provider:"anthropic",context_window:1000000,effort_levels:["low","medium","high"]},{id:"gemini-3.1-pro",name:"Gemini 3.1 Pro",provider:"google",context_window:1000000,effort_levels:["low","medium","high"]}]}}});
    if (request.TeachWorkflow?.action === "begin") return Promise.resolve({TeachWorkflow:{result:"begun",teaching:{teaching_id:"preview-teaching",steps:[],browser_profile_id:state.item?.id || "phoenix",owner_agent_id:state.item?.id || "phoenix",scope:"agent",group_id:null}}});
    if (request.TeachWorkflow?.action === "revise") {const owner=state.revisionOwnerHint||"marketing";return Promise.resolve({TeachWorkflow:{result:"workflow_revision_started",teaching:{teaching_id:"preview-revision",steps:[{}],browser_profile_id:owner,owner_agent_id:owner,scope:"agent",group_id:null,revises_routine_id:request.TeachWorkflow.routine_id},routine:{routine_id:request.TeachWorkflow.routine_id,name:"Publish the weekly product update",description:"Publish the approved update in the company voice.",trigger_phrases:["publish the weekly update"],owner_agent_id:owner,scope:"agent",group_id:null}}});}
    if (request.TeachWorkflow?.action === "interact") { state.teaching.teaching.steps.push({}); return Promise.resolve({TeachWorkflow:{result:"recorded",teaching:state.teaching.teaching}}); }
    return Promise.resolve({Pong:null});
  }

  async function selectConversation(detail) {
    const item=detail?.item;if(!item?.kind||!item?.id)return;
    const sessionId=detail.sessionId||ui.activityFor(item)?.canonical_session_id||`company-${item.id}`,generation=++state.loadGeneration;
    if(conversationIdentity(item,sessionId)!==conversationIdentity())invalidateComposerIngress();
    state.selectionAbortController?.abort();const controller=new AbortController();state.selectionAbortController=controller;
    const token=selectionToken(generation,item,sessionId,controller),feed=$("conversationFeed");
    clearTimeout(state.queueRefreshTimer);state.queueRefreshTimer=0;
    state.subscription?.close();state.subscription=null;state.turnSocket?.close();state.turnSocket=null;
    clearProviderRetry();
    if(state.item){persistComposerDraft(state.item);flushDisplayJournal();stashConversationView();}
    else{clearFeed();closeApproval();}
    state.item=item;state.sessionId=sessionId;syncGroupPals();feed.dataset.conversationKey=token.key;restoreSavedContextUsage(sessionId);
    setImageCommentMode(false);state.inspectionImages=[];state.activeInspectionImageId="";state.imageAnnotations.clear();renderInspectionImageTabs();
    syncWorkingElsewhere();
    syncInspectionHeader();
    restoreContext(state.sessionId);
    state.tasks=[...(state.tasksBySession.get(token.key)||state.tasksBySession.get(state.sessionId)||[])];
    state.queue=[...(state.queueBySession.get(token.key)||state.queueBySession.get(state.sessionId)||[])];
    const isolatedProof=["image-context","image-tabs-proof","summary-proof","handoff-proof","browser-proof","group","attachment","inspection"].includes(previewShot),draft=isolatedProof?{text:"",attachments:[],mentions:[],everyone:false}:restoredComposerDraft();
    $("composerInput").value = draft.tokens ?? draft.text; state.attachments = [...draft.attachments]; state.mentions=[...(draft.mentions||[])];state.groupEveryone=Boolean(draft.everyone&&state.item?.kind==="group");
    if(preview&&new URLSearchParams(location.search).get("shot")==="group")state.mentions=["frontend","coder"];
    renderAttachments();renderMentionTray();autosize();
    state.permission = localStorage.getItem(permissionKey()) || "workspace";
    if(localStorage.getItem(permissionKey()))persistAgentPermission(state.permission);
    state.reasoning = localStorage.getItem(`phoenix-reasoning:${modelLane() || "group"}`) || "high";
    const cachedView=takeConversationView(token.key),cached=Boolean(cachedView?.ready&&restoreConversationView(cachedView));
    if(!cached){resetConversationRuntime();renderConversationLoading();}
    else feed.removeAttribute("aria-busy");
    updateComposerLabels();renderQueue();syncBrowserChrome();
    dispatchEvent(new CustomEvent("phoenix:conversation-selected",{detail:{item:state.item,sessionId:state.sessionId,cached}}));
    peekConversationBrowser().catch(()=>{});
    loadWorkspace().then(()=>{if(selectionIsCurrent(token))updateComposerLabels();}).catch(()=>{});
    state.historyLoadFailed=false;state.historyHydrating=true;state.historyLiveRows=[];state.displayJournalUnsafe=true;
    // Live events must not wait behind a slow native history projection.
    subscribeJournal(token);
    let rows=[],feeds={},journalRecovered=true,historyRecovered=true,asksRequest=null;
    if(preview)rows=mockHistory();
    else{
      const owner=token.owner;
      asksRequest=rpc({ConversationAsks:{session_id:token.sessionId,owner}},8000,token.signal).then((value)=>value.ConversationAsks||[]);
      // Attach rejection handling immediately while the independent history
      // read is pending; the visible error is handled after hydration below.
      asksRequest.catch(()=>{});
      const recovered=await Promise.allSettled([
        ui.invoke("session_context_get",{sessionId:token.sessionId,owner}),
        ui.invoke("feeds_get",{sessionId:token.sessionId,owner}),
      ]);
      if(!selectionIsCurrent(token))return;
      if(recovered[0].status==="fulfilled")rows=recovered[0].value;else historyRecovered=false;
      if(recovered[1].status==="fulfilled")feeds=recovered[1].value;else journalRecovered=false;
      const failures=[["history",recovered[0]],["shown conversation",recovered[1]]]
        .filter(([,result])=>result.status==="rejected")
        .map(([label,result])=>`${label}: ${result.reason?.message||result.reason}`);
      if(failures.length)console.warn("Phoenix conversation recovery",failures);
    }
    if(!selectionIsCurrent(token))return;
    const durableReading=!cached?savedReadingPosition():null;
    const priorRows=state.displayRows,priorDirty=state.displayDirty,bookmark=cached?conversationScrollBookmark():null;
    // A journal we could not read is a journal we must not overwrite. Persisting
    // is re-armed only once a later load succeeds, so a transient read error
    // costs a repaint instead of the conversation.
    state.historyHydrating=false;state.historyLoadFailed=!historyRecovered||!journalRecovered;
    state.displayJournalUnsafe = state.historyLoadFailed;
    if(journalRecovered)state.answerMeta=parseAnswerMeta(feeds);
    state.displayMigrationDirty=false;
    const recoveredDisplay=journalRecovered?mergeHydratedLiveRows(displayRowsVisibleInConversation(parseDisplayRows(feeds)),state.historyLiveRows):priorRows;
    state.historyLiveRows=[];
    const repairedDisplay=repairRepeatedCatchUpTurns(recoveredDisplay,rows);
    replaceDisplayRows(repairedDisplay.rows,priorDirty||repairedDisplay.changed||state.displayMigrationDirty);
    state.displayMigrationDirty=false;
    reconcileHistory(rows);
    if(!cached||!displayRowsEqual(priorRows,state.displayRows))repaintConversation(bookmark,!cached,!cached);
    else feed.removeAttribute("aria-busy");
    if(state.historyLoadFailed)renderConversationLoadError();
    syncRestoredWorkVisibility();
    const live=preview&&new URLSearchParams(location.search).get("shot")==="work-live"||selectedConversationIsLive();
    if(live&&!state.working){setWorking(true);beginTurnActivity(null);}else if(!live&&state.working)setWorking(false);
    scheduleDisplayPersist(true);
    if(asksRequest)asksRequest.then((asks)=>{
      if(!selectionIsCurrent(token))return;
      const askBookmark=conversationScrollBookmark();
      if(reconcileAsks(asks)){repaintConversation(askBookmark,false);syncRestoredWorkVisibility();scheduleDisplayPersist(true);}
    }).catch((error)=>{if(selectionIsCurrent(token)&&error?.name!=="AbortError")ui.toast(`Could not load open requests — ${error.message||error}`,true);});
    Promise.allSettled([refreshTasks(token),refreshQueue(token),refreshModels(token)]).catch(()=>{});
    if(!cached){if(durableReading)restoreReadingPosition(durableReading);else anchorConversationBottom(token.generation,token.sessionId);}
  }
  function mergeHydratedLiveRows(recovered,live=[]){
    const merged=[...recovered],available=recovered.map(entry=>({entry,used:false}));
    for(const entry of live){
      const match=available.find(candidate=>!candidate.used&&displayTurnId(candidate.entry)===displayTurnId(entry)&&equivalentDisplayRows(candidate.entry,entry));
      if(match)match.used=true;else merged.push(entry);
    }
    return merged;
  }
  function conversationKeyOf(item){return item?.kind&&item?.id?`${item.kind}:${item.id}`:"";}
  function inspectionExpandedStorageKey(key=conversationKeyOf(state.item)){return key?`phoenix-inspection-expanded:${key}`:"";}
  function inspectionConversationState(key=conversationKeyOf(state.item)){
    if(!key)return null;
    let saved=state.inspectionConversationStates.get(key);
    if(saved)return saved;
    let expanded=false;
    try{expanded=localStorage.getItem(inspectionExpandedStorageKey(key))==="true";}catch{}
    let shell=null;try{shell=JSON.parse(localStorage.getItem(`phoenix-inspection-shell:${key}`)||'null');}catch{}
    saved={open:typeof shell?.open==='boolean'?shell.open:false,expanded,tab:INSPECTION_TABS.has(shell?.tab)?shell.tab:'changes',images:[],activeImageId:"",imageAnnotations:new Map(),browser:null};
    state.inspectionConversationStates.set(key,saved);
    return saved;
  }
  function cloneInspectionImages(images=state.inspectionImages){return images.map((item)=>({...item,comments:Array.isArray(item.comments)?item.comments.map((comment)=>({...comment})):item.comments}));}
  function cloneImageAnnotationMap(source=state.imageAnnotations){return new Map([...source].map(([id,comments])=>[id,(comments||[]).map((comment)=>({...comment}))]));}
  function rememberInspectionShell(key=conversationKeyOf(state.item)){
    const saved=inspectionConversationState(key);if(!saved)return null;
    saved.open=Boolean(state.inspectionOpen);saved.expanded=Boolean(state.inspectionExpanded);saved.tab=INSPECTION_TABS.has(state.inspectionTab)?state.inspectionTab:"desktop";
    try{localStorage.setItem(`phoenix-inspection-shell:${key}`,JSON.stringify({open:saved.open,expanded:saved.expanded,tab:saved.tab}));}catch{}
    return saved;
  }
  function rememberInspectionBrowser(key=state.browserBoundKey){
    if(!key||key!==conversationKeyOf(state.item)||!state.browserOwnerId)return;
    const saved=inspectionConversationState(key);if(!saved)return;
    const tabs=state.browserTabs.map((tab)=>({...tab})),active=tabs.find((tab)=>tab.active);
    saved.browser={ownerAgentId:state.browserOwnerAgentId||"phoenix",mode:state.browserMode||"browse",tabs,url:active?.url||state.browserFrameUrl||"about:blank"};
  }
  function captureInspectionConversation(key=conversationKeyOf(state.item)){
    const saved=rememberInspectionShell(key);if(!saved)return null;
    saved.images=cloneInspectionImages();saved.activeImageId=state.activeInspectionImageId;saved.imageAnnotations=cloneImageAnnotationMap();
    if(state.browserBoundKey===key&&state.browserMode==="browse")rememberInspectionBrowser(key);
    return saved;
  }
  function clearInspectionForConversationSwitch(){
    state.inspectionOpen=false;state.inspectionExpanded=false;state.inspectionTab="desktop";state.inspectionImages=[];state.activeInspectionImageId="";state.imageAnnotations=new Map();state.browserTabs=[];state.browserFrameUrl="";state.browserAddressEditing=false;state.browserAddressPending="";
    document.body.classList.remove("inspection-open","inspection-expanded","inspection-browser-active","inspection-image-active","image-commenting");
    $("inspectionSidebar")?.setAttribute("aria-hidden","true");syncBrowserAddress("about:blank",true);
    renderInspectionBrowserTabs({tabs:[]});renderInspectionImageTabs();syncPanelControlLocation();syncBrowserChrome();
  }
  async function restoreInspectionConversation(key){
    if(!key||key!==conversationKeyOf(state.item))return;
    const stored=inspectionConversationState(key);if(!stored)return;
    // Rendering tab chrome calls rememberInspectionShell. Snapshot first so
    // that initial closed DOM cannot rewrite the open state being restored.
    const saved={...stored};
    state.inspectionImages=cloneInspectionImages(saved.images||[]);state.activeInspectionImageId=saved.activeImageId||"";state.imageAnnotations=cloneImageAnnotationMap(saved.imageAnnotations||new Map());renderInspectionImageTabs();
    const browser=saved.browser,tabs=(browser?.tabs||[]).map((tab)=>({...tab}));state.browserTabs=tabs;state.browserAddressEditing=false;state.browserAddressPending="";syncBrowserAddress(browser?.url||"about:blank",true);renderInspectionBrowserTabs({tabs});
    state.inspectionTab=INSPECTION_TABS.has(saved.tab)?saved.tab:"desktop";state.inspectionExpanded=Boolean(saved.expanded);
    if(saved.open){showInspectionSidebar(state.inspectionTab);if(state.inspectionTab==="image"&&state.activeInspectionImageId)paintActiveInspectionImage();}
    else{
      state.inspectionOpen=false;setInspectionTab(state.inspectionTab);document.body.classList.remove("inspection-open","inspection-expanded","inspection-browser-active","inspection-image-active");$("inspectionSidebar")?.setAttribute("aria-hidden","true");syncPanelControlLocation();applyInspectionExpanded(Boolean(saved.expanded),false);
    }
  }
  async function selectConversationWithInspection(detail){
    const nextKey=conversationKeyOf(detail?.item),currentKey=conversationKeyOf(state.item),generation=++state.inspectionSwitchGeneration;
    if(currentKey)captureInspectionConversation(currentKey);
    if(currentKey!==nextKey){clearEphemeralWorkersForConversation(nextKey);if(state.browserOwnerId)closeBrowser({preserveWorkspace:true}).catch(()=>{});clearInspectionForConversationSwitch();}
    const selection=selectConversation(detail);
    // selectConversation commits item/session and dispatches its selected event
    // before waiting on history. Restore the independent workspace on that
    // immediate paint path so a slow transcript can never keep the sidebar
    // blank, grey, or populated with the previous agent's tabs.
    if(generation===state.inspectionSwitchGeneration&&nextKey===conversationKeyOf(state.item))await restoreInspectionConversation(nextKey);
    await selection;
  }
  function syncBrowserChrome(){
    const overlay=$("browserOverlay");
    if(!overlay)return;
    const settingsOpen=Boolean($("settingsView")&&!$("settingsView").hidden),modalOpen=Boolean($("modalLayer")&&!$("modalLayer").hidden);
    const visible=Boolean(state.browserBoundKey)&&state.inspectionOpen&&state.inspectionTab==="browser"&&!settingsOpen&&!modalOpen&&conversationKeyOf(state.item)===state.browserBoundKey;
    overlay.hidden=!visible;
    if(state.browserNative){const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration;queueBrowserSurfaceOperation(async()=>{if(!state.browserNative||state.browserOwnerId!==instance||state.browserSurfaceGeneration!==generation)return;await ui.invoke(visible?"browser_surface_show":"browser_surface_hide",{instance});}).catch(()=>{});if(visible)scheduleNativeBrowserBounds();}
  }

  const INSPECTION_TABS=new Set(["browser","image","sources","desktop",...(document.documentElement.dataset.skin==="phoenix"?["agents"]:[])]),EDIT_TOOLS=new Set(["write","str_replace","apply_patch"]);
  // The right panel's tabs overflow sideways; a normal (vertical) wheel
  // scrolls them, and a trackpad's sideways swipe keeps working.
  $("inspectionTabStrip")?.addEventListener("wheel",(event)=>{
    const strip=event.currentTarget;if(strip.scrollWidth<=strip.clientWidth+1||Math.abs(event.deltaX)>Math.abs(event.deltaY))return;
    event.preventDefault();strip.scrollLeft+=event.deltaY*(event.deltaMode===1?32:1);
  },{passive:false});
  // Scroll the selected tab into view only when the selection changes: the
  // browser status refresh runs every second and kept yanking the strip back
  // while the user scrolled it.
  function revealSelectedInspectionTab(){requestAnimationFrame(()=>{const strip=$("inspectionTabStrip");if(!state.inspectionOpen||!strip?.offsetWidth||!strip.getClientRects().length)return;const tab=strip.querySelector('[role="tab"][aria-selected="true"]');if(!tab)return;const key=JSON.stringify([{...tab.dataset},tab.title||tab.id||""]);if(key===state.revealedInspectionTab)return;state.revealedInspectionTab=key;tab.scrollIntoView({block:"nearest",inline:"nearest"});});}
  function conversationLayoutRestore(){const feed=$("conversationFeed");if(!feed||feed.hasAttribute('aria-busy'))return()=>{};const bookmark=conversationScrollBookmark(feed),key=conversationIdentity();return()=>requestAnimationFrame(()=>{if(key===conversationIdentity())restoreConversationScroll(bookmark,key);});}
  function inspectionWidthBounds(){
    const left=syncWorkspaceLeft(),available=innerWidth-left,stacked=innerWidth<=840||available<680,min=stacked?Math.min(280,available):280,conversationReserve=360,max=stacked?available:Math.max(min,available-conversationReserve);
    return{min,max:Math.min(1440,max)};
  }
  function applyInspectionWidth(value=state.inspectionWidth,persist=false,suppliedBounds=null){
    const bounds=suppliedBounds||inspectionWidthBounds(),width=Math.round(Math.max(bounds.min,Math.min(bounds.max,Number(value)||520)));state.inspectionWidth=width;document.documentElement.style.setProperty("--inspection-width",`${width}px`);if(persist)localStorage.setItem("phoenix-inspection-width",String(width));
    // Width writes happen once per animation frame while dragging. Offset and
    // composer measurements do not depend on every intermediate pixel and
    // forced extra layouts here were the main source of the rubber-band feel.
    if(document.body.classList.contains("inspection-resizing"))scheduleNativeBrowserBounds();
    else requestAnimationFrame(()=>{syncInspectionOverlayOffset();scheduleNativeBrowserBounds();syncComposerDensity();});
    return width;
  }
  function syncWorkspaceLeft(){
    const collapsed=document.body.classList.contains("sidebar-collapsed"),sidebar=document.querySelector(".company-sidebar"),sidebarRight=Math.max(0,Math.round(sidebar?.getBoundingClientRect().right||0)),left=document.documentElement.dataset.skin==="phoenix"?sidebarRight:collapsed||innerWidth<=760?0:sidebarRight;
    document.documentElement.style.setProperty("--workspace-left",`${left}px`);
    const stacked=state.inspectionOpen&&!state.inspectionExpanded&&(innerWidth<=840||innerWidth-left<680);
    if(document.body.classList.contains("inspection-stacked")!==stacked)document.body.classList.toggle("inspection-stacked",stacked);
    return left;
  }
  function syncPanelControlLocation(){const target=state.inspectionOpen?$("inspectionPanelControls"):$("stagePanelToggles");["terminalToggle","stageSidebarButton"].forEach((id)=>{const button=$(id);if(button&&target&&button.parentElement!==target)target.append(button);});const sidebarButton=$("stageSidebarButton"),expandButton=$("inspectionExpandButton"),open=state.inspectionOpen;if(expandButton)expandButton.hidden=!open;if(sidebarButton){sidebarButton.setAttribute("aria-label",open?"Close workspace sidebar":"Open workspace sidebar");sidebarButton.title=open?"Close sidebar (Ctrl+B)":"Open sidebar (Ctrl+B)";sidebarButton.setAttribute("aria-expanded",String(open));}}
  function applyInspectionExpanded(open=state.inspectionExpanded,persist=true){state.inspectionExpanded=Boolean(open);const active=state.inspectionOpen&&state.inspectionExpanded;syncWorkspaceLeft();document.body.classList.toggle("inspection-expanded",active);const button=$("inspectionExpandButton");if(button){button.setAttribute("aria-pressed",String(state.inspectionExpanded));button.setAttribute("aria-label",state.inspectionExpanded?"Restore workspace width":"Expand workspace");button.title=state.inspectionExpanded?"Restore workspace width":"Expand workspace";}if(persist){const key=inspectionExpandedStorageKey();if(key)try{localStorage.setItem(key,String(state.inspectionExpanded));}catch{}rememberInspectionShell();}requestAnimationFrame(()=>{scheduleNativeBrowserBounds();syncComposerDensity();requestAnimationFrame(scheduleNativeBrowserBounds);});}
  function toggleInspectionExpanded(){if(!state.inspectionOpen)return;const restore=conversationLayoutRestore();applyInspectionExpanded(!state.inspectionExpanded);restore();}
  function syncInspectionOverlayOffset(){
    const tabbar=document.querySelector(".inspection-tabbar"),toolbar=document.querySelector(".inspection-toolbar"),offset=(tabbar?.offsetHeight||40)+(toolbar?.offsetHeight||39);document.documentElement.style.setProperty("--inspection-browser-offset",`${offset}px`);
  }
  // The shell reports the icon Chromium actually resolved from the page's
  // <link rel="icon">. Guessing `${origin}/favicon.ico` is only a last resort:
  // most sites no longer serve that path, which is why tabs used to fall back
  // to the placeholder glyph for nearly everything.
  function browserFavicon(url,reported=""){
    if(window.__PHOENIX_ISOLATED_BACKEND__?.enabled)return /^data:image\//.test(reported)?reported:"";
    if(reported)return reported;
    try{const value=new URL(url);return /^https?:$/.test(value.protocol)?`${value.origin}/favicon.ico`:"";}catch{return"";}
  }
  // A tab with no site behind it (about:blank, a fresh tab) is OURS, so it
  // wears the Phoenix mark rather than a placeholder glyph. A real site that
  // simply has no favicon gets a neutral globe. Both are stroked SVGs on the
  // same grid as the rest of the chrome.
  const PHOENIX_TAB_MARK=()=>`<img class="tab-mark phoenix-raster-logo" data-phoenix-logo src="${escape(ui.phoenixLogoSource())}" alt="">`;
  const GLOBE_TAB_MARK='<svg class="tab-mark globe" viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="6.5"/><path d="M10 3.5c-3.6 4.3-3.6 8.7 0 13M10 3.5c3.6 4.3 3.6 8.7 0 13M4 7.9h12M4 12.1h12"/></svg>';
  function isBlankTabUrl(url){return !url||url==="about:blank"||String(url).startsWith("phoenix://");}
  function tabMark(url){return isBlankTabUrl(url)?PHOENIX_TAB_MARK():GLOBE_TAB_MARK;}
  // The mark is rendered up front and merely hidden while a favicon is in
  // flight; on error the img removes itself and reveals it. That avoids
  // round-tripping SVG markup through an inline onerror string.
  function tabIconMarkup(url,cls="browser-tab-fallback",reported=""){
    const favicon=browserFavicon(url,reported);
    const mark=`<span class="${cls}"${favicon?" hidden":""}>${tabMark(url)}</span>`;
    if(!favicon)return mark;
    return `${mark}<img class="summary-browser-icon" src="${escape(favicon)}" alt="" onerror="this.previousElementSibling.hidden=false;this.remove()">`;
  }
  function conversationSources(){
    const sources=[],seen=new Set(),push=(source)=>{const key=String(source.path||source.source||source.name||"");if(!key||seen.has(key))return;seen.add(key);sources.push(source);};
    $("conversationFeed").querySelectorAll(".user-message").forEach((row)=>(row._messageAttachments||[]).forEach((file)=>{if(isComposerImage(file))push({path:file.path||"",source:file.preview||"",name:file.name||"Screenshot",kind:"Screenshot"});}));
    $("conversationFeed").querySelectorAll("[data-relayed-image-path]").forEach((node)=>push({path:node.dataset.relayedImagePath||"",source:node._inspectionSource||node.querySelector("img")?.src||"",name:node.querySelector("span")?.textContent||"Conversation image",kind:"Image"}));
    $("conversationFeed").querySelectorAll("[data-inspect-image-path]").forEach((node)=>push({path:node.dataset.inspectImagePath||"",source:node._inspectionSource||node.querySelector("img")?.src||"",name:node.querySelector("small")?.textContent||"Generated image",kind:"Generated"}));
    // Everything the turn read on the web is a source too, so the panel shows
    // the same set the transcript does.
    $("conversationFeed").querySelectorAll(".web-search-results a[href]").forEach((node)=>push({path:node.href,url:node.href,source:"",name:node.title||node.querySelector("strong")?.textContent||node.href,kind:"Web"}));
    return sources;
  }
  const SUMMARY_ICONS=Object.freeze({
    changes:'<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="3.2" y="3.4" width="13.6" height="13.2" rx="2.8"/><path d="M10 6.7v6.6M6.7 10h6.6"/></svg>',
    folder:'<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3.1 6.2A1.7 1.7 0 0 1 4.8 4.5h3.7l1.7 1.8h5A1.7 1.7 0 0 1 17 8v6.8a1.7 1.7 0 0 1-1.7 1.7H4.8a1.7 1.7 0 0 1-1.7-1.7V6.2Z"/></svg>',
    branch:'<svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="6.2" cy="4.7" r="1.7"/><circle cx="6.2" cy="15.3" r="1.7"/><circle cx="13.8" cy="15.3" r="1.7"/><path d="M6.2 6.4v7.2M6.2 10h3.1a4.5 4.5 0 0 1 4.5 4.5"/></svg>',
    access:'<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="4" y="8.2" width="12" height="8.4" rx="2"/><path d="M7 8.2V6.3a3 3 0 0 1 6 0v1.9"/></svg>',
    commit:'<svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="3"/><path d="M3.2 10h3.7M13.1 10h3.7"/></svg>',
    pull:'<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M4.3 5.3h7.4a3.8 3.8 0 0 1 3.8 3.8v.5M10.7 14.7h-7.4a3.8 3.8 0 0 1-3.8-3.8v-.5"/><path d="m12.8 7.2 2.7 2.7-2.7 2.7M7.2 12.8l-2.7-2.7 2.7-2.7"/></svg>',
    terminal:'<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="2.8" y="3.4" width="14.4" height="13.2" rx="2.7"/><path d="m6 7 2.8 3-2.8 3M10.8 13h3.4"/></svg>',
    refresh:'<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M15.7 8.4A6.1 6.1 0 1 0 16 11.1"/><path d="M15.7 4.5v3.9h-3.9"/></svg>',
    chevron:'<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m7.2 4.8 5.6 5.2-5.6 5.2"/></svg>',
  });
  function summaryHandoffs(){return[...$("conversationFeed").querySelectorAll(".handoff-chain")].filter((row)=>{
    if(isEphemeralVolumeAgent(row.dataset.handoffTo)||isEphemeralVolumeAgent(row.dataset.handoffFrom))return false;
    const live=Boolean(row.closest(".work-cluster")?.classList.contains("live"));
    return handoffIsSettled(row)||live||(row.dataset.background==="true"&&row.dataset.handoffState==="working");
  }).slice(-8).reverse();}
  function summarySubagents(){
    const rows=new Map(),rank={working:4,queued:3,blocked:2,done:1};
    const remember=(id,patch={})=>{
      const key=String(id||"").trim();if(!key)return;
      const current=rows.get(key),status=patch.status||current?.status||"queued";
      if(current&&rank[status]<rank[current.status])return;
      const profile=patch.profile||current?.profile||agentProfile(key),label=patch.label||current?.label||agentLabel(key);
      rows.set(key,{id:key,profile,label,status,task:patch.task||current?.task||"",ephemeral:Boolean(patch.ephemeral||current?.ephemeral)});
    };
    const activity=ui.activityFor?.(state.item),owner=state.item?.kind==="agent"?canonicalAgentId(state.item.id):"";
    (activity?.active_agent_ids||[]).forEach((id)=>{if(owner&&canonicalAgentId(id)===owner)return;remember(id,{status:"working"});});
    summaryHandoffs().forEach((row)=>remember(row.dataset.handoffTo,{status:row.dataset.handoffState||"queued",task:row.querySelector(".handoff-task")?.textContent||""}));
    state.ephemeralWorkers.forEach((worker,id)=>{
      if(worker.conversationKey&&worker.conversationKey!==conversationKeyOf(state.item))return;
      if(!["queued","working","running"].includes(worker.status))return;
      remember(`volume:${id}`,{label:worker.label||"Worker",status:worker.status==="running"?"working":worker.status,task:worker.task||"",ephemeral:true,profile:{agent_id:`volume:${id}`,display_name:worker.label||"Worker",color:"#8d7bc4",icon_seed:`volume-${id}`,metadata_json:"{}"}});
    });
    return [...rows.values()].sort((left,right)=>(rank[right.status]||0)-(rank[left.status]||0)||left.label.localeCompare(right.label));
  }
  function summaryBackgroundProcesses(){
    // The selected conversation's own trace is foreground work. Only a trace
    // explicitly marked as detached/background belongs in this section.
    const background=[...$("conversationFeed").querySelectorAll('.group-work-cluster.live[data-background="true"],.work-cluster.live[data-background="true"]')]
      .filter((row)=>!isEphemeralVolumeAgent(row.dataset.agent))
      .map((row)=>({kind:"agent",key:`agent:${row.dataset.agent||"phoenix"}`,name:agentLabel(row.dataset.agent||"phoenix"),detail:"Working in the background",live:true}));
    const terminals=state.terminalProcesses.filter((row)=>!row.exited&&(!row.conversationKey||row.conversationKey===conversationKeyOf(state.item))).map((row)=>({kind:"terminal",key:row.key,name:workspaceName(row.cwd)||"Terminal",detail:row.cwd||"Local shell",live:true}));
    return [...terminals,...background];
  }
  function environmentFallback(){
    const review={},workspace=state.workspace||"";
    return{workspace,workspaceName:workspaceName(workspace)||"Choose workspace",repo:null,branch:null,detached:false,remote:null,upstream:null,changes:Number(review.changes?.length)||0,additions:Number(review.additions)||0,deletions:Number(review.deletions)||0,canCommit:false,canPush:false,canCreatePullRequest:false,ghAvailable:false};
  }
  function environmentSnapshot(){
    const snapshot=state.environmentSnapshot,workspace=String(state.workspace||"").replace(/[\\/]+$/,""),known=String(snapshot?.workspace||"").replace(/[\\/]+$/,"");
    return snapshot&&(!workspace||workspace===known)?snapshot:environmentFallback();
  }
  function environmentPermissionLabel(){
    const name=workspaceName(state.workspace);
    if(state.permission==="full_access")return name?`Full access · ${name}`:"Full access";
    if(state.permission==="workspace")return name?`Workspace · ${name}`:"Workspace";
    return "Talk";
  }
  function environmentPermissionNote(){
    if(state.permission==="full_access")return "Workspace is the default cwd; absolute and outside paths are allowed.";
    if(state.permission==="workspace")return "Workspace-confined tools";
    return "Conversation only";
  }
  function previewEnvironmentSnapshot(){
    const fallback=environmentFallback();return{...fallback,repo:state.workspace||"/workspace",branch:"main",remote:"git@github.com:phoenix/preview.git",upstream:"origin/main",canCommit:Boolean(fallback.changes),canPush:true,canCreatePullRequest:true,ghAvailable:true};
  }
  async function refreshEnvironmentSnapshot(force=false){
    if(state.environmentLoading&&!force)return;
    const workspace=state.workspace;if(!workspace){state.environmentSnapshot=null;state.environmentLoading=false;if(state.summaryOpen)syncActivitySummary();return;}
    const request=++state.environmentRequest;state.environmentLoading=true;if(state.summaryOpen)syncActivitySummary();
    try{const snapshot=preview?previewEnvironmentSnapshot():await ui.invoke("workspace_environment",{workspace});if(request!==state.environmentRequest)return;state.environmentSnapshot=snapshot;}
    catch(error){if(request===state.environmentRequest)state.environmentSnapshot={...environmentFallback(),workspace,error:String(error?.message||error)};}
    finally{if(request===state.environmentRequest){state.environmentLoading=false;if(state.summaryOpen)syncActivitySummary();}}
  }
  // Volume workers intentionally do not become durable handoff transcript
  // rows. Their raw lifecycle signal drives only this live compact presence,
  // then is discarded as soon as the worker cleans up, fails, or is cancelled.
  function consumeVolumeWorkerLifecycle(value){
    const event=value?.VolumeWorkerLifecycle||(value?.Story?.kind==="subagent_lifecycle"?value.Story:null)||(value?.StoryReplay?.kind==="subagent_lifecycle"?value.StoryReplay:null);if(!event?.worker_id)return false;
    const id=`${event.batch_id||"batch"}:${event.worker_id}`,status=String(event.status||"").toLowerCase();
    if(status==="started")state.ephemeralWorkers.set(id,{id,workerId:event.worker_id,label:event.label||"Worker",task:event.item_id||"",status:"working",conversationKey:conversationKeyOf(state.item),turnId:state.activeTurnId});
    else state.ephemeralWorkers.delete(id);
    syncTeamPresence();syncActivitySummary();return true;
  }
  function clearEphemeralWorkersForConversation(key){
    state.ephemeralWorkers.forEach((worker,id)=>{if(worker.conversationKey===key)state.ephemeralWorkers.delete(id);});
  }
  // A web source reads as its host ("anthropic.com/news"); a local artefact
  // reads as its filename. Either way one line, never a wrapped block.
  function sourceLabel(source){
    if(source.url){try{const value=new URL(source.url);return `${value.host.replace(/^www\./,"")}${value.pathname==="/"?"":value.pathname}`;}catch{}}
    const supplied=String(source.name||"").trim(),fallback=String(source.path||"").split("/").filter(Boolean).at(-1)||"Conversation image";
    return /[*?]/.test(supplied)||!supplied?fallback:supplied;
  }
  function summarySourceMark(source){
    if(source.url)return tabIconMarkup(source.url,"summary-source-icon");
    if(source.source)return `<img class="summary-source-thumb" src="${escape(source.source)}" alt="">`;
    // No data URL yet — render a placeholder that resolveSummaryThumbnails()
    // fills in from the path, so a source never sits there as a dead label.
    return `<span class="summary-source-icon" data-thumb-path="${escape(source.path||"")}"><svg viewBox="0 0 20 20" aria-hidden="true"><rect x="3" y="4" width="14" height="12" rx="2.5"/><circle cx="7.6" cy="8.4" r="1.2"/><path d="m3.4 14.4 4.2-3.7L11 13.4l2.4-2 3.2 2.6"/></svg></span>`;
  }
  // The feed caches a data URL on each image node once it loads, but a source
  // can reach the summary before that (replay, a fresh panel open). Resolve
  // those few by path so the thumbnails actually appear instead of staying
  // as generic placeholders.
  function resolveSummaryThumbnails(){
    $("activitySummaryRows")?.querySelectorAll("[data-thumb-path]").forEach(async(node)=>{
      const path=node.dataset.thumbPath;if(!path||node.dataset.thumbLoading)return;node.dataset.thumbLoading="1";
      try{
        const source=await ui.invoke("image_data_url",{path});
        if(!source||!node.isConnected)return;
        const image=document.createElement("img");image.className="summary-source-thumb";image.alt="";image.src=source;node.replaceWith(image);
      }catch{node.removeAttribute("data-thumb-path");}
    });
  }
  async function runEnvironmentAction(command,args,success="Environment updated."){
    if(state.environmentAction)return null;
    state.environmentAction=command;syncActivitySummary();
    try{
      const receipt=preview?{message:success,url:null}:await ui.invoke(command,args);
      ui.closeModal("commit");ui.closeLayers();
      await refreshEnvironmentSnapshot(true);
      ui.toast(receipt?.url?`${receipt.message||success} ${receipt.url}`:receipt?.message||success);
      return receipt;
    }catch(error){ui.toast(error?.message||String(error),true);return null;}
    finally{state.environmentAction="";if(state.summaryOpen)syncActivitySummary();}
  }
  async function openEnvironmentBranchPicker(anchor){
    if(!state.workspace){pickWorkspace();return;}
    const pop=ui.openPopover(anchor,'<div class="popover-label">Local branch</div><div class="popover-empty">Reading local branches…</div>',"environment-branch-picker",{align:"start"});
    try{
      const branches=preview?{current:"main",branches:["main","feature/environment"]}:await ui.invoke("workspace_git_branches",{workspace:state.workspace||null});
      if(!pop.isConnected)return;
      const rows=(branches.branches||[]).map((branch)=>`<button type="button" class="choice-row ${branch===branches.current?"selected":""}" data-environment-branch="${escape(branch)}"><span><strong>${escape(branch)}</strong><small>${branch===branches.current?"Current branch":"Switch workspace branch"}</small></span>${branch===branches.current?icons.check:""}</button>`).join("");
      pop.innerHTML=`<div class="popover-label">Local branch</div>${rows||'<div class="popover-empty">No local branch is available.</div>'}`;
      pop.onclick=(event)=>{const branch=event.target.closest("[data-environment-branch]")?.dataset.environmentBranch;if(!branch||branch===branches.current)return;pop.remove();openEnvironmentBranchConfirmation(branch,branches.current);};
    }catch(error){if(pop.isConnected)pop.innerHTML=`<div class="popover-label">Local branch</div><div class="popover-empty">${escape(error?.message||String(error))}</div>`;}
  }
  function openEnvironmentBranchConfirmation(branch,current){
    ui.showModal(`<section class="modal environment-action-modal" role="dialog" aria-modal="true" aria-labelledby="environmentBranchTitle"><header class="modal-header"><span><strong id="environmentBranchTitle">Switch to ${escape(branch)}?</strong><small>The workspace will move from ${escape(current||"the current branch")} to this local branch.</small></span><button class="modal-close" type="button" aria-label="Cancel">×</button></header><div class="modal-body"><p class="environment-action-note">Uncommitted work remains in the workspace and Git may refuse the switch if it would overwrite a file.</p></div><footer class="modal-footer"><button class="button secondary" type="button" data-environment-cancel>Cancel</button><button class="button primary" type="button" data-environment-confirm-branch>Switch branch</button></footer></section>`);
    const layer=$("modalLayer");layer.querySelector("[data-environment-cancel]").onclick=ui.closeModal;layer.querySelector("[data-environment-confirm-branch]").onclick=()=>runEnvironmentAction("workspace_git_switch_branch",{workspace:state.workspace||null,branch},`Switched to ${branch}.`);
  }
  function openEnvironmentCommit(){
    const environment=environmentSnapshot();
    ui.showModal(`<section class="modal environment-action-modal" role="dialog" aria-modal="true" aria-labelledby="environmentCommitTitle"><header class="modal-header"><span><strong id="environmentCommitTitle">Commit workspace changes</strong><small>${escape(environment.workspaceName||workspaceName(state.workspace)||"Workspace")} stays the default working folder.</small></span><button class="modal-close" type="button" aria-label="Cancel">×</button></header><form id="environmentCommitForm" class="modal-body"><label class="field"><span>Commit message</span><input name="message" required maxlength="500" autofocus placeholder="Describe the completed work"></label><label class="ember-check-label environment-stage-all"><input class="ember-check-input" name="stageAll" type="checkbox"><i class="ember-check-box" aria-hidden="true"></i><span>Stage all workspace changes before committing</span></label><p class="environment-action-note">Only staged files are committed unless you explicitly choose to stage this workspace’s changes.</p></form><footer class="modal-footer"><button class="button secondary" type="button" data-environment-cancel>Cancel</button><button class="button primary" form="environmentCommitForm" type="submit">Commit</button></footer></section>`);
    const form=$("environmentCommitForm");$("modalLayer").querySelector("[data-environment-cancel]").onclick=ui.closeModal;form.onsubmit=(event)=>{event.preventDefault();const data=new FormData(form);runEnvironmentAction("workspace_git_commit",{workspace:state.workspace||null,message:String(data.get("message")||""),stageAll:data.get("stageAll")==="on"},"Committed workspace changes.");};
  }
  function confirmEnvironmentPush(){
    const environment=environmentSnapshot(),branch=environment.branch||"this branch";
    ui.showModal(`<section class="modal environment-action-modal" role="dialog" aria-modal="true" aria-labelledby="environmentPushTitle"><header class="modal-header"><span><strong id="environmentPushTitle">Push ${escape(branch)}?</strong><small>This sends the current workspace branch to its configured remote.</small></span><button class="modal-close" type="button" aria-label="Cancel">×</button></header><div class="modal-body"><p class="environment-action-note">Phoenix will use the existing upstream, or create one on the selected remote for this branch.</p></div><footer class="modal-footer"><button class="button secondary" type="button" data-environment-cancel>Cancel</button><button class="button primary" type="button" data-environment-confirm-push>Push branch</button></footer></section>`);
    const layer=$("modalLayer");layer.querySelector("[data-environment-cancel]").onclick=ui.closeModal;layer.querySelector("[data-environment-confirm-push]").onclick=()=>runEnvironmentAction("workspace_git_push",{workspace:state.workspace||null},`Pushed ${branch}.`);
  }
  function openEnvironmentGitActions(anchor){
    const environment=environmentSnapshot(),busy=Boolean(state.environmentAction),pop=ui.openPopover(anchor,`<div class="popover-label">Git actions</div><button type="button" class="choice-row" data-environment-git="commit" ${!environment.canCommit||busy?"disabled":""}><span><strong>Commit changes</strong><small>${environment.canCommit?"Choose a message and stage intentionally":"No changes in this Git workspace"}</small></span>${SUMMARY_ICONS.commit}</button><button type="button" class="choice-row" data-environment-git="push" ${!environment.canPush||busy?"disabled":""}><span><strong>Push current branch</strong><small>${environment.canPush?environment.upstream||"Set the branch upstream as it pushes":"Add a remote and select a branch first"}</small></span>${SUMMARY_ICONS.pull}</button>`,"environment-git-picker",{align:"start"});
    pop.onclick=(event)=>{const action=event.target.closest("[data-environment-git]")?.dataset.environmentGit;if(action==="commit")openEnvironmentCommit();if(action==="push")confirmEnvironmentPush();};
  }
  function openEnvironmentPullRequest(){
    const environment=environmentSnapshot(),branch=environment.branch||"current branch";
    ui.showModal(`<section class="modal environment-action-modal" role="dialog" aria-modal="true" aria-labelledby="environmentPullTitle"><header class="modal-header"><span><strong id="environmentPullTitle">Create pull request</strong><small>${escape(branch)} is pushed to ${escape(environment.upstream||"its remote")}. GitHub CLI creates the request without opening a mock surface.</small></span><button class="modal-close" type="button" aria-label="Cancel">×</button></header><form id="environmentPullForm" class="modal-body"><label class="field"><span>Title</span><input name="title" required maxlength="500" autofocus placeholder="Describe this change"></label><label class="field"><span>Body <small>optional</small></span><textarea name="body" rows="4" maxlength="500" placeholder="What changed and how it was verified"></textarea></label><label class="ember-check-label"><input class="ember-check-input" name="draft" type="checkbox"><i class="ember-check-box" aria-hidden="true"></i><span>Create as draft</span></label></form><footer class="modal-footer"><button class="button secondary" type="button" data-environment-cancel>Cancel</button><button class="button primary" form="environmentPullForm" type="submit">Create pull request</button></footer></section>`);
    const form=$("environmentPullForm"),layer=$("modalLayer");layer.querySelector("[data-environment-cancel]").onclick=ui.closeModal;form.onsubmit=(event)=>{event.preventDefault();const data=new FormData(form);runEnvironmentAction("workspace_git_create_pull_request",{workspace:state.workspace||null,title:String(data.get("title")||""),body:String(data.get("body")||""),draft:data.get("draft")==="on"},"Created pull request.");};
  }
  function bindActivitySummaryActions(){
    const host=$("activitySummaryRows");if(!host)return;
    host.querySelector("[data-summary-environment-refresh]")?.addEventListener("click",()=>refreshEnvironmentSnapshot(true));
    host.querySelector("[data-summary-workspace]")?.addEventListener("click",pickWorkspace);
    host.querySelector("[data-summary-branch]")?.addEventListener("click",(event)=>openEnvironmentBranchPicker(event.currentTarget));
    host.querySelector("[data-summary-permission]")?.addEventListener("click",openPermission);
    host.querySelector("[data-summary-git]")?.addEventListener("click",(event)=>openEnvironmentGitActions(event.currentTarget));
    host.querySelector("[data-summary-pull-request]")?.addEventListener("click",openEnvironmentPullRequest);
    host.querySelector("[data-summary-agent-fold]")?.addEventListener("click",()=>{state.summaryAgentsFolded=!state.summaryAgentsFolded;localStorage.setItem("phoenix-summary-subagents-folded",String(state.summaryAgentsFolded));syncActivitySummary();});
    host.querySelectorAll("[data-summary-agent]").forEach((button)=>button.addEventListener("click",()=>{const agent=summarySubagents()[Number(button.dataset.summaryAgent)];if(agent&&!agent.ephemeral)ui.selectItem?.({kind:"agent",id:agent.id});}));
    host.querySelectorAll("[data-summary-process]").forEach((button)=>button.addEventListener("click",()=>{const process=summaryBackgroundProcesses()[Number(button.dataset.summaryProcess)];if(process?.kind==="terminal")window.PhoenixView?.toggleTerminal?.(true);else if(process?.kind==="agent")$("conversationFeed").querySelector(`[data-agent="${CSS.escape(process.key.slice(6))}"]`)?.scrollIntoView({block:"center"});}));
    host.querySelector("[data-summary-browser-new]")?.addEventListener("click",()=>$("browserNewTab").click());
    host.querySelector("[data-summary-view-all]")?.addEventListener("click",openSourcesModal);
    host.querySelectorAll("[data-summary-browser-tab]").forEach((button)=>button.addEventListener("click",()=>{showInspectionSidebar("browser");selectInspectionBrowserTab(button.dataset.summaryBrowserTab);}));
    host.querySelectorAll("[data-summary-source]").forEach((button)=>button.addEventListener("click",()=>{const source=conversationSources()[Number(button.dataset.summarySource)];if(!source)return;if(source.url)openBrowser(state.item?.kind==="agent"?state.item.id:"phoenix","browse",source.url);else openImageInspector(source);}));
  }
  function toggleActivitySummary(force,persist=true){
    const open=force??!state.summaryOpen;state.summaryOpen=Boolean(open);const summary=$("activitySummary");if(summary){summary.hidden=false;summary.dataset.summaryFolded=String(!state.summaryOpen);summary.setAttribute("aria-hidden",String(!state.summaryOpen));summary.toggleAttribute("inert",!state.summaryOpen);}
    document.body.classList.toggle("summary-open",state.summaryOpen);$("stageInspectButton")?.setAttribute("aria-expanded",String(state.summaryOpen));
    const fold=$("activitySummaryOpen");if(fold){fold.setAttribute("aria-label","Fold environment summary");fold.title="Fold away";}
    if(persist)localStorage.setItem("phoenix-summary-popover-open",String(state.summaryOpen));
    if(state.summaryOpen){syncActivitySummary();refreshEnvironmentSnapshot();}requestAnimationFrame(syncComposerDensity);
  }
  function syncActivitySummary(){
    const summary=$("activitySummary");if(!summary)return;
    if(!state.summaryOpen){$("inspectionSourcesCount").textContent=String(conversationSources().length);return;}
    const environment=environmentSnapshot(),subagents=summarySubagents(),processes=summaryBackgroundProcesses(),sources=conversationSources();
    const tabs=state.browserTabs.length?state.browserTabs:(state.browserBoundKey?[{id:"current",url:state.browserFrameUrl||$("browserAddress")?.value||"about:blank",title:"",active:true}]:[]),busy=Boolean(state.environmentAction);
    $("activitySummaryTitle").textContent="Environment";$("activitySummarySubtitle").textContent=environment.workspaceName||workspaceName(state.workspace)||"Workspace";
    const section=(label,count,body,action="")=>`<section class="activity-summary-section"><header><strong>${escape(label)}</strong>${action}${count!=null?`<small>${escape(count)}</small>`:""}</header>${body||'<div class="summary-empty">None</div>'}</section>`;
    const changes=Number(environment.changes)||0,additions=Number(environment.additions)||0,deletions=Number(environment.deletions)||0;
    const changeStat=changes?`<span class="summary-change-stat"><b>+${additions}</b><i>−${deletions}</i></span>`:'<span class="summary-environment-muted">Clean</span>';
    const environmentRows=`<section class="activity-summary-section summary-environment-section">
      <div class="summary-environment-row summary-environment-static"><span class="summary-environment-icon">${SUMMARY_ICONS.changes}</span><span><strong>Changes</strong><small>${changes?`${changes} changed file${changes===1?"":"s"}`:"No workspace changes"}</small></span>${changeStat}</div>
      <button type="button" class="summary-environment-row" data-summary-workspace><span class="summary-environment-icon">${SUMMARY_ICONS.folder}</span><span><strong>Local</strong><small title="${escape(environment.workspace||state.workspace||"")}">${escape(environment.workspaceName||workspaceName(state.workspace)||"Choose workspace")}</small></span><i class="summary-environment-chevron">${SUMMARY_ICONS.chevron}</i></button>
      ${environment.repo?`<button type="button" class="summary-environment-row summary-environment-branch" data-summary-branch ${busy?"disabled":""}><span class="summary-environment-icon">${SUMMARY_ICONS.branch}</span><span><strong>${escape(environment.branch||"Detached HEAD")}</strong><small>${environment.detached?"Choose a branch before Git actions":"Local branch"}</small></span><i class="summary-environment-chevron">${SUMMARY_ICONS.chevron}</i></button>`:""}
      <button type="button" class="summary-environment-row summary-environment-access" data-summary-permission><span class="summary-environment-icon">${SUMMARY_ICONS.access}</span><span><strong>${escape(environmentPermissionLabel())}</strong><small>${escape(environmentPermissionNote())}</small></span><i class="summary-environment-chevron">${SUMMARY_ICONS.chevron}</i></button>
      <button type="button" class="summary-environment-row summary-environment-action" data-summary-git ${!environment.repo||busy?"disabled":""} title="${escape(environment.repo?"Commit staged work or push the current branch":"Choose a Git workspace first")}"><span class="summary-environment-icon">${SUMMARY_ICONS.commit}</span><span><strong>Commit or push</strong><small>${busy?"Working…":environment.canCommit?"Changes are ready to review":"Git actions for this workspace"}</small></span><i class="summary-environment-chevron">${SUMMARY_ICONS.chevron}</i></button>
      <button type="button" class="summary-environment-row summary-environment-action" data-summary-pull-request ${!environment.canCreatePullRequest||busy?"disabled":""} title="${escape(environment.canCreatePullRequest?"Create a GitHub pull request from the pushed branch":"Push a GitHub branch first, then create a pull request")}"><span class="summary-environment-icon">${SUMMARY_ICONS.pull}</span><span><strong>Create pull request</strong><small>${environment.canCreatePullRequest?"GitHub CLI is ready":environment.ghAvailable?"Push a GitHub branch first":"GitHub CLI is unavailable"}</small></span><i class="summary-environment-chevron">${SUMMARY_ICONS.chevron}</i></button>
      ${environment.error?`<p class="summary-environment-error">${escape(environment.error)}</p>`:""}
    </section>`;
    const working=subagents.filter((agent)=>["working","queued"].includes(agent.status)).length;
    const agentRows=subagents.map((agent,index)=>`<button type="button" class="agent-metal-chip summary-agent-chip ${agent.status==="done"?"complete":agent.status==="blocked"?"failed":""}" data-summary-agent="${index}" style="--agent-chip:${escape(agent.profile?.color||"#77736d")}" title="${escape(agent.task?`${agent.label}: ${agent.task}`:agent.label)}"><span class="agent-chip-avatar">${ui.avatarSvg(agent.profile)}</span><strong>${escape(agent.label)}</strong><i class="summary-agent-state" data-state="${escape(agent.status)}" aria-label="${escape(handoffStateLabel(agent.status))}"></i></button>`).join("");
    const agentGridHeight=Math.max(28,Math.ceil(subagents.length/2)*32);const agentsBody=subagents.length?`<div class="summary-agent-fold ${state.summaryAgentsFolded&&subagents.length>2?"folded":""}" style="--summary-agent-expanded:${agentGridHeight}px"><div class="summary-agent-grid">${agentRows}</div><div class="summary-agent-fold-foot"><span>${working?`${working} working`:"All settled"}</span>${subagents.length>2?`<button type="button" data-summary-agent-fold aria-expanded="${String(!state.summaryAgentsFolded)}">${state.summaryAgentsFolded?`Show ${subagents.length-2} more`:"Fold away"}</button>`:""}</div></div>`:'<div class="summary-empty">No workers active.</div>';
    const processRows=processes.slice(0,6).map((process,index)=>`<button type="button" class="summary-entry summary-process-entry" data-summary-process="${index}" title="${escape(process.detail)}"><span class="summary-process-icon">${SUMMARY_ICONS.terminal}</span><span><strong>${escape(process.name)}</strong><small>${escape(process.detail)}</small></span><em>Live</em></button>`).join("");
    const browserRows=tabs.map((tab,index)=>{const url=tab.url||"about:blank",title=isBlankTabUrl(url)?"New tab":tab.title||hostLabel(url);return`<button type="button" class="summary-entry" data-summary-browser-tab="${escape(tab.id||String(index))}">${tabIconMarkup(url,"summary-browser-fallback",tab.favicon||"")}<span><strong>${escape(title||"New tab")}</strong><small>${escape(hostLabel(url)||"New tab")}</small></span><em>${tab.active?"Open":""}</em></button>`;}).join("");
    const sourceRows=sources.slice(0,3).map((source,index)=>`<button type="button" class="summary-entry summary-source-entry" data-summary-source="${index}" title="${escape(source.name)}">${summarySourceMark(source)}<span><strong>${escape(sourceLabel(source))}</strong></span></button>`).join("");
    $("inspectionSourcesCount").textContent=String(sources.length);
    $("activitySummaryRows").innerHTML=environmentRows+section("Subagents",working?`${working} working`:subagents.length||null,agentsBody)+section("Background processes",processes.length?String(processes.length):null,processRows)+(tabs.length?section("Browser",String(tabs.length),browserRows,'<button type="button" data-summary-browser-new aria-label="New browser tab" title="New browser tab"><svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 5.2v9.6M5.2 10h9.6"/></svg></button>'):"")+section("Sources",sources.length?String(sources.length):null,sourceRows+(sources.length>3?'<button type="button" class="summary-view-all" data-summary-view-all>View all</button>':""));
    bindActivitySummaryActions();resolveSummaryThumbnails();
  }
  function renderInspectionBrowserTabs(surface=null){
    const host=$("inspectionBrowserTabs"),rawTabs=Array.isArray(surface?.tabs)&&surface.tabs.length?surface.tabs:[state.browserBoundKey?{id:"current",url:surface?.url||state.browserFrameUrl||"about:blank",title:"",active:true}:null].filter(Boolean);
    const tabs=rawTabs.map((tab,index)=>{const id=String(tab.id||index),cacheKey=`${state.browserOwnerId||state.browserBoundKey}:${id}`,reported=String(tab.favicon||"");if(reported)state.browserFavicons.set(cacheKey,reported);return{...tab,id,favicon:reported||state.browserFavicons.get(cacheKey)||""};});
    state.browserTabs=tabs.map((tab)=>({...tab}));
    // Status is polled while the native surface is open. Replacing innerHTML
    // on every identical response destroys and reloads every favicon, making
    // the whole tab strip flash once a second. Rebuild only when tab identity,
    // title, URL, active state, or the resolved icon actually changes.
    const signature=JSON.stringify([state.item?.kind||"",state.browserOwnerAgentId||"",state.browserOwnerId||"",state.browserBoundKey||"",tabs.map((tab)=>[tab.id,tab.url||"",tab.title||"",tab.active!==false,tab.favicon||""])]);
    if(signature!==state.browserTabRenderSignature){
      state.browserTabRenderSignature=signature;
      host.innerHTML=tabs.map((tab)=>{const title=isBlankTabUrl(tab.url)?"New tab":String(tab.title||hostLabel(tab.url)||"New tab"),active=tab.active!==false&&(tabs.length===1||tab.active);return`<button type="button" role="tab" class="inspection-tab browser-page-tab" data-inspection-tab="browser" data-browser-tab-id="${escape(tab.id)}" aria-selected="${String(state.inspectionTab==="browser"&&active)}" tabindex="${state.inspectionTab==="browser"&&active?"0":"-1"}">${tabIconMarkup(tab.url,"browser-tab-fallback",tab.favicon||"")}<strong${active?' id="browserTabTitle"':''}>${escape(title)}</strong><span class="browser-tab-close" data-close-browser-tab aria-label="Close tab">×</span></button>`;}).join("");
      if(tabs.length&&state.item?.kind==="group"){const owner=ownerProfile(),name=owner?.display_name||agentLabel(state.browserOwnerAgentId||"phoenix"),first=String(name).split(/\s+/)[0];host.insertAdjacentHTML("afterbegin",`<span class="browser-owner-chip" title="${escape(name)}'s own browser, with their own logins">${ui.avatarSvg(owner||{agent_id:state.browserOwnerAgentId||"phoenix",display_name:name})}<span>${escape(first)}'s browser</span></span>`);}
      if(!tabs.length)host.innerHTML=`<button id="inspectionBrowserTab" type="button" role="tab" class="inspection-tab browser-page-tab" data-inspection-tab="browser" aria-selected="false" tabindex="-1" hidden>${tabIconMarkup("")}<strong id="browserTabTitle">New tab</strong></button>`;
    }
    const active=tabs.find((tab)=>tab.active)||tabs[0],activeUrl=active?.url||surface?.url||"";if(activeUrl)syncBrowserAddress(activeUrl);rememberInspectionBrowser();revealSelectedInspectionTab();syncActivitySummary();
  }
  async function inspectBrowserSurface(instance){if(window.__PHOENIX_CHROMIUM_SHELL__?.enabled===true)return ui.invoke("browser_surface_status",{instance});const value=await rpc({BrowserSurface:{instance,action:"status"}},4000);return value.BrowserSurface;}
  async function refreshInspectionBrowserTabs(){const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration,boundKey=state.browserBoundKey;if(!instance||boundKey!==conversationKeyOf(state.item))return null;const surface=await inspectBrowserSurface(instance);if(instance!==state.browserOwnerId||generation!==state.browserSurfaceGeneration||boundKey!==state.browserBoundKey||boundKey!==conversationKeyOf(state.item))return null;renderInspectionBrowserTabs(surface);return surface;}
  async function selectInspectionBrowserTab(tabId){if(!tabId)return;try{if(state.browserBoundKey!==conversationKeyOf(state.item))await resumeInspectionBrowser();if(tabId==="restore")return;if(state.browserBoundKey!==conversationKeyOf(state.item))throw new Error("This conversation has no open browser.");if(state.browserTabs.find((tab)=>tab.id===tabId)?.active)return;// The tabs live in the desktop's own browser surface, so switch them
// there. Going through the gateway waited on the agent's browser lock and
// refused the click whenever the agent was mid-action.
let switched=false;try{const status=await ui.invoke("browser_surface_switch_tab",{instance:state.browserOwnerId,targetId:tabId});switched=status?.supported!==false&&!status?.reason;}catch{}
if(!switched)await browserCommand({action:"switch_tab",tab_id:tabId});await refreshInspectionBrowserTabs();}catch(error){ui.toast(error.message||String(error),true);}}
  async function closeInspectionBrowserTab(tabId){
    if(!tabId||state.closingBrowserTabs.has(tabId))return;
    const boundKey=state.browserBoundKey,previous=state.browserTabs.map((tab)=>({...tab}));
    try{
      if(boundKey!==conversationKeyOf(state.item))await resumeInspectionBrowser();
      if(state.browserBoundKey!==conversationKeyOf(state.item))throw new Error("This conversation has no open browser.");
      if(state.browserTabs.length<=1){await cancelBrowserFlow();return;}
      state.closingBrowserTabs.add(tabId);
      const optimistic=state.browserTabs.filter((tab)=>tab.id!==tabId);
      if(!optimistic.some((tab)=>tab.active)&&optimistic.length)optimistic[0].active=true;
      renderInspectionBrowserTabs({tabs:optimistic});
      await browserCommand({action:"close_tab",tab_id:tabId});
      // A second close may already be queued. Do not let this earlier status
      // response briefly resurrect that tab; the last close performs the one
      // authoritative refresh after the command queue drains.
      if(state.closingBrowserTabs.size===1)await refreshInspectionBrowserTabs();
    }catch(error){
      if(state.browserBoundKey===boundKey)renderInspectionBrowserTabs({tabs:previous});
      ui.toast(error.message||String(error),true);
    }finally{state.closingBrowserTabs.delete(tabId);}
  }
  function syncInspectionHeader(){
    syncActivitySummary();applyInspectionWidth();syncWorkspaceLeft();syncInspectionOverlayOffset();
  }
  function desktopViewerOwners(){
    let profiles=[];
    if(state.item?.kind==="agent"){
      const profile=knownAgentProfile(state.item.id)||agentProfile(state.item.id);if(profile)profiles=[profile];
    }else if(state.item?.kind==="group"){
      const members=(ui.state.view?.directory.members||[]).filter((member)=>member.group_id===state.item.id).sort((a,b)=>(a.sort_order??0)-(b.sort_order??0));
      profiles=members.map((member)=>knownAgentProfile(member.agent_id)||agentProfile(member.agent_id)).filter(Boolean);
    }else{
      const profile=knownAgentProfile("phoenix")||agentProfile("phoenix");if(profile)profiles=[profile];
    }
    return profiles.map((profile)=>({id:canonicalAgentId(profile.agent_id),label:profile.display_name||agentLabel(profile.agent_id),aliases:[profile.agent_id,profile.internal_role,profile.display_name].filter(Boolean)}));
  }
  function updateDesktopViewer(visible){
    window.PhoenixDesktopViewer?.update({visible:Boolean(visible),context:conversationKeyOf(state.item),owners:desktopViewerOwners(),rpc});
  }
  function setInspectionTab(next){
    const tab=INSPECTION_TABS.has(next)?next:"desktop";state.inspectionTab=tab;
    window.dispatchEvent(new CustomEvent("phoenix:inspection-tab",{detail:{tab}}));
    if(tab!=="image"&&state.imageCommentMode)setImageCommentMode(false);
    document.querySelectorAll("[data-inspection-tab]").forEach((button)=>{const browserTab=button.dataset.browserTabId,imageTab=button.dataset.imageTabId,selected=button.dataset.inspectionTab===tab&&(!browserTab||state.browserTabs.find((item)=>item.id===browserTab)?.active)&&(!imageTab||imageTab===state.activeInspectionImageId);button.setAttribute("aria-selected",String(Boolean(selected)));button.setAttribute("tabindex",selected?"0":"-1");});
    document.querySelectorAll("[data-inspection-panel]").forEach((panel)=>{panel.hidden=panel.dataset.inspectionPanel!==tab;});
    document.querySelectorAll("[data-inspection-toolbar]").forEach((toolbar)=>{toolbar.hidden=toolbar.dataset.inspectionToolbar!==tab;});
    document.body.classList.toggle("inspection-browser-active",tab==="browser");document.body.classList.toggle("inspection-image-active",tab==="image");
    if(tab==="sources")renderInspectionSources();
    updateDesktopViewer(state.inspectionOpen&&tab==="desktop");
    rememberInspectionShell();revealSelectedInspectionTab();syncInspectionOverlayOffset();syncBrowserChrome();
  }
  function showInspectionSidebar(tab=state.inspectionTab){
    const restore=conversationLayoutRestore();toggleActivitySummary(false);if(!state.inspectionOpen&&innerWidth<=760&&!document.body.classList.contains("sidebar-collapsed")){state.inspectionRestoreSidebar=true;$("sidebarToggle")?.click();}state.inspectionOpen=true;state.revealedInspectionTab="";document.body.classList.add("inspection-open");$("inspectionSidebar").setAttribute("aria-hidden","false");syncPanelControlLocation();applyInspectionExpanded(state.inspectionExpanded,false);applyInspectionWidth();syncInspectionHeader();setInspectionTab(tab);rememberInspectionShell();restore();if(state.inspectionTab==="browser")resumeInspectionBrowser().catch((error)=>ui.toast(error.message||String(error),true));requestAnimationFrame(()=>{scheduleNativeBrowserBounds();syncComposerDensity();});
  }
  function hideInspectionSidebar(){
    const restore=conversationLayoutRestore(),restoreSidebar=state.inspectionRestoreSidebar;setImageCommentMode(false);state.inspectionRestoreSidebar=false;state.inspectionOpen=false;updateDesktopViewer(false);document.body.classList.remove("inspection-open","inspection-expanded","inspection-browser-active","inspection-image-active");$("inspectionSidebar").setAttribute("aria-hidden","true");syncPanelControlLocation();rememberInspectionShell();syncBrowserChrome();restore();requestAnimationFrame(()=>{if(restoreSidebar&&document.body.classList.contains("sidebar-collapsed"))$("sidebarWake")?.click();syncWorkspaceLeft();syncComposerDensity();});
  }
  function toggleInspectionSidebar(){state.inspectionOpen?hideInspectionSidebar():showInspectionSidebar();}
  function renderInspectionSources(){const sources=conversationSources(),host=$("inspectionSourcesGrid");$("inspectionSourcesCount").textContent=String(sources.length);$("inspectionSourcesLabel").textContent=sources.length?`${sources.length} conversation image${sources.length===1?"":"s"}`:"Conversation images";host.innerHTML=sources.map((source,index)=>`<button type="button" class="inspection-source-card${source.source?"":" source-loading"}" data-source-card="${index}">${source.source?`<img src="${escape(source.source)}" alt="${escape(sourceLabel(source))}">`:'<span class="source-preview-placeholder">Loading preview…</span>'}<strong>${escape(sourceLabel(source)||`Image ${index+1}`)}</strong><small>${escape(source.kind||"Image")}</small></button>`).join("")||'<div class="inspection-empty review-empty"><strong>No conversation images yet</strong><small>Screenshots posted in this conversation will appear here.</small></div>';resolveInspectionSourceThumbnails(sources);}
  function resolveInspectionSourceThumbnails(sources){$("inspectionSourcesGrid")?.querySelectorAll(".source-loading[data-source-card]").forEach(async(card)=>{const source=sources[Number(card.dataset.sourceCard)],path=source?.path;if(!path)return;try{const data=await ui.invoke("image_data_url",{path});if(!card.isConnected)return;source.source=data;card.classList.remove("source-loading");card.querySelector(".source-preview-placeholder")?.replaceWith(Object.assign(document.createElement("img"),{src:data,alt:source.name||"Conversation image"}));}catch{card.classList.add("source-unavailable");const placeholder=card.querySelector(".source-preview-placeholder");if(placeholder)placeholder.textContent="Preview unavailable";}});}
  function openSourcesModal(){$("inspectionSourcesTab").hidden=false;showInspectionSidebar("sources");renderInspectionSources();}
  function browserCursorPoint(event){
    const payload=parseToolPayload(event?.target),x=Number(payload?.x??payload?.cx),y=Number(payload?.y??payload?.cy);
    if(Number.isFinite(x)&&Number.isFinite(y)){const rect=$("browserViewport").getBoundingClientRect();return{real:true,x:Math.max(8,Math.min(92,x/Math.max(1,rect.width)*100)),y:Math.max(8,Math.min(88,y/Math.max(1,rect.height)*100))};}
    const seed=answerKey(`${event?.tool||"browser"}:${event?.target||""}`).split("").reduce((sum,char)=>sum+char.charCodeAt(0),0);return{x:18+(seed%67),y:18+((seed*7)%61)};
  }
  function animateBrowserCursor(event){
    const cursor=$("browserGhostCursor");if(!cursor)return;const point=browserCursorPoint(event);
    // The live page covers this layer; draw the cursor inside the page too.
    if(state.browserNative&&state.browserOwnerId&&point.real)ui.invoke("browser_surface_cursor",{instance:state.browserOwnerId,x:point.x,y:point.y,click:/click|act|navigate/i.test(String(event?.tool||"")),visible:true,motion:document.documentElement.dataset.cursorMotion||"signature_arc",reduced:document.documentElement.dataset.motion==="minimal"||matchMedia("(prefers-reduced-motion:reduce)").matches}).catch(()=>{});cursor.style.setProperty("--cursor-x",`${point.x}%`);cursor.style.setProperty("--cursor-y",`${point.y}%`);cursor.classList.add("active");cursor.classList.toggle("click",/click|act|navigate/i.test(String(event?.tool||"")));clearTimeout(state.browserGhostTimer);state.browserGhostTimer=setTimeout(()=>cursor.classList.remove("click"),480);
    // Only a live agent action shows the cursor; it fades once the agent stops.
    clearTimeout(state.browserGhostHideTimer);state.browserGhostHideTimer=setTimeout(hideBrowserCursor,2200);
  }
  function hideBrowserCursor(){clearTimeout(state.browserGhostHideTimer);$("browserGhostCursor")?.classList.remove("active","click");if(state.browserNative&&state.browserOwnerId)ui.invoke("browser_surface_cursor",{instance:state.browserOwnerId,visible:false}).catch(()=>{});}
  function inspectionImageId({path="",source="",name="Image"}={}){return`image-${answerKey(`${path}|${name}|${String(source).slice(0,120)}`).replace(/[^a-z0-9]/gi,"").slice(-24)}`;}
  function activeInspectionImage(){return state.inspectionImages.find((item)=>item.id===state.activeInspectionImageId)||null;}
  function imageAnnotations(item=activeInspectionImage()){if(!item)return[];if(Array.isArray(item.comments)){const comments=normalizedImageComments(item.comments);item.comments.splice(0,item.comments.length,...comments);state.imageAnnotations.set(item.id,item.comments);return item.comments;}if(!state.imageAnnotations.has(item.id))state.imageAnnotations.set(item.id,[]);item.comments=state.imageAnnotations.get(item.id);return item.comments;}
  function renderInspectionImageTabs(){const host=$("inspectionImageTabs");if(!host)return;host.innerHTML=state.inspectionImages.map((item)=>`<button type="button" role="tab" class="inspection-tab image-page-tab" data-inspection-tab="image" data-image-tab-id="${escape(item.id)}" aria-selected="${String(state.inspectionTab==="image"&&item.id===state.activeInspectionImageId)}" tabindex="${state.inspectionTab==="image"&&item.id===state.activeInspectionImageId?"0":"-1"}" title="${escape(item.name)}"><img class="image-tab-thumbnail" src="${escape(item.source||ui.phoenixLogoSource())}" alt=""><strong>${escape(item.name)}</strong><span class="browser-tab-close" data-close-image-tab aria-label="Close ${escape(item.name)}">×</span></button>`).join("");revealSelectedInspectionTab();}
  async function paintActiveInspectionImage(){const item=activeInspectionImage(),request=++state.inspectionImageRequest,figure=$("inspectionImageFigure"),empty=$("inspectionImageEmpty"),image=$("inspectionImage"),loading=$("inspectionImageLoading");if(!item){empty.hidden=false;figure.hidden=true;return;}empty.hidden=true;figure.hidden=false;image.hidden=true;image.removeAttribute("src");loading.hidden=false;loading.textContent="Loading image…";try{const data=item.source||(preview?ui.phoenixLogoSource():await ui.invoke("image_data_url",{path:item.path}));if(request!==state.inspectionImageRequest||item.id!==state.activeInspectionImageId)return;item.source=data;await new Promise((resolve,reject)=>{image.onload=resolve;image.onerror=reject;image.src=data;});if(request!==state.inspectionImageRequest||item.id!==state.activeInspectionImageId)return;image.alt=item.name;image.hidden=false;loading.hidden=true;renderInspectionImageTabs();renderImageCommentState();}catch{if(request!==state.inspectionImageRequest)return;loading.textContent="This image is no longer available.";renderImageCommentState();}}
  async function selectInspectionImageTab(id){if(!id||id===state.activeInspectionImageId)return;setImageCommentMode(false);state.activeInspectionImageId=id;renderInspectionImageTabs();setInspectionTab("image");await paintActiveInspectionImage();}
  function closeInspectionImageTab(id=state.activeInspectionImageId){const index=state.inspectionImages.findIndex((item)=>item.id===id);if(index<0)return;setImageCommentMode(false);state.inspectionImages.splice(index,1);if(state.activeInspectionImageId===id)state.activeInspectionImageId=state.inspectionImages[Math.min(index,state.inspectionImages.length-1)]?.id||"";renderInspectionImageTabs();if(state.activeInspectionImageId){setInspectionTab("image");paintActiveInspectionImage();}else setInspectionTab(conversationSources().length?"sources":"desktop");}
  function setImageCommentMode(open){state.imageCommentMode=Boolean(open&&activeInspectionImage());if(!state.imageCommentMode)state.imageCommentDraft=null;document.body.classList.toggle("image-commenting",state.imageCommentMode);$("imageCommentToggle")?.setAttribute("aria-pressed",String(state.imageCommentMode));$("imageCommentInstruction").hidden=!state.imageCommentMode;renderImageCommentState();}
  function imageCommentPosition(point){const image=$("inspectionImage").getBoundingClientRect(),canvas=$("inspectionImageCanvas").getBoundingClientRect();return`left:${image.left-canvas.left+image.width*point.x/100}px;top:${image.top-canvas.top+image.height*point.y/100}px`;}
  function renderImageCommentState(){
    const item=activeInspectionImage(),comments=imageAnnotations(item),draft=state.imageCommentDraft,pins=$("imageCommentPins"),editor=$("imageCommentEditor");if(!pins||!editor)return;
    $("imageCommentTotal").textContent=`${comments.length} comment${comments.length===1?"":"s"}`;
    pins.innerHTML=comments.map((comment,index)=>`<button type="button" class="image-comment-pin saved" style="${imageCommentPosition(comment)}" title="${escape(comment.text)}" aria-label="Comment ${index+1}: ${escape(comment.text)}">${index+1}</button>`).join("")+(draft?`<span class="image-comment-pin draft" style="${imageCommentPosition(draft)}">${comments.length+1}</span>`:"");
    editor.hidden=!draft;
    if(draft){
      const image=$("inspectionImage").getBoundingClientRect(),canvas=$("inspectionImageCanvas").getBoundingClientRect(),pointX=image.left-canvas.left+image.width*draft.x/100,pointY=image.top-canvas.top+image.height*draft.y/100,width=editor.offsetWidth||360,height=editor.offsetHeight||90,left=pointX+18+width<=canvas.width-8?pointX+18:pointX-width-18,top=pointY+height+18<=canvas.height-8?pointY+18:pointY-height-18;
      editor.style.left=`${Math.max(8,Math.min(canvas.width-width-8,left))}px`;editor.style.top=`${Math.max(48,Math.min(canvas.height-height-8,top))}px`;if($("imageCommentText").value!==draft.text)$("imageCommentText").value=draft.text||"";
    }
    $("imageCommentSave").disabled=!draft?.text?.trim();$("imageCommentSend").disabled=!composerRequest();
  }
  function placeImageComment(event){if(!state.imageCommentMode||event.target.closest("button,textarea,label"))return;const image=$("inspectionImage"),rect=image.getBoundingClientRect();if(image.hidden||event.clientX<rect.left||event.clientX>rect.right||event.clientY<rect.top||event.clientY>rect.bottom)return;state.imageCommentDraft={x:Math.max(0,Math.min(100,(event.clientX-rect.left)/rect.width*100)),y:Math.max(0,Math.min(100,(event.clientY-rect.top)/rect.height*100)),text:""};renderImageCommentState();requestAnimationFrame(()=>$("imageCommentText").focus());}
  function toggleImageActualSize(){state.imageActualSize=!state.imageActualSize;$("inspectionImageCanvas").classList.toggle("actual-size",state.imageActualSize);$("imageResizeToggle").setAttribute("aria-pressed",String(state.imageActualSize));$("imageResizeToggle").textContent=state.imageActualSize?"Fit":"Actual size";}
  async function ensureImageCommentAttachment(item){if(item.path)return item.path;if(!item.source?.startsWith("data:image/"))return"";if(preview)return`/tmp/${item.name||"commented-image.png"}`;item.path=await ui.invoke("save_attachment",{dataUrl:item.source});return item.path;}
  function matchingComposerImage(item){return state.attachments.find((file)=>isComposerImage(file)&&((item.path&&file.path===item.path)||(!item.path&&file.name===item.name&&file.preview===item.source)));}
  async function saveImageComment(){const item=activeInspectionImage(),draft=state.imageCommentDraft,text=String(draft?.text||"").trim();if(!item||!draft||!text)return;const path=await ensureImageCommentAttachment(item);if(!path)throw new Error("Phoenix could not attach this image to the composer.");const comments=imageAnnotations(item),comment={...draft,text,createdAt:new Date().toISOString()};comments.push(comment);const extension=item.name.toLowerCase().split(".").at(-1),type=COMPOSER_IMAGE_EXTENSIONS.get(extension)||"image/png",existing=matchingComposerImage(item);if(existing){existing.preview=existing.preview||item.source||"";existing.comments=comments;}else state.attachments.push({name:item.name,path,size:0,type,preview:item.source||"",comments});state.imageCommentDraft=null;$("imageCommentText").value="";renderAttachments();renderImageCommentState();}
  function clearImageComments(){const item=activeInspectionImage();if(!item)return;const comments=imageAnnotations(item);comments.splice(0);state.imageCommentDraft=null;const attachment=matchingComposerImage(item);if(attachment)attachment.comments=[];renderAttachments();setImageCommentMode(false);}
  async function sendCommentedComposer(){if(!composerRequest())return;await submitTurn();setImageCommentMode(false);}
  async function openImageInspector({path="",source="",name="Image",comments=null}={}){const resolvedName=name||String(path).split("/").filter(Boolean).at(-1)||"Image",candidate={path,source,name:resolvedName,comments:Array.isArray(comments)?comments:undefined},id=inspectionImageId(candidate),existing=state.inspectionImages.find((item)=>item.id===id);if(existing){const preserved=existing.comments;Object.assign(existing,candidate);if(!Array.isArray(candidate.comments))existing.comments=preserved;}else state.inspectionImages.push({...candidate,id});state.activeInspectionImageId=id;renderInspectionImageTabs();showInspectionSidebar("image");await paintActiveInspectionImage();}
  function renderBrowserSettingsMenu(){
    const menu=$("browserSettingsMenu");menu.innerHTML='<button type="button" data-browser-setting="find">Find in page <span>Ctrl+F</span></button><div class="zoom-row"><strong>Zoom</strong><button type="button" data-browser-setting="zoom_out" aria-label="Zoom out">−</button><button type="button" data-browser-setting="zoom_reset">100%</button><button type="button" data-browser-setting="zoom_in" aria-label="Zoom in">+</button></div><hr><button type="button" data-browser-setting="device">Toggle device preview <span>390 px</span></button><button type="button" data-browser-setting="screenshot">Capture screenshot</button><button type="button" data-browser-setting="downloads">Downloads</button><button type="button" data-browser-setting="history">History</button><hr><button type="button" data-browser-setting="passwords">Passwords and autofill</button><button type="button" data-browser-setting="clear">Clear browsing data</button><button type="button" data-browser-setting="settings">Browser settings</button>';
  }
  async function runBrowserSetting(action){
    $("browserSettingsMenu").hidden=true;
    if(!state.browserOwnerId&&!["settings"].includes(action)){ui.toast("Open a browser tab first.",true);return;}
    try{
      if(action==="find")await browserCommand({action:"send_keys",keys:"Ctrl+F"});
      else if(action==="zoom_in"){state.browserZoom=Math.min(2,state.browserZoom+.1);await browserCommand({action:"send_keys",keys:"Ctrl++"});}
      else if(action==="zoom_out"){state.browserZoom=Math.max(.5,state.browserZoom-.1);await browserCommand({action:"send_keys",keys:"Ctrl+-"});}
      else if(action==="zoom_reset"){state.browserZoom=1;await browserCommand({action:"send_keys",keys:"Ctrl+0"});}
      else if(action==="device"){$("browserViewport").classList.toggle("device-preview");scheduleNativeBrowserBounds();scheduleBrowserResize();}
      else if(action==="screenshot"){const result=await ui.invoke("browser_surface_screenshot",{instance:state.browserOwnerId});ui.toast(`Screenshot saved${result?.path?` to ${result.path}`:""}.`);}
      else if(action==="downloads"){const files=await ui.invoke("browser_downloads_list",{profileId:state.browserOwnerId});if(files?.[0]?.path)await ui.invoke("browser_download_open",{path:files[0].path,reveal:true});else ui.toast("No downloads in this browser profile yet.");}
      else{const destinations={history:"chrome://history",passwords:"chrome://password-manager/passwords",clear:"chrome://settings/clearBrowserData",settings:"chrome://settings"};await navigateBrowser(destinations[action]);}
    }catch(error){ui.toast(`Browser control failed: ${error.message||error}`,true);}
  }

  async function catchUpConversation(token=activeSelectionToken(),{quiet=true}={}) {
    if(!token)return;
    try {
      const rows=await ui.invoke("session_context_get",{sessionId:token.sessionId,owner:token.owner});
      if(!selectionIsCurrent(token))return;
      // The background catch-up only appends. While a coworker's context is
      // being compacted its saved history is rewritten; reconciling against it
      // redrew the whole conversation every few seconds.
      const {added,reordered}=reconcileHistory(rows,{appendOnly:quiet});
      if(!added.length&&!reordered){flushOwnedStories();return;}
      $("conversationFeed").querySelector(".conversation-empty")?.remove();
      if(reordered)repaintConversation(conversationScrollBookmark(),false);
      else paintFeed(()=>added.forEach(renderDisplayEntry),state.pinToLatest);
      syncRestoredWorkVisibility();
      // An authored interim answer is not a turn-completion receipt.
      // Done and the live activity registry own the working state.
      scheduleDisplayPersist(true);
      flushOwnedStories();
    } catch {}
  }
  function subscribeJournal(candidate=activeSelectionToken()) {
    const token=candidate?.key?candidate:activeSelectionToken(Number.isFinite(candidate)?candidate:state.loadGeneration);
    state.subscription?.close();if(preview||!token||!selectionIsCurrent(token))return;
    // The server replays only workers that are active at this exact
    // subscription boundary. Clear this conversation's prior live snapshot
    // before replay so a worker that finished while the socket was down cannot
    // remain stuck as "working" forever; current Started rows immediately
    // repopulate it ahead of the registration Pong.
    clearEphemeralWorkersForConversation(conversationKeyOf(state.item));syncActivitySummary();
    const socket=new WebSocket(ui.wsUrl());state.subscription=socket;
    socket.onopen=()=>{if(selectionIsCurrent(token)&&state.subscription===socket)socket.send(JSON.stringify({SubscribeJournal:{session_id:token.sessionId,owner:token.owner}}));};
    socket.onmessage=(message)=>{if(state.subscription!==socket||!selectionIsCurrent(token))return;state.lastJournalEventAt=Date.now();try{const value=JSON.parse(message.data);consumeFluffyWire(value,fluffyContext());if(consumeVolumeWorkerLifecycle(value))return;if(value.StoryReplay)renderStory(value.StoryReplay,true);else if(value==="Pong"||value?.Pong!==undefined){if(state.replayNeedsRepaint){state.replayNeedsRepaint=false;repaintConversation(conversationScrollBookmark(),false);syncRestoredWorkVisibility();}if(!state.historyHydrating)catchUpConversation(token,{quiet:true});return;}else if(value.Story){
      const event=value.Story,disposition=journalStoryDisposition(event);
      if(disposition==="render")renderStory(event);
      else if(disposition==="browser")autoRevealAgentBrowser(normalizeGroupOperationalAgent(event));
    }}catch{}};
    socket.onclose=()=>{if(state.subscription===socket&&selectionIsCurrent(token))setTimeout(()=>{if(selectionIsCurrent(token))subscribeJournal(token);},3000);};
  }

  function journalStoryDisposition(event){
    if(ownedStoryTurn(event)&&state.activeTurnId&&ownedStoryTurn(event)!==state.activeTurnId)return"render";
    if(!state.turnSocket||state.queuedWakeTurnId||event?.kind==="user"||["handoff","return","group_message"].includes(event?.kind))return"render";
    if(["tool_start","tool"].includes(event?.kind)&&/^browser_(?!close$)/i.test(String(event?.tool||"")))return"browser";
    return"ignore";
  }

  function activatableCoworker(profile){return Boolean(profile&&profile.lifecycle==="active"&&profile.kind==="responsibility_owner");}
  function mintClientTurnId(){
    if(globalThis.crypto?.randomUUID)return`turn_${crypto.randomUUID().replaceAll("-","")}`;
    const bytes=new Uint8Array(16);globalThis.crypto?.getRandomValues?.(bytes);
    const random=[...bytes].map((byte)=>byte.toString(16).padStart(2,"0")).join("")||`${Date.now()}_${Math.random().toString(16).slice(2)}`;
    return`turn_${random}`;
  }
  function composerImageCommentContext(files){const groups=files.map((file)=>({file,comments:normalizedImageComments(file.comments)})).filter((group)=>group.comments.length);if(!groups.length)return"";return`\n\nImage comments:\n${groups.map(({file,comments})=>`**${file.name||"Image"}**\n${comments.map((comment,index)=>`${index+1}. ${Math.round(comment.x)}% from the left, ${Math.round(comment.y)}% from the top — ${comment.text}`).join("\n")}`).join("\n\n")}`;}
  function composerRequest(text = null) {
    if(text==null)normalizeComposerTokens(true);
    const typedRequest=String(text ?? composerText("request")).trim(),typedDisplay=String(text ?? composerText("display")).trim(),files=[...state.attachments],attachments=files.map((file)=>file.path).filter(Boolean),profiles=state.mentions.map(knownAgentProfile).filter(activatableCoworker);
    if(!typedRequest&&!attachments.length&&!profiles.length&&!state.groupEveryone)return null;
    const fallback=`Review ${attachments.length===1?"the attached file":"these attached files"}.`,requestBody=typedRequest||fallback,displayBody=typedDisplay||fallback,commentContext=composerImageCommentContext(files);
    let requestText=`${requestBody}${commentContext}`;
    // Plain follow-ups keep the latest initiating coworker; explicit mentions
    // and @everyone retain their existing activation rules.
    // Group leader architecture: a led room routes unaddressed messages to
    // its leader server-side, so the follow-up rewrite applies only to
    // legacy payloads without leader_agent_id.
    if(state.item?.kind==='group'&&!ui.profileFor(state.item)?.leader_agent_id&&!/@[\w-]+/.test(requestBody)&&!state.groupEveryone){
      const latest=[...state.displayRows].reverse().find(entry=>displayRole(entry)==='user');
      const owner=latest&&initiatingAgentForTurn({turn_id:displayTurnId(latest)});
      const profile=owner&&knownAgentProfile(owner);
      if(activatableCoworker(profile)&&composerGroupProfiles().some(member=>member.agent_id===profile.agent_id))requestText=`@${profile.agent_id} ${requestText}`;
    }
    const displayText=displayBody;
    return{requestText,displayText,typedText:typedDisplay,files,attachments,everyone:state.groupEveryone};
  }
  function confirmEveryoneActivation(previewValue) {
    const names=previewValue.active_display_names||[];
    return new Promise((resolve)=>{
      let settled=false;
      const finish=(confirmed)=>{if(settled)return;settled=true;ui.closeModal();resolve(confirmed);};
      ui.showModal(`<section class="modal everyone-confirm" role="dialog" aria-modal="true" aria-labelledby="everyoneConfirmTitle"><header class="modal-header"><span><strong id="everyoneConfirmTitle">Wake everyone in this group?</strong><small>This activates the exact current roster shown below. Nobody added later will be silently included.</small></span><button class="modal-close" type="button" aria-label="Cancel"><svg viewBox="0 0 20 20"><path d="m5 5 10 10M15 5 5 15"/></svg></button></header><div class="modal-body"><div class="everyone-confirm-list" role="list">${names.map((name)=>`<div role="listitem"><span>${escape(name)}</span><small>will wake</small></div>`).join("")}</div></div><footer class="modal-footer"><button class="button secondary" type="button" data-everyone-cancel>Cancel</button><button class="button primary" type="button" data-everyone-confirm>Wake ${names.length} coworker${names.length===1?"":"s"}</button></footer></section>`);
      $("modalLayer").addEventListener("phoenix:modal-closing",()=>{if(!settled){settled=true;resolve(false);}},{once:true});
      $("modalLayer").querySelector(".modal-close").onclick=()=>finish(false);
      $("modalLayer").querySelector("[data-everyone-cancel]").onclick=()=>finish(false);
      $("modalLayer").querySelector("[data-everyone-confirm]").onclick=()=>finish(true);
    });
  }
  async function prepareGroupActivation(request) {
    if(state.item?.kind!=="group")return null;
    const groupId=state.item.id,generation=state.loadGeneration,lifecycle=ui.profileFor(state.item)?.lifecycle;
    if(lifecycle&&lifecycle!=="active")throw new Error(lifecycle==="pending_deletion"?"This group is scheduled for deletion, so nobody here can be woken. Restore it from the sidebar menu to talk in it.":"This group is archived, so nobody here can be woken. Restore it from the sidebar menu to talk in it.");
    const response=await rpc({GroupActivationPreview:{group_id:groupId,user_request:request.requestText}});
    if(generation!==state.loadGeneration||state.item?.kind!=="group"||state.item.id!==groupId)throw new Error("The conversation changed before the group preview finished.");
    const previewValue=response.GroupActivationPreview;
    if(!previewValue||previewValue.group_id!==groupId)throw new Error("Phoenix could not verify who this group message would wake.");
    if(request.everyone&&!await confirmEveryoneActivation(previewValue))return false;
    const waves=previewValue.execution_wave_display_names||[],names=previewValue.active_display_names||[];
    if(names.length>1&&!request.everyone){
      const dependencies=previewValue.execution_dependencies,ids=previewValue.active_agent_ids||[],nameFor=(id)=>names[ids.indexOf(id)]||id;
      const summary=Array.isArray(dependencies)&&dependencies.length
        ?`Start: ${ids.filter((id)=>!dependencies.some((edge)=>edge.dependent===id)).map(nameFor).join(" + ")}. ${ids.filter((id)=>dependencies.some((edge)=>edge.dependent===id)).map((id)=>`${nameFor(id)} waits for ${dependencies.filter((edge)=>edge.dependent===id).map((edge)=>nameFor(edge.prerequisite)).join(" + ")}`).join("; ")}.`
        :previewValue.execution_mode==="ordered"
        ?`Order: ${waves.map((wave)=>wave.join(" + ")).join(" → ")}. Later waves wait for persisted results.`
        :`Parallel: ${names.join(", ")}. Replies appear as they finish.`;
      ui.toast(summary);
    }
    return{group_id:previewValue.group_id,roster_fingerprint:previewValue.roster_fingerprint,selection:previewValue.selection,active_agent_ids:previewValue.active_agent_ids||[],execution_mode:previewValue.execution_mode||"parallel",execution_waves:previewValue.execution_waves||[],execution_dependencies:previewValue.execution_dependencies??null,inspection_participants:previewValue.inspection_participants||[],tool_constraints:previewValue.tool_constraints||{}};
  }
  // ── Prompt stash ───────────────────────────────────────────────────────
  // Set a draft aside and bring it back later. The stash button stashes the
  // current text, or opens the stash (a beUI BottomSheet) when the box is
  // empty. Using a stashed prompt swaps it with whatever is being typed.
  const STASH_KEY="phoenix-prompt-stash";
  function readStash(){try{const rows=JSON.parse(localStorage.getItem(STASH_KEY)||"[]");return Array.isArray(rows)?rows.filter((row)=>row&&typeof row.text==="string"):[];}catch{return[];}}
  function writeStash(rows){try{localStorage.setItem(STASH_KEY,JSON.stringify(rows.slice(0,60)));}catch{}syncStashButton();}
  function syncStashButton(){const count=readStash().length,badge=$("stashButton")?.querySelector(".stash-count");if(!badge)return;badge.hidden=!count;badge.textContent=count>99?"99+":String(count);}
  function setComposerDraft(tokens){const input=$("composerInput");input.value=tokens;input.dispatchEvent(new Event("input",{bubbles:true}));autosize();syncSendMode();input.focus();}
  function stashCurrentPrompt(){
    const display=composerText("display").trim();if(!display)return false;
    const rows=readStash();rows.unshift({id:`stash-${Date.now().toString(36)}`,text:display,tokens:$("composerInput").value,where:currentName(),created_at:new Date().toISOString()});writeStash(rows);
    setComposerDraft("");const button=$("stashButton");button.classList.remove("stashed");void button.offsetWidth;button.classList.add("stashed");
    ui.toast("Prompt stashed.");return true;
  }
  function stashAge(iso){const minutes=Math.round((Date.now()-new Date(iso).getTime())/60000);return minutes<1?"just now":minutes<60?`${minutes}m ago`:minutes<1440?`${Math.round(minutes/60)}h ago`:`${Math.round(minutes/1440)}d ago`;}
  function openPromptStash(){
    const kit=window.PhoenixAgentKit,SNAPS=[.5,.92];let snap=0;
    if($("promptStashSheet"))return;
    const scrim=document.createElement("button");scrim.type="button";scrim.className="sheet-scrim";scrim.setAttribute("aria-label","Close prompt stash");
    const sheet=document.createElement("div");sheet.id="promptStashSheet";sheet.className="bottom-sheet";sheet.setAttribute("role","dialog");sheet.setAttribute("aria-modal","true");sheet.setAttribute("aria-labelledby","promptStashTitle");
    sheet.innerHTML=`<div class="sheet-top"><div class="sheet-grab" aria-hidden="true"><i></i></div><div class="sheet-heading"><h2 id="promptStashTitle">Prompt stash</h2><p>Drafts you set aside. Use one to put it back in the composer.</p></div></div><div class="sheet-body"><ul class="stash-list"></ul></div>`;
    const list=sheet.querySelector(".stash-list"),previousFocus=document.activeElement;
    const paint=()=>{const rows=readStash();list.innerHTML=rows.length?rows.map((row,index)=>`<li class="stash-card" data-stash-id="${escape(row.id)}" style="--i:${index}"><div class="stash-text">${escape(row.text)}</div><div class="stash-meta"><span>${escape(row.where||"")}${row.where?" · ":""}${escape(stashAge(row.created_at))}</span><span class="stash-actions"><button type="button" class="stash-use" data-stash-use>${kit.icon("cornerDownLeft")}Use</button><button type="button" class="stash-icon" data-stash-copy aria-label="Copy prompt" title="Copy">${kit.icon("copy")}</button><button type="button" class="stash-icon" data-stash-delete aria-label="Delete prompt" title="Delete">${kit.icon("trash")}</button></span></div></li>`).join(""):`<li class="stash-empty">${kit.icon("archive","stash-empty-icon")}<strong>Nothing stashed yet</strong><span>Type a prompt and press the stash button to set it aside.</span></li>`;};
    paint();
    document.body.append(scrim,sheet);
    const setSnap=(index)=>{snap=index;sheet.style.height=`${SNAPS[index]*100}vh`;};
    setSnap(0);
    requestAnimationFrame(()=>{scrim.classList.add("open");sheet.classList.add("open");});
    const close=()=>{if(!sheet.isConnected)return;scrim.classList.remove("open");sheet.classList.remove("open");sheet.style.transform="";removeEventListener("keydown",onKey,true);setTimeout(()=>{scrim.remove();sheet.remove();},520);(previousFocus?.isConnected?previousFocus:$("composerInput"))?.focus?.();};
    const onKey=(event)=>{if(event.key==="Escape"){event.preventDefault();event.stopPropagation();close();}};
    addEventListener("keydown",onKey,true);scrim.onclick=close;
    setTimeout(()=>sheet.querySelector("[data-stash-use], .sheet-grab")?.focus?.(),60);
    // Drag the grab pill: follow the finger (elastic past the top), then
    // dismiss on a strong fling or long pull, or settle to the nearest snap.
    const grab=sheet.querySelector(".sheet-top");let drag=null;
    grab.addEventListener("pointerdown",(event)=>{if(event.button!==0||event.target.closest("button"))return;drag={id:event.pointerId,y:event.clientY,t:performance.now(),lastY:event.clientY,lastT:performance.now()};sheet.classList.add("dragging");grab.setPointerCapture(event.pointerId);});
    grab.addEventListener("pointermove",(event)=>{if(!drag||drag.id!==event.pointerId)return;const dy=event.clientY-drag.y,offset=dy<0?dy*.02:dy*.9;sheet.style.transform=`translateY(${offset}px)`;drag.v=(event.clientY-drag.lastY)/Math.max(1,performance.now()-drag.lastT)*1000;drag.lastY=event.clientY;drag.lastT=performance.now();});
    const endDrag=(event)=>{if(!drag||drag.id!==event.pointerId)return;const offset=event.clientY-drag.y,velocity=drag.v||0;drag=null;sheet.classList.remove("dragging");sheet.style.transform="";
      if(velocity>600||offset>120){if(snap>0&&velocity<800&&offset<192){setSnap(snap-1);return;}close();return;}
      if(velocity<-500||offset<-80){setSnap(Math.min(SNAPS.length-1,snap+1));return;}
      if(offset>80&&snap>0)setSnap(snap-1);};
    grab.addEventListener("pointerup",endDrag);grab.addEventListener("pointercancel",endDrag);
    sheet.onclick=async(event)=>{const card=event.target.closest("[data-stash-id]");if(!card)return;const rows=readStash(),row=rows.find((item)=>item.id===card.dataset.stashId);if(!row)return;
      if(event.target.closest("[data-stash-delete]")){writeStash(rows.filter((item)=>item!==row));card.classList.add("leaving");setTimeout(paint,180);return;}
      if(event.target.closest("[data-stash-copy]")){try{await navigator.clipboard.writeText(row.text);ui.toast("Copied.");}catch{}return;}
      if(event.target.closest("[data-stash-use]")){const remaining=rows.filter((item)=>item!==row),current=composerText("display").trim();if(current)remaining.unshift({id:`stash-${Date.now().toString(36)}`,text:current,tokens:$("composerInput").value,where:currentName(),created_at:new Date().toISOString()});writeStash(remaining);close();setComposerDraft(row.tokens??row.text);if(current)ui.toast("Swapped: your draft went into the stash.");}};
  }
  function clearComposerDraft(){invalidateComposerIngress();$("composerInput").value="";state.attachments=[];state.mentions=[];state.groupEveryone=false;state.drafts.delete(conversationKey());localStorage.removeItem(draftStorageKey());renderAttachments();renderMentionTray();autosize();closeMention();closeSlash();}
  function clearSubmittedDraft(draft,token){
    // Acknowledgement of an older send never erases a newly edited draft.
    // A thumbnail can finish decoding while acknowledgement is in flight;
    // its display-only preview is not a change to the user's submitted draft.
    const content=value=>JSON.stringify({...value,attachments:value.attachments.map(({preview,...file})=>file)});
    if(draft&&selectionIsCurrent(token)&&content(draftSnapshot())===content(draft))clearComposerDraft();
  }
  function pendingSubmissionKey(token){return `phoenix-pending-submission:${token.key}`;}
  function submissionSignature(request){return JSON.stringify([request.requestText,request.attachments]);}
  function pendingSubmission(token){try{return JSON.parse(localStorage.getItem(pendingSubmissionKey(token))||"null");}catch{return null;}}
  function acknowledgeSubmission(request,draft,token){
    const saved=pendingSubmission(token);
    if(saved?.turnId===request.turnId)localStorage.removeItem(pendingSubmissionKey(token));
    if(state.unackedSend?.turnId===request.turnId)state.unackedSend=null;
    // The sent composer was already cleared; acknowledgement never edits a new draft.
  }
  // Idempotent send: a repeat Enter/click while a send is being accepted is
  // ignored, and the composer is cleared as soon as the send is accepted, so
  // a second press can never resubmit the same draft (it used to stay in the
  // composer until the first Story arrived; a second Enter then went out as a
  // mid-task steer with a fresh turn id, which the runtime cannot dedupe).
  async function submitTurn(text = null) {
    if(state.sendInFlight)return;
    state.sendInFlight=true;state.lastSendAt=Date.now();
    try{return await submitTurnOnce(text);}finally{state.sendInFlight=false;}
  }
  function restoreUnackedDraft(request,token){
    const pending=state.unackedSend;if(!pending||pending.turnId!==request?.turnId)return;
    state.unackedSend=null;const draft=pending.draft;
    if(selectionIsCurrent(token)&&conversationKey(pending.item)===conversationKey()){
      if(composerText("request").trim()||state.attachments.length)return;
      $("composerInput").value=draft.tokens||draft.text;state.attachments=[...draft.attachments];state.mentions=[...(draft.mentions||[])];state.groupEveryone=Boolean(draft.everyone&&state.item?.kind==="group");
      renderAttachments();renderMentionTray();autosize();syncSendMode();persistComposerDraft();
    }else if(pending.item&&!localStorage.getItem(draftStorageKey(pending.item))){
      try{localStorage.setItem(draftStorageKey(pending.item),JSON.stringify({...draft,attachments:draft.attachments.map(({preview,...file})=>file)}));}catch{}
      state.drafts.set(conversationKey(pending.item),draft);
    }
  }
  async function submitTurnOnce(text = null) {
    const request = composerRequest(text); if (!request) return;
    const turnToken=activeSelectionToken();if(!turnToken)return;
    const submittedDraft=text==null?draftSnapshot():null;
    const unresolved=pendingSubmission(turnToken);
    if(unresolved?.signature===submissionSignature(request)){
      ui.toast("The earlier send is still unconfirmed. Your draft is saved. Check this conversation and its current activity before sending the same request again.",true);return;
    }
    if(preview)document.documentElement.dataset.lastSubmittedRequest=request.requestText;
    request.turnId=mintClientTurnId();
    if(!request.attachments.length&&!state.mentions.length&&!state.groupEveryone&&await handleSlashCommand(request.typedText)){if(selectionIsCurrent(turnToken))clearComposerDraft();return;}
    try{
      request.groupActivation=await prepareGroupActivation(request);
      if(state.item?.kind==="group"&&request.groupActivation===false)return;
    }catch(error){ui.toast(error.message||String(error),true);return;}
    if(!selectionIsCurrent(turnToken))return;
    // A conversation that is working takes the message INTO the running work:
    // it is shown as sent right away. One-to-one: the agent reads it at its
    // next step. Group room: it lands in the room transcript at once, every
    // running member hears it, the addressed members act (an idle one starts
    // now). Nothing waits in a queue.
    if (state.working) { steerTurn(request,turnToken); return; }
    if (state.working) {
      try{
        const queueId=await queueTurn(request.requestText,request.attachments,request.groupActivation,request.turnId);
        if(!selectionIsCurrent(turnToken))return;
        const files=request.files.map(({name,path,size,type})=>({name,path,size,type})),draft={queueId,turnId:request.turnId,requestText:request.requestText,displayText:request.displayText,initiatingAgentId:request.groupActivation?.execution_waves?.[0]?.[0]||request.groupActivation?.active_agent_ids?.[0]||targetAgent(),files};
        state.queuedDrafts.set(request.turnId,draft);
        const local=state.queue.find((row)=>row.queue_id===queueId);if(local)Object.assign(local,{preview:request.displayText,turn_id:request.turnId});else state.queue.push({queue_id:queueId,preview:request.displayText,turn_id:request.turnId,state:"waiting"});
        state.queueBySession.set(conversationIdentity(),state.queue.map((item)=>({...item})));
        renderQueue();
        clearSubmittedDraft(submittedDraft,turnToken);
      }catch(error){if(selectionIsCurrent(turnToken))ui.toast(error.message||String(error),true);}
      return;
    }
    let socket=null;
    if(!preview){
      try{socket=new WebSocket(ui.wsUrl());}
      catch(error){ui.toast("The message could not be sent. Your draft is saved.",true);return;}
    }
    state.activeGroupAgentIds=state.item?.kind==="group"?[...(request.groupActivation?.active_agent_ids||[])]:[];
    if(state.item?.kind==="group")resetGroupContextSamples(turnToken.sessionId);
    state.pinToLatest = true;
    state.turnStartedAt = Date.now();
    state.pendingAnswer = null;
    appendDisplay("history",{role:"user",text:request.displayText,initiating_agent_id:request.groupActivation?.execution_waves?.[0]?.[0]||request.groupActivation?.active_agent_ids?.[0]||targetAgent(),attachments:request.files.map(({name,path,size,type})=>({name,path,size,type})),turn_id:request.turnId},true);
    const prompt = renderUser(request.displayText, request.files),groupWillWake=state.item?.kind!=="group"||Boolean(request.groupActivation?.active_agent_ids?.length); if(submittedDraft){state.unackedSend={turnId:request.turnId,draft:submittedDraft,item:state.item};clearComposerDraft();} if(groupWillWake){setWorking(true);beginTurnActivity(prompt);}
    if (preview) {
      acknowledgeSubmission(request,submittedDraft,turnToken);
      setTimeout(() => {if(selectionIsCurrent(turnToken))renderAgentUpdate(state.item?.id||"phoenix", "Checking the company context before I answer.");},120);
      setTimeout(() => {if(selectionIsCurrent(turnToken))renderTool({kind:"tool_start",agent:state.item?.id||"phoenix",tool:"codebase_search",target:"the company context"});},280);
      setTimeout(() => {if(selectionIsCurrent(turnToken))renderTool({kind:"tool",agent:state.item?.id||"phoenix",tool:"codebase_search",target:"the company context",ok:true,detail:"Found the relevant company memory and current state."});},900);
      setTimeout(async () => {if(!selectionIsCurrent(turnToken))return;renderAnswer("I have the context. ## Next steps 1. Confirm the exact behavior. 2. Verify it in the running app.");await settleAnswerMeta();if(!selectionIsCurrent(turnToken))return;setWorking(false);updateContext(84520,1000000);},1300); return;
    }
    const session=turnToken.sessionId;state.turnSocket=socket;
    const body=turnRequestBody(session,request,"queue");
    socket.onopen=()=>{if(state.turnSocket===socket&&selectionIsCurrent(turnToken)){
      try{
        // Persist uncertainty before writing to the transport. Reopening the
        // renderer must not silently mint a second turn for this exact send.
        localStorage.setItem(pendingSubmissionKey(turnToken),JSON.stringify({turnId:request.turnId,signature:submissionSignature(request)}));
        socket.send(JSON.stringify({Turn:body}));
      }catch(error){localStorage.removeItem(pendingSubmissionKey(turnToken));restoreUnackedDraft(request,turnToken);ui.toast("The message could not be sent. Your draft is saved.",true);finishTurn(socket,true,turnToken);}
    }else try{socket.close();}catch{}};
    bindTurnSocket(socket,request,turnToken,submittedDraft);
  }
  const STEERED_LABEL='<span class="steered-label" title="Sent while the agent was working; it reads this at its next step">Delivered mid-task</span>';
  function markSteeredNode(node,steered){
    if(!node)return;node.classList.toggle("steered-message",steered);
    const content=node.querySelector(".message-content");content?.querySelector(":scope > .steered-label")?.remove();
    if(steered)content?.insertAdjacentHTML("beforeend",STEERED_LABEL);
  }
  // Retag a mid-task bubble as the boundary of its own new turn. Used when the
  // running turn ended before the message could be delivered into it, so the
  // gateway started a normal turn with it instead (never both).
  function promoteSteeredUser(entry,node,turnId){
    entry.turn_id=turnId;entry.value.turn_id=turnId;entry.value.steer_claimed=true;delete entry.value.steered;
    if(node){node.dataset.turnId=turnId;markSteeredNode(node,false);}
    state.activeTurnId=turnId;state.queuedWakeTurnId=turnId;
    replaceDisplayRows(trimDisplayRows(state.displayRows),true);scheduleDisplayPersist(true);
    finishTurnActivity(false);state.pinToLatest=true;state.turnStartedAt=Date.now();setWorking(true);beginTurnActivity(node||null);
  }
  function steeredNodeFor(entry){
    const id=String(entry?.value?.steer_id||"");if(!id)return null;
    return $("conversationFeed").querySelector(`.user-message[data-steer-id="${CSS.escape(id)}"]`);
  }
  // The race-safety wake announces the adopted message as a new turn boundary
  // (WakeTurn). The bubble is already on screen as "delivered mid-task".
  function claimSteeredUser(event){
    const text=String(event?.text||"").replace(/…$/,""),turnId=String(event?.turn_id||"");
    if(!text||!turnId)return false;
    const head=text.slice(0,200);
    const entry=[...state.displayRows].reverse().find((row)=>displayRole(row)==="user"&&row.value?.steered&&!row.value?.steer_claimed
      &&(String(row.value.steer_request||"").startsWith(head)||String(row.value.text||"").startsWith(humanMentions(head))));
    if(!entry)return false;
    promoteSteeredUser(entry,steeredNodeFor(entry),turnId);
    return true;
  }
  function steerTurn(request,turnToken){
    const files=request.files.map(({name,path,size,type})=>({name,path,size,type}));
    const turnId=state.activeTurnId||state.displayRows.at(-1)?.turn_id||request.turnId;
    const entry={source:"history",turn_id:turnId,value:{role:"user",turn_id:turnId,text:request.displayText,attachments:files,steered:true,steer_id:request.turnId,steer_request:request.requestText}};
    state.displayRows.push(entry);state.displayDirty=true;
    const node=renderUser(request.displayText,request.files,null,{steered:true});
    if(node)node.dataset.steerId=request.turnId;
    clearComposerDraft();scheduleDisplayPersist(true);scrollLatest();
    if(preview)return;
    const socket=new WebSocket(ui.wsUrl());let settled=false,adopted=false;
    const fail=(error)=>{
      if(settled||adopted)return;settled=true;clearTimeout(timer);try{socket.close();}catch{}
      if(!selectionIsCurrent(turnToken))return;
      const at=state.displayRows.indexOf(entry);if(at>=0)state.displayRows.splice(at,1);
      node?.remove();replaceDisplayRows(trimDisplayRows(state.displayRows),true);scheduleDisplayPersist(true);
      if(!composerText("request").trim())setComposerDraft(request.typedText??request.displayText);
      ui.toast(`Your message was not delivered: ${error.message||error}`,true);
    };
    const timer=setTimeout(()=>fail(new Error("Phoenix did not confirm it in time.")),8000);
    socket.onopen=()=>{if(selectionIsCurrent(turnToken))socket.send(JSON.stringify({Turn:turnRequestBody(turnToken.sessionId,request,"steer")}));else fail(new Error("conversation changed"));};
    socket.onerror=()=>fail(new Error("could not reach the local runtime"));
    socket.onmessage=(message)=>{
      if(adopted||settled)return;
      let value;try{value=JSON.parse(message.data);}catch(error){fail(error);return;}
      if(value.Error){fail(new Error(value.Error.message));return;}
      if(value.Done?.completion==="steered"){settled=true;clearTimeout(timer);try{socket.close();}catch{}return;}
      // Anything else means the running turn had already ended: the gateway is
      // running this message as a normal turn on this socket. Adopt it.
      adopted=true;clearTimeout(timer);
      if(!selectionIsCurrent(turnToken)){try{socket.close();}catch{}return;}
      state.turnSocket=socket;promoteSteeredUser(entry,node,request.turnId);
      bindTurnSocket(socket,request,turnToken);socket.onmessage(message);
    };
  }
  function turnRequestBody(session,request,delivery){
    return {session_id:session,turn_id:request.turnId,user_request:request.requestText,permission_mode:state.permission,yolo:null,workspace:state.workspace||null,journal:true,target_agent:targetAgent(),target_group:targetGroup(),group_activation:request.groupActivation||null,delivery,sticky_notes:null,viewport:null,attachments:request.attachments.length?request.attachments:null};
  }
  // The foreground Turn socket's lifecycle. Shared by a normal send and by a
  // mid-task message whose running turn ended before it could be delivered
  // (the gateway then runs it as a normal turn on that same socket).
  function bindTurnSocket(socket,request,turnToken,submittedDraft=null){
    const session=turnToken.sessionId;
    socket.onmessage=async(message)=>{if(state.turnSocket!==socket||!selectionIsCurrent(turnToken))return;try{const value=JSON.parse(message.data);if(value.Error&&!value.Story&&!value.Done)restoreUnackedDraft(request,turnToken);if(value.Story||value.Done)acknowledgeSubmission(request,submittedDraft,turnToken);consumeFluffyWire(value,fluffyContext());if(value.Done && ["completed","canceled","stopped"].includes(value.Done.completion)) window.PhoenixFluffies?.terminal(value.Done.completion === "canceled" ? "canceled" : value.Done.completion === "completed" ? "turn_completed" : "stopped", {agentId:turnToken.owner.kind==="agent"?turnToken.owner.id:null,owner:turnToken.owner,sessionId:session,execution:window.PhoenixFluffies.activity.snapshot(turnToken.owner.id).execution,turnId:request.turnId},value.Done);if(value.Error)window.PhoenixFluffies?.terminal("error",{agentId:turnToken.owner.kind==="agent"?turnToken.owner.id:null,owner:turnToken.owner,sessionId:session,execution:window.PhoenixFluffies.activity.snapshot(turnToken.owner.id).execution,turnId:request.turnId},value.Error);if(consumeVolumeWorkerLifecycle(value))return;if(value.Story)renderStory(value.Story);if(value.Done?.completion==="queued"){finishTurn(socket,true,turnToken);return;}if(value.Done){const final=value.Done.final_markdown;if(state.item?.kind!=="group"&&final&&!isCompactionText(final)&&state.pendingAnswer?.text!==visibleAnswerText(final,targetAgent()||"phoenix")&&appendDisplay("history",{role:"answer",text:final,agent:targetAgent()||"phoenix"},true))renderAnswer(final);await settleAnswerMeta();if(!selectionIsCurrent(turnToken))return;if(state.item?.kind!=="group"&&!state.turnUsageSeen&&value.Done.context_window){const selected=selectedModelContext().effective,reported=value.Done.context_window[1],limit=selected?Math.min(selected,reported):reported;updateContext(value.Done.context_window[0],limit);}finishTurn(socket,terminalPreservesUnfinished(value.Done),turnToken);}if(value.Error){renderStory(turnFailureCard(value.Error,targetAgent()||"phoenix"));finishTurn(socket,true,turnToken);}}catch(error){if(selectionIsCurrent(turnToken))ui.toast(String(error),true);}};
    socket.onerror=()=>{restoreUnackedDraft(request,turnToken);if(state.turnSocket!==socket||!selectionIsCurrent(turnToken))return;renderStory({kind:"card",agent:targetAgent()||"phoenix",subject:"Connection lost",body:"Phoenix could not reach the local runtime. Your message remains visible here.",ok:false});finishTurn(socket,true,turnToken);};
    socket.onclose=()=>{
      restoreUnackedDraft(request,turnToken);
      if(state.turnSocket!==socket||!selectionIsCurrent(turnToken))return;
      renderStory({kind:"failure",agent:targetAgent()||"phoenix",turn_id:request.turnId,text:"Connection to Phoenix was lost before completion was confirmed. Your saved messages remain available. Check the recovered activity before resending the request."});
      finishTurn(socket,true,turnToken);
    };
  }
  function terminalPreservesUnfinished(summary){return["incomplete","canceled","unknown"].includes(summary?.completion)||Boolean(runtimeFailureSummary(summary?.final_markdown));}
  function finishTurn(socket,preserveWork=false,token=activeSelectionToken()){if(state.turnSocket===socket)state.turnSocket=null;try{socket.close();}catch{}if(!selectionIsCurrent(token))return;if(!state.queuedWakeTurnId){state.activeGroupAgentIds=[];setWorking(false,preserveWork);}refreshQueue(token);refreshTasks(token);}
  async function stopTurn(){
    if(!state.working)return;
    const token=activeSelectionToken(),owner=canvasConversationOwner(),delegate=liveDelegate();
    const cancel=async(sessionId,agentId,scope)=>{
      const reply=await rpc({Cancel:{session_id:sessionId,target_agent:agentId,owner:scope}},8000,token?.signal);
      if(reply.Done?.completion!=="canceled"||reply.Done?.route!=="cancel"||reply.Done?.main_session_id!==sessionId)throw new Error("Stop was not confirmed for this conversation.");
    };
    try{
      await cancel(state.sessionId,targetAgent(),owner);
      if(delegate)await cancel(delegate.sessionId,delegate.agentId,{kind:"agent",id:delegate.agentId});
    }catch(error){
      if(selectionIsCurrent(token))ui.toast("Phoenix could not confirm that work stopped. Check the current activity before retrying.",true);
      return;
    }
    if(!selectionIsCurrent(token))return;
    appendDisplay("story",{kind:"settled",agent:targetAgent()||"phoenix",ok:false},true);
    state.turnSocket?.close();state.turnSocket=null;state.queuedWakeTurnId="";state.activeGroupAgentIds=[];
    window.PhoenixFluffies?.terminal("stopped",{agentId:targetAgent(),sessionId:state.sessionId,execution:window.PhoenixFluffies.activity.snapshot(targetAgent()).execution});
    setWorking(false,true);updateTaskHeadline("Stopped",false);
  }
  function displayTurnDeletion(turnId) {
    const promptIndex=state.displayRows.findIndex((entry)=>entry.turn_id===turnId&&displayRole(entry)==="user");
    if(promptIndex<0)return null;
    const turnsAfter=new Set(state.displayRows.slice(promptIndex+1).filter((entry)=>displayRole(entry)==="user").map((entry)=>entry.turn_id));
    const askIds=[...new Set(state.displayRows.filter((entry)=>entry.turn_id===turnId&&entry.value?.kind==="ask_pending").map((entry)=>String(entry.value?.id||entry.value?.ask_id||"")).filter(Boolean))];
    return {turnsFromEnd:turnsAfter.size,prompt:String(state.displayRows[promptIndex].value?.text||""),askIds};
  }
  function repaintConversationAfterDeletion() {
    repaintConversation(null,true,false);
    foldRestoredWork();syncRestoredWorkVisibility();renderPromptRail();
  }
  async function permanentlyDeleteTurn(button,scope) {
    const node=button.closest("[data-turn-id]"),turnId=node?.dataset.turnId;
    if(!turnId||state.deletingTurnId)return;
    if(button.dataset.deleteArmed!=="true"){
      button.dataset.deleteArmed="true";button.classList.add("delete-armed");button.title="Click again to delete permanently";
      ui.toast(scope==="Prompt"?"Click delete again to permanently remove the prompt and its response.":"Click delete again to permanently remove the complete agent turn.");
      setTimeout(()=>{if(button.isConnected){delete button.dataset.deleteArmed;button.classList.remove("delete-armed");button.title=scope==="Prompt"?"Delete prompt and response":"Delete agent turn";}},2600);
      return;
    }
    const deletion=displayTurnDeletion(turnId);if(!deletion){ui.toast("That turn could not be located.",true);return;}
    state.deletingTurnId=turnId;button.disabled=true;
    const token=activeSelectionToken(),session=state.sessionId,generation=state.loadGeneration,identity=conversationIdentity(),owner=canvasConversationOwner();
    state.subscription?.close();state.subscription=null;
    state.turnSocket?.close();state.turnSocket=null;
    setWorking(false,true);
    const previousRows=state.displayRows.map((entry)=>cloneDisplayValue(entry)).filter(Boolean);
    try{
      const nextRows=scope==="Prompt"
        ?state.displayRows.filter((entry)=>entry.turn_id!==turnId)
        :state.displayRows.filter((entry)=>entry.turn_id!==turnId||displayRole(entry)==="user");
      replaceDisplayRows(ensureDisplayTurnIds(trimDisplayRows(nextRows)),true);
      state.answerMeta=new Map();state.pendingAnswer=null;
      $("approvalStack").replaceChildren();syncApprovalStack();
      await persistDisplayJournalStrict(session,owner);
      if(selectionIsCurrent(token))repaintConversationAfterDeletion();
      await rpc({DeleteTranscriptTurn:{session_id:session,turns_from_end:deletion.turnsFromEnd,expected_prompt:deletion.prompt,scope,ask_ids:deletion.askIds,owner}});
      if(selectionIsCurrent(token))ui.toast(scope==="Prompt"?"Prompt and complete response permanently deleted.":"Complete agent turn permanently deleted.");
    }catch(error){
      if(selectionIsCurrent(token)){replaceDisplayRows(ensureDisplayTurnIds(previousRows),true);try{await persistDisplayJournalStrict(session,owner);}catch{}repaintConversationAfterDeletion();ui.toast(error.message||String(error),true);}
      else{const cached=state.conversationViews.get(identity);if(cached){cached.displayRows=ensureDisplayTurnIds(previousRows);cached.displayBytes=displayRowsBytes(cached.displayRows);cached.displayDirty=true;}}
    }
    finally{if(state.deletingTurnId===turnId)state.deletingTurnId="";if(state.sessionId===session&&state.loadGeneration===generation)subscribeJournal(generation);}
  }
  async function queueTurn(text, attachments = [], groupActivation = null, turnId = mintClientTurnId()) {
    if(preview){const identity=conversationIdentity(),rows=state.previewQueues.get(identity)||[],queueId=`q-${Date.now()}`;rows.push({queue_id:queueId,preview:text,state:"waiting",attachment_count:attachments.length,enqueued_at:new Date().toISOString()});state.previewQueues.set(identity,rows);state.queue=rows;state.queueBySession.set(identity,rows.map((item)=>({...item})));renderQueue();return queueId;}
    const session=state.sessionId,body={session_id:session,turn_id:turnId,user_request:text,permission_mode:state.permission,yolo:null,workspace:state.workspace||null,journal:true,target_agent:targetAgent(),target_group:targetGroup(),group_activation:groupActivation||null,delivery:"queue",sticky_notes:null,viewport:null,attachments:attachments.length?attachments:null};
    return new Promise((resolve,reject)=>{
      const socket=new WebSocket(ui.wsUrl());let settled=false;
      const finish=(fn,value)=>{if(settled)return;settled=true;clearTimeout(timer);try{socket.close();}catch{}fn(value);};
      const timer=setTimeout(()=>finish(reject,new Error("Phoenix did not confirm the queued message in time.")),8000);
      socket.onopen=()=>socket.send(JSON.stringify({Turn:body}));
      socket.onerror=()=>finish(reject,new Error("Could not queue that prompt."));
      socket.onmessage=(message)=>{try{const value=JSON.parse(message.data);if(value.Error)finish(reject,new Error(value.Error.message));else if(value.Done){finish(resolve,value.Done.run_id);refreshQueue();}}catch(error){finish(reject,error);}};
    });
  }

  function syncSendMode() { const hasDraft=Boolean(composerText("request").trim()||state.attachments.length||state.mentions.length||state.groupEveryone),queueing=state.working&&hasDraft,button=$("sendButton");$("composerZone").classList.toggle("queueing-input",queueing);button.disabled=!state.working&&!hasDraft;const queueLabel=state.item?.kind==="group"?"Send now — everyone in the room hears it":"Send now — the agent reads it at its next step";button.setAttribute("aria-label",queueing?queueLabel:state.working?"Stop agent":"Send message");button.title=queueing?queueLabel:state.working?"Stop agent (Esc)":"Send message";syncSendOrb(); }
  // Liquid while working; gather into stop on hover or queue with a draft.
  function syncSendOrb() {
    const label=$("sendLabel");
    if(label)label.textContent=state.working?($("composerZone").classList.contains("queueing-input")?"Send":"Stop"):"Send";
    const canvas=$("sendOrb"),button=$("sendButton");
    if(!canvas||!button)return;
    if(!window.PhoenixFluidOrb){canvas.hidden=true;button.classList.remove("orb-live");return;}
    if(!state.sendOrb)state.sendOrb=new window.PhoenixFluidOrb(canvas);
    const live=state.working,hasDraft=$("composerZone").classList.contains("queueing-input");
    if(!live){state.sendOrb.set("off");button.classList.remove("orb-live");return;}
    const visible=state.sendOrb.set(hasDraft?"arrow":(state.sendOrbHovered||button.matches(":focus-visible"))?"stop":"liquid");
    button.classList.toggle("orb-live",visible);
  }
  function setWorking(value, preserveWork = false) { state.working=value; $("composerZone").classList.toggle("working",value); $("taskBlock").classList.toggle("live",value&&Boolean(state.tasks?.length)); syncSendMode(); if(!value){finishTurnActivity(preserveWork);document.querySelectorAll(".work-cluster.live").forEach((node)=>node.classList.remove("live"));requestAnimationFrame(()=>scrollLatest());}renderTasks();syncActivitySummary(); }
  function updateTaskHeadline(_text, live) { $("taskBlock").classList.toggle("live",Boolean(live)&&Boolean(state.tasks?.length)); }
  let taskTimer=null; function refreshTasksSoon(){clearTimeout(taskTimer);taskTimer=setTimeout(refreshTasks,250);}
  async function refreshTasks(token=null){
    const session=state.sessionId,owner=canvasConversationOwner(),identity=conversationIdentity();if(!session)return;
    try{
      const value=await rpc({TodoList:{session_id:session,owner}},8000,token?.signal);if(token?!selectionIsCurrent(token):conversationIdentity()!==identity)return;
      state.tasks=value.TodoList||[];state.tasksBySession.set(identity,state.tasks.map((item)=>({...item})));renderTasks();
    }catch{}
  }
  // To-dos follow beUI's TodoList. Each step's ring shows the agent's own
  // progress estimate (todo_write `progress`), rows update in place so the
  // check, strike-through and ring animate, and the list folds when done.
  const TODO_STATUS_LABEL={pending:"Pending","in-progress":"In progress",completed:"Completed",cancelled:"Cancelled"};
  function todoStatusSvg(){return`<svg class="td-status" viewBox="0 0 24 24" aria-hidden="true"><circle class="td-base" cx="12" cy="12" r="9"/><circle class="td-arc" cx="12" cy="12" r="9" pathLength="1"/><path class="td-tick" d="M7.5 12.25 10.5 15.25 16.75 8.75" pathLength="1"/><path class="td-cross" d="M8.5 8.5 15.5 15.5M15.5 8.5 8.5 15.5" pathLength="1"/></svg>`;}
  function renderTasks(){
    const block=$("taskBlock"),list=$("taskList"),rows=state.tasks||[],isDone=(item)=>item.completed===true||item.status==="completed",done=rows.filter(isDone).length,allComplete=rows.length>0&&done===rows.length,firstOpen=rows.findIndex((item)=>!isDone(item)&&item.status!=="cancelled");
    block.hidden=!rows.length;if(!rows.length){state.taskSignature="";state.taskAllComplete=false;list.replaceChildren();return;}
    // Fold once when the plan completes; reopen if it gains open steps again.
    if(allComplete&&!state.taskAllComplete&&state.taskSignature)state.taskPlanExpanded=false;
    if(!allComplete&&state.taskAllComplete)state.taskPlanExpanded=true;
    block.open=state.taskPlanExpanded;
    state.taskSignature=rows.map((item)=>`${item.task||item.text}:${item.status||isDone(item)}:${item.progress??""}`).join("|");
    state.taskAllComplete=allComplete;block.classList.toggle("complete",allComplete);block.classList.toggle("live",state.working&&!allComplete);$("taskHeadline").textContent="To-dos";$("taskProgress").innerHTML=`<span class="sr-only">${done} of ${rows.length} tasks completed</span><span aria-hidden="true">${done}/${rows.length}</span>`;
    const existing=new Map([...list.children].map((node)=>[node.dataset.key,node])),ordered=[];
    rows.forEach((item,index)=>{
      const title=String(item.text||item.content||item.task||""),explicit=String(item.status||"").replace("_","-"),status=["pending","in-progress","completed","cancelled"].includes(explicit)?explicit:isDone(item)?"completed":index===firstOpen&&state.working?"in-progress":"pending";
      const key=`${index}:${title}`;let node=existing.get(key);
      if(!node){node=document.createElement("li");node.className="task-item td-item entering";node.dataset.key=key;node.innerHTML=`${todoStatusSvg()}<span class="sr-only td-sr"></span><span class="td-text"><span class="td-strike-wrap"><span class="td-title-text"></span><span class="td-strike" aria-hidden="true"></span></span></span><span class="td-detail"></span>`;requestAnimationFrame(()=>requestAnimationFrame(()=>node.classList.remove("entering")));}
      existing.delete(key);
      const progress=Number.isFinite(Number(item.progress))&&item.progress!==null&&item.progress!==undefined?Math.max(0,Math.min(100,Number(item.progress))):null;
      node.dataset.status=status;node.classList.toggle("indeterminate",status==="in-progress"&&progress===null);
      node.style.setProperty("--td-progress",String(status==="in-progress"?(progress===null?0.68:progress/100):0));
      node.querySelector(".td-sr").textContent=`${TODO_STATUS_LABEL[status]}: `;
      node.querySelector(".td-title-text").textContent=title;
      const detail=node.querySelector(".td-detail"),detailText=String(item.detail||"")||(status==="in-progress"&&progress!==null?`${Math.round(progress)}%`:"");detail.textContent=detailText;detail.hidden=!detailText;
      ordered.push(node);
    });
    existing.forEach((node)=>node.remove());
    ordered.forEach((node,index)=>{if(list.children[index]!==node)list.insertBefore(node,list.children[index]||null);});
  }
  async function refreshQueue(token=null){
    const session=state.sessionId,owner=canvasConversationOwner(),identity=conversationIdentity();if(!session)return;
    try{
      const value=await rpc({QueuedTurns:{session_id:session,owner}},8000,token?.signal);if(token?!selectionIsCurrent(token):conversationIdentity()!==identity)return;
      state.queue=value.QueuedTurns||[];state.queueBySession.set(identity,state.queue.map((item)=>({...item})));renderQueue();
      clearTimeout(state.queueRefreshTimer);state.queueRefreshTimer=0;
      if(state.queue.some((row)=>["queued","waiting","running"].includes(String(row.state||"waiting"))))state.queueRefreshTimer=setTimeout(()=>refreshQueue(token),900);
    }catch{}
  }
  const QUEUE_GLYPH='<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M15 4v4.5a2 2 0 0 1-2 2H5"/><path d="m8 7.5-3 3 3 3"/></svg>';
  function queueFailureCopy(row){
    if(row.state!=="failed")return"";
    const detail=typeof row.failure==="string"?Array.from(row.failure).slice(0,2000).join(""):"";
    return `<span class="queue-review-note">Review this conversation before sending the request again. Removing this entry does not undo work or mark the task complete.</span>${detail?`<details class="queue-failure"><summary>Why this needs review</summary><pre>${escape(detail)}</pre></details>`:""}`;
  }
  function renderQueue(){
    const shownQueueIds=new Set(state.displayRows.map((entry)=>entry.value?.queued_id).filter(Boolean));
    // A prompt already on screen as a bubble is not repeated in the drawer,
    // even when the gateway queued it without this window knowing its id
    // (the agent was still busy when it was sent).
    const clean=(value)=>String(value||"").replace(/\s+/g," ").trim().replace(/…$/,"");
    const bubbles=[...$("conversationFeed").querySelectorAll(":scope > .user-message:not(.answer-resume-message)")].slice(-6).map((node)=>clean(node.querySelector(".user-bubble")?.textContent));
    (state.queue||[]).forEach((row)=>{const preview=clean(row.preview);if(preview&&row.state!=="failed"&&bubbles.some((text)=>text&&text.startsWith(preview.slice(0,120))))shownQueueIds.add(row.queue_id);});
    // Prompts authored in this window already appear as normal chat bubbles.
    // Keep the queue UI only as recovery/failure UI, rather than duplicating a
    // prompt in both the conversation and a persistent “queued” drawer.
    // Group rooms never queue a message: no queue row or chip there. Only a
    // failed item that needs review stays visible.
    const rows=(state.queue||[]).filter((row)=>["queued","waiting","failed"].includes(String(row.state||"waiting"))&&(row.state==="failed"||!shownQueueIds.has(row.queue_id))&&(state.item?.kind!=="group"||row.state==="failed")),block=$("queueBlock");
    block.hidden=!rows.length;
    const identity=conversationIdentity();
    if(rows.length){
      if(!state.queueExpandedBySession.has(identity))state.queueExpandedBySession.set(identity,true);
      block.open=state.queueExpandedBySession.get(identity)!==false;
    }
    $("queueCount").textContent=rows.length;
    $("queuePreview").textContent=rows[0]?.preview||"";
    // Steering converts a waiting prompt into an interrupt for the turn already
    // running. The runtime rejects it for group discussions, which keep their
    // next-turn ordering, so the action is not offered there.
    const steerable=state.working&&state.item?.kind!=="group";
    const list=$("queueList"),openDetails=new Set([...list.querySelectorAll('.queue-failure[open]')].map(node=>node.closest('.queue-row')?.dataset.id));
    const previousFocus=list.contains(document.activeElement)?document.activeElement:null,focusedRow=previousFocus?.closest('.queue-row')?.dataset.id,focusedAction=previousFocus?.dataset.queue;
    const focusedDetail=previousFocus?.matches?.('.queue-failure>summary'),scrollTop=list.scrollTop;
    list.innerHTML=rows.map((row)=>{const answer=row.origin?.kind==="ask_answer",pending=state.queueActions.has(`${identity}\u0000${row.queue_id}`),disabled=pending?' disabled':'';return`<div class="queue-row" data-id="${escape(row.queue_id)}"${pending?' aria-busy="true"':''}>`
      +`<span class="queue-glyph" aria-hidden="true">${QUEUE_GLYPH}</span>`
      +`<span class="queue-copy"><strong>${escape(row.preview)}</strong>`
      +`<small>${answer?"Answer · ":""}${escape(row.state==="failed"?"Needs review":row.state||"waiting")}</small>${queueFailureCopy(row)}</span>`
      +(steerable&&row.state!=="failed"?`<button class="queue-steer" data-queue="steer" title="Deliver this now, into the running turn"${disabled}>${QUEUE_GLYPH}<span>Steer</span></button>`:"")
      +`<button class="queue-icon danger" data-queue="cancel" aria-label="${row.state==="failed"?"Remove reviewed queue entry":"Remove queued message"}" title="Remove"${disabled}>${deleteIcon()}</button>`
      +`</div>`;}).join("");
    for(const details of list.querySelectorAll('.queue-failure'))details.open=openDetails.has(details.closest('.queue-row')?.dataset.id);
    const newFocusedRow=focusedRow?[...list.querySelectorAll('.queue-row')].find(row=>row.dataset.id===focusedRow):null;
    const focusTarget=focusedDetail?newFocusedRow?.querySelector('.queue-failure>summary'):[...newFocusedRow?.querySelectorAll('[data-queue]')||[]].find(button=>button.dataset.queue===focusedAction&&!button.disabled);
    focusTarget?.focus({preventScroll:true});list.scrollTop=scrollTop;
  }
  async function queueAction(button){
    const row=button.closest(".queue-row"),id=row?.dataset.id;if(!id||button.disabled||!['cancel','steer'].includes(button.dataset.queue))return;
    const steer=button.dataset.queue==="steer";
    if(preview){const identity=conversationIdentity();state.queue=state.queue.filter((x)=>x.queue_id!==id);for(const [turnId,draft] of state.queuedDrafts)if(draft.queueId===id)state.queuedDrafts.delete(turnId);state.previewQueues.set(identity,state.queue);state.queueBySession.set(identity,state.queue.map((item)=>({...item})));renderQueue();return;}
    button.disabled=true;
    const token=activeSelectionToken(),owner=canvasConversationOwner();
    const actionKey=`${conversationIdentity()}\u0000${id}`;if(state.queueActions.has(actionKey))return;
    state.queueActions.add(actionKey);
    try{
      await rpc(steer
        ?{SteerQueuedTurn:{session_id:state.sessionId,queue_id:id,owner}}
        :{CancelQueuedTurn:{session_id:state.sessionId,queue_id:id,owner}},8000,token?.signal);
      if(!steer)for(const [turnId,draft] of state.queuedDrafts)if(draft.queueId===id)state.queuedDrafts.delete(turnId);
      // A steer goes into the running turn instead of starting its own, so
      // nothing else would ever put the message into the conversation.
      if(steer&&selectionIsCurrent(token))for(const draft of [...state.queuedDrafts.values()])if(draft.queueId===id){
        const turnId=state.activeTurnId||draft.turnId;
        state.displayRows.push({source:"history",turn_id:turnId,value:{role:"user",turn_id:turnId,text:draft.displayText,attachments:draft.files,steered:true}});
        renderUser(draft.displayText,draft.files);state.queuedDrafts.delete(draft.turnId);
        state.displayDirty=true;scheduleDisplayPersist();scrollLatest();
      }
      if(selectionIsCurrent(token))await refreshQueue(token);
    }catch(error){if(selectionIsCurrent(token)){button.disabled=false;ui.toast(error.message||String(error),true);}}
    finally{state.queueActions.delete(actionKey);}
  }

  function approvalDetail(key,value) {
    const labels={site:"Website",agent_id:"Coworker",owner_agent_id:"Coworker",scope:"Available to",methods:"Sign-in options",reason:"Needed for",username_hint:"Account",workflow_goal:"Workflow",start_url:"Start at"};
    const scopes={agent:"Only this coworker",group:"This group",company:"Whole company"};
    const methods={import_cookies:"Import browser cookies",user_login:"Log in yourself",create_account:"Create a free account"};
    let rendered=value;
    if(key==="agent_id"||key==="owner_agent_id")rendered=agentLabel(value);
    else if(key==="scope")rendered=scopes[value]||value;
    else if(key==="methods")rendered=String(value).split(",").map((part)=>methods[part.trim()]||part.trim().replaceAll("_"," ")).join(" · ");
    else if(typeof value==="boolean")rendered=value?"Yes":"No";
    if(key==="start_url")try{rendered=new URL(String(value)).hostname.replace(/^www\./,"");}catch{}
    return {label:labels[key]||String(key).replaceAll("_"," ").replace(/^./,(letter)=>letter.toUpperCase()),value:rendered};
  }
  const APPROVAL_DETAIL_KEYS = new Set(["site","scope","reason","username_hint","workflow_goal","start_url"]);
  function askDisplayAnswers(ask) {
    if(Array.isArray(ask.display_answers))return ask.display_answers;
    const parsed=[...String(ask.answer||"").matchAll(/^\s*A:\s*(.*)$/gm)].map((match)=>match[1].trim());
    if(parsed.length)return parsed;
    return ask.answer?[String(ask.answer)]:[];
  }
  function renderAskHistory(ask) {
    const questions=Array.isArray(ask.questions)&&ask.questions.length?ask.questions:(ask.question?[{question:ask.question}]:[]),answers=askDisplayAnswers(ask),skipped=ask.status==="dismissed";
    // Every answer to a question looks the same: the user's reply on the
    // right, "Answer to <agent>", with the agent's continuing work below it.
    if(!skipped&&answers.some(Boolean)){
      const text=answers.filter(Boolean).join("\n\n"),existing=findAnswerBubble(String(ask.id||""),text);
      if(existing)return existing;
      const target=agentLabel(ask.agent||state.item?.id||"phoenix");
      const questionContext=questions.length?`<details class="answer-question-context"><summary>View question</summary><div class="markdown">${questions.map(question=>markdown(question.question||"" )).join("")}</div></details>`:"";
      const node=feedNode("message-row user-message answer-resume-message",`<div class="message-content"><div class="user-bubble answer-resume-bubble"><div class="answer-resume-marker"><span>${icons.reply||QUEUE_GLYPH}</span><strong>Answer to ${escape(target)}</strong></div>${questionContext}<div class="answer-resume-prompt">${markdown(text)}</div></div></div>`,{slot:"message",from:"user",askId:String(ask.id||"")});
      state.replayWorkCluster=null;
      if(!state.painting){scrollLatest();syncComposerFade();}
      return node;
    }
    const heading=skipped?"Request dismissed":questions.length===1?"Question answered":`${questions.length} questions answered`,profile=agentProfile(ask.agent||"phoenix");
    const previewAnswer=answers.find(Boolean)||(skipped?"Not now":"Answered");
    const rows=questions.map((question,index)=>`<div><strong>${escape(question.header||question.question||`Question ${index+1}`)}</strong>${question.header?`<p>${escape(question.question||"")}</p>`:""}<small>${escape(answers[index]??(skipped?"Not now":"Answered"))}</small></div>`).join("");
    const node=document.createElement("article");node.className="ask-history-row";node.innerHTML=`<details><summary><span class="ask-history-avatar">${ui.avatarSvg(profile)}</span><strong>${escape(agentLabel(ask.agent||"phoenix"))}</strong><small>${escape(heading)}</small><em>${escape(previewAnswer)}</em><span class="ask-history-chevron">${icons.chevron}</span></summary><div class="ask-history-details">${rows}</div></details>`;
    // A resolved question is one event in the current trace, not a boundary
    // that creates a second activity block. Once the turn finishes the whole
    // trace (including the answer receipt) folds under one Thought row.
    const replayCluster=state.painting?ensureWorkCluster(ask.agent||state.item?.id||"phoenix"):null;
    const activeCluster=state.working&&state.turnStatus?.isConnected?state.turnStatus:null;
    settleReasoningSubgroups(activeCluster||replayCluster);
    const replay=replayCluster?.querySelector(".work-tools");
    const active=activeCluster?.querySelector(".work-tools");
    if(active||replay)(active||replay).append(node);else $("conversationFeed").insertBefore(node,conversationTail);
    if(!state.painting){scrollLatest();syncComposerFade();}
    return node;
  }
  const DECISION_APPROVAL_ACTIONS = new Set(["tool_permission","governed_effect","outside_group_call","login_request","teach_workflow","vault_unlock","permanent_agent","pass_request"]);
  function approvalAccessName(value){return value==="full_access"?"Full Access":value==="workspace"?"Workspace":value==="talk"?"Talk":String(value||"").replaceAll("_"," ").replace(/^./,(letter)=>letter.toUpperCase());}
  function approvalToolAction(tool){
    const name=String(tool||"").toLowerCase();
    const actions={browser_state:"check the current Chrome page",browser_navigate:"open a page in Chrome",browser_extract:"read the current Chrome page",browser_act:"interact with the current Chrome page",computer_use:"use the computer",computer_act:"interact with the computer",computer_window_act:"interact with an app window",composio_run:"use a connected app",composio_connections:"check connected accounts",credential_list:"check saved passes",credential_generate:"create a secure password",pass_use:"use a saved pass",account_manage:"manage an account",mcp_call:"use a connected service",cron:"change a schedule",skill_install:"install a skill",bash:"run a command"};
    if(actions[name])return actions[name];
    const label=humanTool(name,true).replace(/^(?:Using|Checking|Running|Reading|Opening|Working in)\s+/i,"").trim();
    return `use ${label?label.toLowerCase():"this tool"}`;
  }
  function approvalDecisionPresentation(model,q){
    const action=model.approval.action,details=model.approval.details||{},name=agentLabel(model.ask.agent||model.cardAgent||"phoenix"),site=String(details.site||"").replace(/^https?:\/\//,"").replace(/\/$/,"");
    if(action==="tool_permission"){
      const required=approvalAccessName(details.required_mode),current=approvalAccessName(details.current_mode);
      return{title:`Allow ${name} to ${approvalToolAction(details.tool_name||model.approval.subject)} once?`,body:`This call needs ${required}. ${current||"Current access"} remains the default afterward.`,signal:3,label:"One call only",tone:"green"};
    }
    if(action==="governed_effect")return{title:`Approve ${model.approval.subject||"this exact action"}?`,body:`${name} is ready to act. This approval applies once and does not change standing permissions.`,signal:2,label:"Needs review",tone:"orange"};
    if(action==="login_request")return{title:`How should ${name} sign in${site?` to ${site}`:""}?`,body:"Use the site's real sign-in. Passkeys, security keys, passwords, OAuth and 2FA stay between the site, browser and your device; Phoenix keeps only the resulting private session.",signal:3,label:"User presence · no secret capture",tone:"green"};
    if(action==="vault_unlock")return{title:"Unlock Passes?",body:safeRuntimeCopy(details.reason)||`${name} needs a saved pass to continue. Unlock once — Passes stays unlocked until you quit Phoenix.`,signal:3,label:"Once per session · stays on this device",tone:"green"};
    if(action==="teach_workflow")return{title:`Teach ${name} this workflow?`,body:safeRuntimeCopy(details.workflow_goal)||"Phoenix will open the teaching surface and save only the workflow you demonstrate.",signal:2,label:"Reusable workflow",tone:"orange"};
    if(action==="permanent_agent")return{title:q.question||`Add ${model.approval.subject||"this coworker"} permanently?`,body:"This changes the company roster. Review the proposed role before continuing.",signal:2,label:"Company change",tone:"orange"};
    if(action==="outside_group_call")return{title:q.question||`Let ${name} contact someone outside this group?`,body:"The action crosses the current group boundary and applies only to this request.",signal:2,label:"Outside this group",tone:"orange"};
    return{title:q.question||`${name} needs a decision`,body:"Review the recommendation and choose an alternative if needed.",signal:1,label:"Your decision",tone:"neutral"};
  }
  function approvalMeterMarkup(signal,tone){return`<span class="approval-meter tone-${tone}" aria-hidden="true">${[0,1,2].map((bar)=>`<i class="${bar<signal?"on":""}"></i>`).join("")}</span>`;}
  function renderApproval(ask) {
    if(ask.status&&ask.status!=="pending"){renderAskHistory(ask);return;}
    if (document.querySelector(`[data-ask-id="${CSS.escape(ask.id)}"]`)) return;
    const approval=(ask.approval?.action==="teach_workflow"&&ask.approval.approved_option!=="Teach now")?{}:(ask.approval||{}), supplied=Array.isArray(ask.questions)&&ask.questions.length?ask.questions:[{question:ask.question||"",options:ask.options||[],header:null,multi_select:false}],questionText=supplied.map((item)=>item.question||"").join(" "),login=approval.action==="login_request"||(!approval.action&&supplied.length===1&&/log in|credential|sign in/i.test(supplied[0].question)),teaching=approval.action==="teach_workflow",vault=approval.action==="vault_unlock"||/vault (?:is )?locked|unlock (?:the )?(?:credential )?vault/i.test(questionText),decision=DECISION_APPROVAL_ACTIONS.has(approval.action);
    const internalLoginOwner=String(approval.details?.agent_id||"").trim(),loginOwner=agentLabel(ask.agent||internalLoginOwner),internalLoginPrefix=internalLoginOwner?`${internalLoginOwner} needs`:"";
    const questions=supplied.map((question,index)=>{const rawQuestion=String(question.question||""),visibleQuestion=login&&internalLoginPrefix&&rawQuestion.startsWith(internalLoginPrefix)?`${loginOwner} needs${rawQuestion.slice(internalLoginPrefix.length)}`:rawQuestion;return {...question,question:visibleQuestion,header:question.header||`Question ${index+1}`,options:Array.isArray(question.options)&&question.options.length?question.options:(login?["Import from browser","Log in for them","Create account"]:approval.action?[approval.approved_option||"Approve","Not now"]:[])};});
    const feed=$("conversationFeed");
    if(![...feed.querySelectorAll(".decision-request")].some(node=>node.dataset.decisionAskId===String(ask.id))){
      // The question is the agent's reply: shown like any other message,
      // with the answer card right under it.
      feedNode("message-row agent-message decision-request",`${avatar(agentProfile(ask.agent||"phoenix"))}<div class="message-content"><header><strong>${escape(agentLabel(ask.agent||"phoenix"))}</strong></header><div class="markdown">${questions.map(q=>markdown(q.question)).join("<br>")}</div></div>`,{decisionAskId:String(ask.id),from:"assistant"});
    }
    if(approval.action==="pass_request"&&window.PhoenixPasses){
      const card=passRequestCard(ask,approval,questions);
      $("approvalStack").append(card);syncApprovalStack();if(!state.painting)scrollLatest();
      if(!state.painting)requestAnimationFrame(()=>card.querySelector("input:not([readonly])")?.focus({preventScroll:true}));
      if(!state.painting&&!preview&&document.hidden&&notificationEnabled("attention"))ui.notify({title:`${agentLabel(ask.agent)} needs ${approval.details?.title||"a pass"}`,body:questions[0]?.question||"Phoenix is waiting for you.",item:{kind:"agent",id:ask.agent||"phoenix"},external:true});
      return;
    }
    const card=document.createElement("article");card.className=`approval-card ${decision?"approval-decision-card":"approval-question-card"}`;card.dataset.askId=ask.id;
    card.dataset.agent=approval.details?.owner_agent_id||approval.details?.agent_id||ask.agent||"phoenix";card.dataset.login=login?"true":"false";card.dataset.teaching=teaching?"true":"false";card.dataset.site=approval.details?.site||"";card.dataset.startUrl=approval.details?.start_url||"";card.dataset.teachingScope=approval.details?.scope||"agent";card.dataset.groupId=approval.details?.group_id||"";
    card._conversationScope={identity:conversationIdentity(),sessionId:state.sessionId,owner:canvasConversationOwner()};
    card._askState={ask,approval,questions,index:0,answers:Array(questions.length).fill(null),multiSelections:Array.from({length:questions.length},()=>new Set()),customAnswers:Array(questions.length).fill(""),decisionSelection:0,alternativesOpen:false,login,teaching,vault,decision,cardAgent:card.dataset.agent};
    const kit=window.PhoenixAgentKit;
    const custom=`<div class="approval-custom" hidden><label><span>Your answer</span><input data-ask-custom-input autocomplete="off" placeholder="Type an answer"></label><button class="primary" data-ask-custom-save>Continue</button></div>`;
    const vaultForm=`<form class="approval-vault" hidden><label><span>Passes master password</span><input data-vault-password type="password" required minlength="12" autocomplete="current-password" placeholder="Unlock once for this session"></label><div class="approval-vault-actions"><button type="button" data-vault-cancel>Not now</button><button class="primary" type="submit" data-vault-submit>Unlock and continue</button></div></form>`;
    card.dataset.state="pending";
    // Decisions follow beUI ToolApproval; questions follow beUI ApprovalCard.
    card.innerHTML=decision
      ?`<div class="ta-head"><span class="ta-tile">${kit.icon("shieldCheck")}</span><div class="ta-main"><div class="ta-row"><div class="ta-titles"><div class="approval-question ta-title"></div><div class="ta-tool"></div></div><span class="ta-badge">Approval required</span></div><p class="approval-decision-copy ta-desc"></p><button type="button" class="ta-details-toggle" data-ta-details aria-expanded="false" hidden>View details${kit.icon("chevronDown","ta-chevron")}</button></div></div><div class="ta-details" hidden><dl class="ta-params"></dl></div>${custom}${vaultForm}<footer class="approval-decision-footer ta-foot"></footer>`
      :`<div class="qa-main"><div class="qa-row"><h3 class="approval-question qa-title" tabindex="-1"></h3><span class="approval-position qa-pos"></span><button type="button" class="approval-dismiss qa-dismiss" data-ask-dismiss aria-label="Dismiss">${kit.icon("x")}</button></div><div class="qa-body"><p class="qa-desc" hidden></p><div class="approval-choice-list qa-options" role="radiogroup"></div></div>${custom}<div class="qa-foot"><button type="button" class="qa-back" data-ask-nav="back" aria-label="Previous question">${kit.icon("arrowLeft")}</button><span class="qa-dots"></span><label class="approval-inline-custom qa-custom"><input data-ask-custom-input autocomplete="off" placeholder="Add another response…" aria-label="Custom answer"></label><button type="button" class="qa-next primary" data-ask-continue></button></div></div>`;
    window.PhoenixQuestionDrafts?.restore(card);renderApprovalQuestion(card);
    if(ask.recovered_unplaced){const provenance=document.createElement("small");provenance.className="saved-request-provenance";const date=new Date(ask.created_at);provenance.textContent="Saved request"+(Number.isFinite(date.getTime())?` · ${date.toLocaleString()}`:"");card.prepend(provenance);}
    if(!decision){
      const details=document.createElement("details"),summary=document.createElement("summary");details.className="pending-question-disclosure";
      summary.innerHTML=`<strong>${escape(agentLabel(ask.agent||"phoenix"))} has a question</strong><span>${escape(questions[0]?.question||"Your input is needed")}</span>`;
      details.append(summary,...card.childNodes);card.append(details);
      const key=`phoenix-question-open:${conversationIdentity()}:${ask.id}`;
      try{details.open=localStorage.getItem(key)==="true";}catch{}
      details.addEventListener("toggle",()=>{try{localStorage.setItem(key,String(details.open));}catch{}syncComposerEnd();});
    }
    $("approvalStack").append(card);syncApprovalStack();if(!state.painting)scrollLatest();
    // The inline card is the canonical request surface while Phoenix is open.
    // Only raise an OS notification when the app is actually in the background;
    // duplicating the same request as a corner toast competes with the question.
    if(!state.painting&&!preview&&document.hidden&&notificationEnabled("attention"))ui.notify({title:login?`${agentLabel(ask.agent)} needs a login`:`${agentLabel(ask.agent)} needs your input`,body:questions[0]?.question||"Phoenix is waiting for you.",item:{kind:"agent",id:ask.agent||"phoenix"},external:true});
  }
  // ask_for_pass: a purpose-built popup that renders exactly the fields the
  // agent asked for. The secret goes UI → gateway Vault (sealed to Passes);
  // the agent's answer is only the metadata receipt the gateway returns.
  function passRequestCard(ask,approval,questions){
    const P=window.PhoenixPasses,details=approval.details||{},kind=P.KINDS[details.kind]?details.kind:"secret",meta=P.KINDS[kind];
    let fields=[],labels={};try{fields=JSON.parse(details.fields||"[]");}catch{}try{labels=JSON.parse(details.labels||"{}");}catch{}
    if(kind==="login"&&!fields.includes("site"))fields.unshift("site");
    if(kind==="api_key"&&!fields.includes("service"))fields.unshift("service");
    const name=agentLabel(ask.agent||details.agent_id||"phoenix"),title=details.title||meta.label,site=details.site||"";
    const values={site,service:site,username:details.username_hint||""};
    const optional=kind==="login"&&fields.includes("totp");
    const fieldNames=optional?fields.filter((field)=>field!=="totp"):fields;
    const card=document.createElement("article");
    card.className="approval-card pass-request-card";card.dataset.askId=ask.id;card.dataset.agent=details.agent_id||ask.agent||"phoenix";card.dataset.kind=kind;card.dataset.state="pending";
    card._conversationScope={identity:conversationIdentity(),sessionId:state.sessionId,owner:canvasConversationOwner()};
    card._askState={ask,approval,questions,index:0,answers:Array(questions.length).fill(null),multiSelections:questions.map(()=>new Set()),customAnswers:questions.map(()=>""),decision:true,pass:true,cardAgent:card.dataset.agent};
    card.innerHTML=`<div class="pr-head"><span class="pr-tile pr-tile-${kind}">${P.icons[meta.icon]}</span><div class="pr-titles"><h3 class="pr-title" tabindex="-1">${escape(title)}</h3><p class="pr-reason">${escape(questions[0]?.question||`${name} needs this to continue.`)}</p></div><button type="button" class="approval-dismiss pr-dismiss" data-pass-cancel aria-label="Not now">${window.PhoenixAgentKit?.icon("x")||"×"}</button></div>
      <form class="pr-form" autocomplete="off" novalidate>
        <div class="pr-fields pr-fields-${kind}">${P.renderFields(kind,fieldNames,{labels,values,lockSite:Boolean(site)})}</div>
        ${optional?`<details class="pr-more"><summary>Add 2FA setup key <small>optional — lets ${escape(name)} fill your 6-digit codes</small></summary>${P.renderFields(kind,["totp"],{labels})}</details>`:""}
        <footer class="pr-foot"><span class="pr-lock">${P.icons.lock}<span>Encrypted on this device. ${escape(name)} gets a reference, never the value.</span></span><span class="pr-actions"><button type="button" class="ta-btn ghost" data-pass-cancel>Not now</button><button type="submit" class="ta-btn primary" data-pass-save>Save to Passes</button></span></footer>
      </form>`;
    const form=card.querySelector(".pr-form");P.bind(form);
    form.addEventListener("submit",(event)=>{event.preventDefault();savePassRequest(card);});
    form.addEventListener("keydown",(event)=>{if(event.key==="Escape"){event.preventDefault();dismissPassRequest(card);}});
    card.querySelectorAll("[data-pass-cancel]").forEach((button)=>button.addEventListener("click",(event)=>{event.preventDefault();event.stopPropagation();dismissPassRequest(card);}));
    return card;
  }
  function dismissPassRequest(card){window.PhoenixPasses.wipe(card.querySelector(".pr-form"));card.dataset.state="leaving";dismissAsk(card);}
  async function savePassRequest(card){
    const P=window.PhoenixPasses,model=card._askState,details=model.approval.details||{},kind=card.dataset.kind,form=card.querySelector(".pr-form");
    const value=P.collect(form,kind);if(!value){card.classList.remove("pr-shake");void card.offsetWidth;card.classList.add("pr-shake");return;}
    const save=card.querySelector("[data-pass-save]");card.querySelectorAll("button,input").forEach((item)=>item.disabled=true);save.classList.add("busy");save.textContent="Saving…";
    try{
      const reply=await rpc({Vault:{action:"fulfill_request",ask_id:card.dataset.askId,kind,site:value.site||details.site||null,label:details.title||null,username:value.username,metadata_json:JSON.stringify(value.metadata||{}),secret:value.secret,fields:value.fields}},20000);
      P.wipe(form);
      const receipt=reply?.Vault?.answer;if(!receipt)throw new Error("Passes did not confirm the save.");
      card.dataset.state="saved";save.innerHTML=`${P.icons.check}Saved`;
      model.answers=model.questions.map(()=>`Saved to Passes · ${details.title||P.KINDS[kind].label}`);
      await new Promise((resolve)=>setTimeout(resolve,420));
      await submitAsk(card,receipt);
    }catch(error){
      card.querySelectorAll("button,input").forEach((item)=>item.disabled=false);save.classList.remove("busy");save.textContent="Save to Passes";
      ui.toast(error.message||String(error),true);
    }
  }
  function approvalParameters(model,q){
    const details=model.approval.details||{};
    try{const rows=JSON.parse(details.parameters||"null");if(Array.isArray(rows)&&rows.length)return rows.filter((row)=>Array.isArray(row)&&row.length===2).map(([label,value])=>[String(label),String(value)]);}catch{}
    if(model.approval.action==="tool_permission")return[["Tool",details.tool_name||model.approval.subject||""],["Needs",approvalAccessName(details.required_mode)],["Current",approvalAccessName(details.current_mode)]].filter(([,value])=>value);
    if(details.site)return[["Site",String(details.site)]];
    return[];
  }
  function approvalTitle(model,presentation){
    if(model.approval.action!=="governed_effect")return presentation.title;
    const name=agentLabel(model.ask.agent||model.cardAgent||"phoenix"),effect=String(model.approval.details?.effect||"");
    return effect==="external_delete"?`Let ${name} delete this?`:effect==="purchase"?`Let ${name} make this purchase?`:`Let ${name} send this?`;
  }
  const DENY_OPTION=/^(?:deny|do not allow|don't allow|not now|no|skip|cancel)$/i;
  function renderApprovalQuestion(card){
    const kit=window.PhoenixAgentKit,model=card._askState,q=model.questions[model.index],answer=model.answers[model.index],chosen=model.multiSelections[model.index],options=q.options.slice(0,5);
    card.querySelector(".approval-custom").hidden=true;
    card.querySelector(".approval-vault")?.setAttribute("hidden","");
    if(model.decision){
      const presentation=approvalDecisionPresentation(model,q),details=model.approval.details||{},params=approvalParameters(model,q);
      card.querySelector(".approval-question").textContent=approvalTitle(model,presentation);
      card.querySelector(".ta-tool").textContent=[details.tool_name,params.find(([label])=>label==="Action")?.[1]].filter(Boolean).join(" · ")||presentation.label;
      card.querySelector(".approval-decision-copy").textContent=q.question||presentation.body;
      const toggle=card.querySelector("[data-ta-details]");toggle.hidden=!params.length;
      card.querySelector(".ta-params").innerHTML=params.map(([label,value])=>`<div><dt>${escape(label)}</dt><dd>${escape(value)}</dd></div>`).join("");
      const primaryOption=options[0]||model.approval.approved_option||"Allow once",rest=options.slice(1),hasDeny=rest.some((option)=>DENY_OPTION.test(option.trim()));
      const buttons=[`<button type="button" class="ta-btn primary" data-ask-decision-confirm data-ask-option="${escape(primaryOption)}">${escape(primaryOption)}</button>`]
        .concat(rest.map((option)=>`<button type="button" class="ta-btn ${DENY_OPTION.test(option.trim())?"ghost":"secondary"}" data-ask-option="${escape(option)}">${escape(option)}</button>`))
        .concat(hasDeny?[]:[`<button type="button" class="ta-btn ghost" data-ask-dismiss>Deny</button>`]);
      const foot=card.querySelector(".ta-foot");foot.innerHTML=buttons.join("");
      foot.querySelector("[data-ask-decision-confirm]").toggleAttribute("data-vault-open",model.vault&&/unlock/i.test(primaryOption));
    }else{
      const last=model.index===model.questions.length-1,multiple=model.questions.length>1,multi=Boolean(q.multi_select);
      card.querySelector(".approval-question").textContent=q.question;
      const desc=card.querySelector(".qa-desc"),header=String(q.header||"");desc.hidden=!header||/^Question \d+$/.test(header);desc.textContent=header;
      card.querySelector(".approval-position").textContent=multiple?`${model.index+1}/${model.questions.length}`:"";
      const list=card.querySelector(".approval-choice-list");list.setAttribute("role",multi?"group":"radiogroup");
      list.innerHTML=options.map((option)=>{const selected=multi?chosen.has(option):answer===option;return`<button type="button" class="approval-choice qa-option ${selected?"selected":""}" role="${multi?"checkbox":"radio"}" aria-checked="${selected}" data-ask-option="${escape(option)}"><i class="${multi?"qa-check":"qa-radio"}">${multi?kit.icon("check"):"<b></b>"}</i><span>${escape(option)}</span></button>`;}).join("");list.hidden=!options.length;
      const customInput=card.querySelector(".qa-custom input");if(customInput&&document.activeElement!==customInput)customInput.value=model.customAnswers[model.index]||"";if(customInput)customInput.placeholder=options.length?"Add another response…":"Type your answer…";
      const back=card.querySelector('[data-ask-nav="back"]');back.hidden=!multiple;back.disabled=model.index===0;
      card.querySelector(".qa-dots").innerHTML=multiple?`<span class="sr-only">Question ${model.index+1} of ${model.questions.length}</span>${model.questions.map((_,index)=>`<i class="${index===model.index?"current":index<model.index?"past":""}"></i>`).join("")}`:"";
      const hasAnswer=Boolean((multi?chosen.size:answer)||model.customAnswers[model.index]?.trim());
      const next=card.querySelector("[data-ask-continue]");next.disabled=!hasAnswer;next.classList.toggle("icon-only",!last);
      next.innerHTML=last?`Submit response${kit.icon("arrowRight")}`:kit.icon("arrowRight");next.setAttribute("aria-label",last?"Submit response":"Next question");
    }
    syncApprovalStack();
  }
  function syncApprovalStack(){
    const stack=$("approvalStack"),count=stack?.children.length||0;if(!stack)return;
    stack.dataset.waiting=count>1?`${count-1} more request${count===2?"":"s"} waiting`:"";
    // The active card is the one visible request surface. Keep its transcript
    // record for replay, but avoid printing the same question above the card.
    const visibleAsk=stack.lastElementChild?.dataset.askId;
    for(const row of $("conversationFeed").querySelectorAll(".decision-request"))
      row.hidden=row.dataset.decisionAskId===visibleAsk;
    syncComposerEnd();
  }
  function formatAskAnswers(model){const answered=model.answers.filter((answer)=>answer!=null);return `Collected ${answered.length} answer(s) from user:\n\n${model.questions.map((question,index)=>`  [${question.header}] Q: ${question.question}\n  A: ${model.answers[index]}`).join("\n\n")}`;}
  // A group-boundary decision covers every open card asking the same thing:
  // same group, same coworker, same permission. The gateway settles those
  // siblings too, so the client closes them with the decision.
  function outsideCallScope(model){const approval=model?.approval||{},details=approval.details||{};return approval.action==="outside_group_call"&&details.group_id&&details.target_agent_id?`${details.group_id}\u0000${details.target_agent_id}`:"";}
  function matchingApprovalCards(card){const scope=outsideCallScope(card._askState);if(!scope)return[];return[...$("approvalStack").querySelectorAll(".approval-card")].filter((other)=>other!==card&&outsideCallScope(other._askState)===scope);}
  function recordAskResolution(card,answer,status){const id=card.dataset.askId,model=card._askState;let entry=state.displayRows.find((row)=>row.source==="story"&&row.value?.kind==="ask_pending"&&(row.value.id||row.value.ask_id)===id);if(!entry){entry={source:"story",value:cloneDisplayValue(model.ask)};state.displayRows.push(entry);}Object.assign(entry.value,{id,status,answer,display_answers:model.answers.map((value)=>value==null?null:String(value)),resolved_at:new Date().toISOString()});return entry;}
  // Resolving a card updates only that card's own nodes. Repainting the whole
  // feed here cleared every approval card and the transcript and rebuilt
  // them, which read as the app reloading after each approval.
  function resolveAskDisplay(card,answer,status){
    window.PhoenixQuestionDrafts?.discard(card);
    const siblings=status==="answered"?matchingApprovalCards(card):[],primaryAnswers=card._askState.answers;
    const entry=recordAskResolution(card,answer,status);
    siblings.forEach((other)=>{window.PhoenixQuestionDrafts?.discard(other);other._askState.answers=other._askState.questions.map((_,index)=>primaryAnswers[index]??primaryAnswers.at(-1)??null);recordAskResolution(other,answer,status);});
    replaceDisplayRows(trimDisplayRows(state.displayRows),true);
    const feed=$("conversationFeed"),bookmark=conversationScrollBookmark(),primaryId=String(card.dataset.askId||"");
    const anchor=[...feed.querySelectorAll(".decision-request")].find((request)=>request.dataset.decisionAskId===primaryId)||null;
    [card,...siblings].forEach((node)=>{const id=String(node.dataset.askId||"");feed.querySelectorAll(".decision-request").forEach((request)=>{if(request!==anchor&&request.dataset.decisionAskId===id)request.remove();});node.remove();});
    // The answer takes the question's own place in the transcript, exactly
    // where a full repaint would put it, without rebuilding the feed.
    const resolved=renderAskHistory(entry.value);
    if(anchor){if(resolved&&resolved!==anchor&&resolved.classList.contains("answer-resume-message"))anchor.replaceWith(resolved);else anchor.remove();}
    syncApprovalStack();restoreConversationScroll(bookmark);scheduleDisplayPersist(true);
  }
  // A card whose decision was already made elsewhere (a sibling card, another
  // window, a turn that ended) is obsolete, not failed.
  const OBSOLETE_ASK=/no longer pending|already (?:answered|resolved)|different saved decision|no active participant|continuation is unavailable|submission is unavailable/i;
  async function submitAsk(card,answer){
    const token=activeSelectionToken(),scope=card._conversationScope;
    // The question keeps its original owner across cached views and callbacks.
    if(scope&&scope.identity!==conversationIdentity()){
      ui.toast("Open the conversation that asked this question to answer it.",true);return;
    }
    const session_id=scope?.sessionId||state.sessionId,owner=scope?.owner||canvasConversationOwner();
    card.querySelectorAll("button,input").forEach((item)=>item.disabled=true);
    try{
      await rpc({AnswerAsk:{ask_id:card.dataset.askId,answer,session_id,owner}},8000,token?.signal);
      if(selectionIsCurrent(token))resolveAskDisplay(card,answer,"answered");
    }catch(error){
      if(!selectionIsCurrent(token))return;
      if(card._askState?.decision&&OBSOLETE_ASK.test(String(error?.message||error))){resolveAskDisplay(card,answer,"answered");return;}
      card.querySelectorAll("button,input").forEach((item)=>item.disabled=false);ui.toast(error.message,true);
    }
  }
  async function dismissAsk(card){const token=activeSelectionToken(),session_id=state.sessionId,owner=canvasConversationOwner();card.querySelectorAll("button,input").forEach((item)=>item.disabled=true);try{await rpc({DismissAsk:{ask_id:card.dataset.askId,session_id,owner}},8000,token?.signal);if(!selectionIsCurrent(token))return;card._askState.answers=card._askState.questions.map(()=>"Not now");resolveAskDisplay(card,"Not now","dismissed");}catch(error){if(!selectionIsCurrent(token))return;card.querySelectorAll("button,input").forEach((item)=>item.disabled=false);ui.toast(error.message,true);}}
  function recordAskAnswer(card,answer){const model=card._askState;model.answers[model.index]=answer;if(model.index<model.questions.length-1){model.index+=1;renderApprovalQuestion(card);card.querySelector(".approval-question").focus?.();return;}submitAsk(card,formatAskAnswers(model));}
  async function unlockApprovalVault(card){const input=card.querySelector("[data-vault-password]"),password=input.value;if(password.length<12){input.reportValidity();return;}card.querySelectorAll("button,input").forEach((item)=>item.disabled=true);try{await rpc({Vault:{action:"unlock_with_password",master_password:password}},20000);input.value="";card._askState.answers[card._askState.index]="Unlocked";await submitAsk(card,"A: Unlock here\nPasses is unlocked for this Phoenix session. Continue the blocked action now.");}catch(error){card.querySelectorAll("button,input").forEach((item)=>item.disabled=false);input.focus();ui.toast(error.message,true);}}
  function closeApproval(){window.PhoenixQuestionDrafts?.persistAll();$("approvalStack").replaceChildren();syncApprovalStack();}
  function answerApproval(button){const card=button.closest(".approval-card"),model=card._askState,answer=button.dataset.askOption;if(model.questions[model.index].multi_select){const selected=model.multiSelections[model.index];selected.has(answer)?selected.delete(answer):selected.add(answer);renderApprovalQuestion(card);return;}
    if(!model.decision){const index=model.index;model.answers[index]=answer;model.customAnswers[index]="";renderApprovalQuestion(card);clearTimeout(model.advanceTimer);if(index<model.questions.length-1)model.advanceTimer=setTimeout(()=>{if(card.isConnected&&model.index===index){model.index+=1;renderApprovalQuestion(card);card.querySelector(".approval-question")?.focus?.({preventScroll:true});}},240);return;}
    if(model.decision){card.dataset.state="approving";card.querySelector(".ta-badge")&&(card.querySelector(".ta-badge").textContent=DENY_OPTION.test(String(answer).trim())?"Denying":"Approving");}if(model.login&&/log in|sign in/i.test(answer)){state.loginAsk={answer,card};card.querySelectorAll("button").forEach((item)=>item.disabled=true);openBrowser(card.dataset.agent||state.item?.id||"phoenix","login",card.dataset.site?`https://${card.dataset.site}`:null).catch((error)=>{card.querySelectorAll("button").forEach((item)=>item.disabled=false);state.loginAsk=null;ui.toast(error.message||String(error),true);});return;}if(model.teaching&&answer===model.approval.approved_option){state.teachingAsk={answer,card};card.querySelectorAll("button,input").forEach((item)=>item.disabled=true);startTeaching(card.dataset.agent||state.item?.id||"phoenix",card.dataset.startUrl||null,card.dataset.teachingScope||"agent",card.dataset.groupId||null);return;}recordAskAnswer(card,answer);}
  function approvalAction(button){
    const card=button.closest(".approval-card"),model=card._askState;
    if(button.hasAttribute("data-vault-cancel")){renderApprovalQuestion(card);return;}
    if(button.hasAttribute("data-vault-open")){card.querySelector(".approval-decision-footer")?.setAttribute("hidden","");card.querySelector(".approval-alternatives")?.setAttribute("hidden","");card.querySelector(".approval-vault").hidden=false;card.querySelector("[data-vault-password]").focus();return;}
    if(button.hasAttribute("data-ta-details")){const panel=card.querySelector(".ta-details"),open=panel.hidden;panel.hidden=!open;button.setAttribute("aria-expanded",String(open));return;}
    if(button.hasAttribute("data-ask-alternatives")){model.alternativesOpen=!model.alternativesOpen;renderApprovalQuestion(card);return;}
    if(button.dataset.askDecisionSelect!=null){model.decisionSelection=Number(button.dataset.askDecisionSelect)||0;model.alternativesOpen=false;renderApprovalQuestion(card);return;}
    if(button.hasAttribute("data-ask-decision-confirm")){answerApproval(button);return;}
    if(button.dataset.askOption!=null){answerApproval(button);return;}
    if(button.dataset.askNav){model.index=Math.min(model.questions.length-1,Math.max(0,model.index+(button.dataset.askNav==="back"?-1:1)));renderApprovalQuestion(card);return;}
    if(button.hasAttribute("data-ask-skip")){if(model.index===model.questions.length-1){dismissAsk(card);return;}model.answers[model.index]="Skipped";model.index+=1;renderApprovalQuestion(card);return;}
    if(button.hasAttribute("data-ask-custom")){card.querySelector(".approval-alternatives")?.setAttribute("hidden","");card.querySelector(".approval-custom").hidden=false;card.querySelector("[data-ask-custom-input]").focus();return;}
    if(button.hasAttribute("data-ask-custom-save")){const value=card.querySelector(".approval-custom [data-ask-custom-input]").value.trim();if(value)recordAskAnswer(card,value);return;}
    if(button.hasAttribute("data-ask-continue")){const custom=model.customAnswers[model.index]?.trim();if(custom){recordAskAnswer(card,custom);return;}if(model.questions[model.index].multi_select&&model.multiSelections[model.index].size){recordAskAnswer(card,[...model.multiSelections[model.index]].join(", "));return;}if(model.answers[model.index])recordAskAnswer(card,model.answers[model.index]);return;}
    if(button.hasAttribute("data-ask-multi")){recordAskAnswer(card,[...model.multiSelections[model.index]].join(", "));return;}
    if(button.hasAttribute("data-ask-dismiss"))dismissAsk(card);
  }

  function formatTokenCount(n){n=Number(n)||0;return n>=1e6?`${(n/1e6).toFixed(n>=1e7?0:1)}M`:n>=1e3?`${Math.round(n/1e3)}K`:String(n);}
  // A conversation's own gauge is measured against the window chosen on its
  // Context slider, which can differ from the window a saved sample reported.
  function contextPercent(usage){const chosen=usage===state.usage&&state.item?.kind!=="group"?Number(selectedModelContext().effective):0,limit=chosen||Number(usage?.limit)||1;return Math.min(100,Math.round((Number(usage?.used)||0)/Math.max(1,limit)*100));}
  function contextUsageStorageKey(session,agent=""){return`phoenix-context-usage:${session}:${canonicalAgentId(agent)||"owner"}`;}
  function storedContextUsage(session,agent=""){try{const usage=JSON.parse(localStorage.getItem(contextUsageStorageKey(session,agent))||"null"),used=Number(usage?.used),limit=Number(usage?.limit);return Number.isFinite(used)&&used>=0&&Number.isFinite(limit)&&limit>1?{used,limit}:null;}catch{return null;}}
  function persistContextUsage(session,agent,usage){if(!session||!usage?.limit)return;try{localStorage.setItem(contextUsageStorageKey(session,agent),JSON.stringify({used:Number(usage.used)||0,limit:Number(usage.limit)||1}));}catch{}}
  function groupContextUsages(session=state.sessionId){const prefix=`${session}:`;return[...state.usageByConversationAgent.entries()].filter(([key])=>key.startsWith(prefix)).map(([key,usage])=>({agent:key.slice(prefix.length),...usage})).sort((a,b)=>contextPercent(b)-contextPercent(a));}
  function paintContext(){paintContextTokens();const percent=contextPercent(state.usage),known=Number(state.usage?.limit)>1;$("contextPercent").textContent=known?percent:"—";$("contextButton").setAttribute("aria-label",known?(state.item?.kind==="group"?`Highest coworker context usage ${percent}%`:`Context window usage ${percent}%`):"Context usage is not available until the model reports it");}
  function restoreContext(session=state.sessionId){const groupRows=state.item?.kind==="group"?groupContextUsages(session):[];state.usage={...(groupRows[0]||state.usageBySession.get(session)||{used:0,limit:1})};paintContext();}
  function updateContext(used,limit,session=state.sessionId,agent=null){const usage={used:Number(used)||0,limit:Number(limit)||1},agentKey=agent?canonicalAgentId(agent):"";if(session&&agentKey)state.usageByConversationAgent.set(`${session}:${agentKey}`,usage);if(session&&(!agent||state.item?.kind!=="group"))state.usageBySession.set(session,usage);if(session!==state.sessionId)return;if(state.item?.kind==="group"){const rows=groupContextUsages(session);state.usage={...(rows[0]||usage)};}else state.usage=usage;paintContext();}
  function resetGroupContextSamples(session=state.sessionId){const prefix=`${session}:`;for(const key of state.usageByConversationAgent.keys())if(key.startsWith(prefix))state.usageByConversationAgent.delete(key);if(state.item?.kind==="group"&&session===state.sessionId){state.usage={used:0,limit:1};paintContext();}}
  function openContext(){const rows=state.item?.kind==="group"?groupContextUsages():[],known=Number(state.usage?.limit)>1,percent=contextPercent(state.usage),perAgent=rows.length?`<div class="context-agent-list">${rows.map((usage)=>`<div><span><strong>${escape(agentLabel(usage.agent))}</strong><small>${formatTokenCount(usage.used)} of ${formatTokenCount(usage.limit)}</small></span><b>${contextPercent(usage)}%</b></div>`).join("")}</div>`:"";ui.openPopover($("contextButton"),known?`<div class="popover-label">${rows.length?"Coworker context windows":"Context window"}</div><div class="context-stat"><strong>${percent}%</strong><span>${rows.length?"highest live sample":"used"}</span></div><div class="context-meter"><i style="width:${percent}%"></i></div><div class="context-meta"><span><small>In context</small><strong>${formatTokenCount(state.usage.used)}</strong></span><span><small>Capacity</small><strong>${formatTokenCount(state.usage.limit)}</strong></span></div>${perAgent}`:`<div class="popover-label">Context window</div><div class="popover-empty">Waiting for a live model usage sample. Phoenix no longer reuses an old percentage from a previous app session.</div>`,"context-popover");}
  function formatContextWindow(n){const value=Number(n)||0;if(value>=1e6)return`${(value/1e6).toFixed(value%1e6?2:0).replace(/0+$/," ").trim().replace(/\.$/,"")}M`;if(value>=1e3)return`${Math.round(value/1e3)}K`;return String(value);}
  function selectedModelContext(){const lane=state.selectedLane||laneSetting(),maximum=Number(lane?.max_context_window||state.selectedModel?.context_window||lane?.context_window||0),override=Number(lane?.context_window_override)||0,effective=Number(lane?.context_window)||(override||maximum);return{lane,maximum,override:override||null,effective:effective||maximum};}
  function liquidFill(node,value){if(node)node.style.width=`${Math.max(0,Math.min(100,Number(value)||0))}%`;}
  function syncModelContextLabel(){queueMicrotask(paintContext);
    const label=$("composerContext"),range=$("contextRange"),control=$("contextControl");if(!label||!range||!control)return;
    if(state.item?.kind==="group"){
      const rows=groupModelRows(),ratios=rows.map(({lane})=>{const max=Number(lane?.max_context_window||lane?.context_window||0),value=Number(lane?.context_window||max);return max>0?value/max*100:0;}).filter(Number.isFinite);
      label.textContent="Team";range.disabled=true;range.setAttribute("aria-label","Team context windows; open for per-agent details");control.classList.add("group-summary");liquidFill($("contextRangeFill"),ratios.length?Math.max(...ratios):0);return;
    }
    control.classList.remove("group-summary");
    const context=selectedModelContext(),maximum=Math.max(8192,Number(context.maximum)||8192),effective=Math.max(8192,Math.min(maximum,Number(context.effective)||maximum)),value=context.effective?formatContextWindow(context.effective):"Context";
    label.textContent=value;range.disabled=!context.maximum;range.min=String(Math.min(8192,maximum));range.max=String(maximum);range.step="1";range.value=String(effective);range.setAttribute("aria-label",context.effective?`Context window ${value} of ${formatContextWindow(context.maximum)} maximum`:"Context window unavailable");liquidFill($("contextRangeFill"),context.maximum?effective/maximum*100:0);
  }
  function syncReasoningSlider(){
    const label=$("composerReasoning"),range=$("reasoningRange"),control=$("reasoningControl");if(!label||!range||!control)return;
    if(state.item?.kind==="group"){
      const rows=groupModelRows(),scale=["minimal","low","medium","high","xhigh","max"],values=rows.map(({lane})=>scale.indexOf(String(lane?.reasoning_effort||"").toLowerCase())).filter((value)=>value>=0);
      label.textContent="Per agent";range.disabled=true;range.min="0";range.max="5";range.value=String(values.length?Math.round(values.reduce((a,b)=>a+b,0)/values.length):0);range.setAttribute("aria-label","Reasoning is configured per agent; open for details");control.classList.add("group-summary");liquidFill($("reasoningRangeFill"),values.length?Number(range.value)/5*100:0);return;
    }
    control.classList.remove("group-summary");
    const levels=selectedEffortLevels(),choices=[null,...levels],index=Math.max(0,choices.indexOf(state.reasoning));
    label.textContent=state.reasoning?state.reasoning[0].toUpperCase()+state.reasoning.slice(1):levels.length?"Default":"Built in";const speed=codexSpeed();if(speed!=="standard")label.textContent+=speed==="fast"?" ⚡":" ⚡⚡";range.disabled=!levels.length;range.min="0";range.max=String(Math.max(1,choices.length-1));range.value=String(index);range.setAttribute("aria-label",`Reasoning effort ${label.textContent}`);liquidFill($("reasoningRangeFill"),choices.length>1?index/(choices.length-1)*100:0);
  }
  function applyLocalModelContext(requested){const laneId=modelLane(),maximum=selectedModelContext().maximum;if(!laneId||!maximum)return;const effective=requested==null?maximum:Number(requested),next={...(state.selectedLane||{lane:laneId,provider_id:state.selectedModel?.provider_id||state.selectedModel?.provider,model:state.selectedModel?.id}),context_window:effective,max_context_window:maximum,context_window_override:effective<maximum?effective:null};state.selectedLane=next;if(state.modelSnapshot){const lanes=state.modelSnapshot.lanes||(state.modelSnapshot.lanes=[]),index=lanes.findIndex((entry)=>entry.lane===laneId||(laneId==="phoenix"&&entry.lane==="orchestrator"));if(index>=0)lanes[index]=next;else lanes.push(next);}syncModelContextLabel();}
  async function saveModelContextWindow(requested){const lane=modelLane(),context=selectedModelContext(),maximum=context.maximum,value=requested==null?null:Number(requested);if(!lane||!maximum)return;if(value!=null&&(!Number.isInteger(value)||value<8192||value>maximum)){ui.toast(`Choose between 8,192 and ${maximum.toLocaleString()} tokens.`,true);return;}const previous={lane:state.selectedLane?{...state.selectedLane}:null};applyLocalModelContext(value);ui.closeLayers();if(preview){ui.toast(value==null?`Using ${formatContextWindow(maximum)} model maximum.`:`Context limited to ${formatContextWindow(value)}.`);return;}try{const reply=await rpc({Settings:{action:"set_model_context_window",lane,context_window:value,expected_config_revision:state.modelSnapshot?.config_revision||null}}),snapshot=reply.Settings?.snapshot||reply.Settings?.models;if(snapshot)state.modelSnapshot=snapshot;await refreshModels();ui.toast(value==null?`Using ${formatContextWindow(maximum)} model maximum.`:`Context limited to ${formatContextWindow(value)}.`);}catch(error){state.selectedLane=previous.lane;syncModelContextLabel();ui.toast(error.message||String(error),true);}}
  // Context and reasoning open beUI's gooey Popover holding an InlineSlider.
  // The hidden range inputs stay the source of truth: the slider writes them
  // and fires their input/change handlers, which label and persist as before.
  function pushRange(id,value,commit){const range=$(id);if(!range)return;range.value=String(value);range.dispatchEvent(new Event("input",{bubbles:true}));if(commit)range.dispatchEvent(new Event("change",{bubbles:true}));}
  function openGooContext(){
    if(state.item?.kind==="group"){openModelContext();return;}
    const context=selectedModelContext(),maximum=Number(context.maximum)||0;if(!maximum){openModelContext();return;}
    const kit=window.PhoenixAgentKit,min=8192,effective=Number(context.effective)||maximum;
    // Continuous: a stop every 1K between the minimum and the model maximum,
    // plus the current setting, so the slider stops anywhere.
    const values=[...new Set([min,...Array.from({length:Math.max(0,Math.floor(maximum/1000)-Math.ceil(min/1000)+1)},(_,i)=>(Math.ceil(min/1000)+i)*1000).filter((value)=>value>min&&value<maximum),effective,maximum])].filter((value)=>value>=min&&value<=maximum).sort((a,b)=>a-b);
    const stops=values.map((value)=>({value,label:formatContextWindow(value)}));
    // Typing "500k", "1.05m" or "350000" picks that size (nearest 1K).
    const typedIndex=(text)=>{const match=String(text||"").trim().toLowerCase().replace(/[,\s_]/g,"").match(/^(\d+(?:\.\d+)?)(k|m)?(?:tokens?)?$/);if(!match){ui.toast("Type a size like 355K, 1.05M or 400000.",true);return null;}
      const wanted=Math.round(Number(match[1])*(match[2]==="m"?1e6:match[2]==="k"?1e3:1));if(wanted<min||wanted>maximum){ui.toast(`Choose between ${formatContextWindow(min)} and ${formatContextWindow(maximum)}.`,true);return null;}
      return values.reduce((best,value,i)=>Math.abs(value-wanted)<Math.abs(values[best]-wanted)?i:best,0);};
    const start=values.reduce((best,value,i)=>Math.abs(value-effective)<Math.abs(values[best]-effective)?i:best,0);
    const used=Number(state.usage?.limit)>1?Number(state.usage?.used)||0:0,percent=contextPercent(state.usage);
    const content=document.createElement("div");content.className="goo-content";
    content.innerHTML=`<div class="goo-head"><strong>Context window</strong><small>${used?`${formatTokenCount(used)} in context · ${percent}%`:"No usage reported yet"}</small></div>`;
    const slider=kit.inlineSlider({label:"Context",stops,index:start,ariaLabel:"Context window",typed:typedIndex,onInput:(stop)=>pushRange("contextRange",stop.value,false),onCommit:(stop)=>pushRange("contextRange",stop.value,true)});
    content.append(slider.root);
    content.insertAdjacentHTML("beforeend",`<p class="goo-note">Max for ${escape(state.selectedModel?.name||state.selectedModel?.id||"this model")}: ${formatContextWindow(maximum)}</p>`);
    kit.gooPopover($("contextControl"),content,{side:"top",align:"start"});
  }
  function codexSpeedSupport(){
    const model=state.selectedModel,provider=model?.provider_id||model?.provider;
    const fast=provider==='openai-codex'&&['gpt-6.1-sol','gpt-6-astra','gpt-6-sol','gpt-6-luna','gpt-5.6-sol','gpt-5.6-terra','gpt-5.6-luna','gpt-5.5'].includes(model?.id);
    return {fast,ultrafast:fast&&['gpt-6.1-sol','gpt-6-astra'].includes(model.id)};
  }
  function codexSpeed(){
    const support=codexSpeedSupport(),tier=state.selectedLane?.service_tier||laneSetting()?.service_tier||'standard';
    return support[tier]?tier:'standard';
  }
  async function saveCodexSpeed(tier,content){
    const lane=modelLane(),token=activeSelectionToken(),previous=state.selectedLane?{...state.selectedLane}:null;
    if(tier!=='standard'&&!codexSpeedSupport()[tier])return;
    state.selectedLane={...(state.selectedLane||laneSetting()||{}),service_tier:tier};
    const paint=()=>content.querySelectorAll('[data-codex-speed]').forEach(button=>button.setAttribute('aria-pressed',String(button.dataset.codexSpeed===codexSpeed())));
    paint();syncReasoningSlider();
    if(preview){localStorage.setItem('phoenix-speed:'+lane,tier);return;}
    const buttons=[...content.querySelectorAll('[data-codex-speed]')];buttons.forEach(button=>button.disabled=true);
    try{
      await rpc({Settings:{action:'set_model_service_tier',lane,service_tier:tier,expected_config_revision:state.modelSnapshot?.config_revision||null}});
      if(selectionIsCurrent(token))await refreshModels(token);
    }catch(error){if(selectionIsCurrent(token)){state.selectedLane=previous;syncReasoningSlider();}ui.toast(error.message||String(error),true);}
    finally{if(content.isConnected&&selectionIsCurrent(token)){const support=codexSpeedSupport();buttons.forEach(button=>button.disabled=!support[button.dataset.codexSpeed]);paint();}}
  }
  function addCodexSpeedControls(content){
    if(!codexSpeedSupport().fast)return;
    const row=document.createElement('div');row.className='codex-speed-controls';row.setAttribute('role','group');row.setAttribute('aria-label','Codex speed');
    const fastTip='Fast: 2.5× included subscription usage; 2× purchased credits. Click again for Standard.';
    const ultraTip='Ultrafast: 8× included subscription usage; 6× purchased credits. Requires $500 Pro or eligible Enterprise/Edu. GPT-6.1 Sol and GPT-6 Astra only. Click again for Standard.';
    const bolt='<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m11.5 2-7 9H10l-1.5 7 7-9H10z"/></svg>';
    row.innerHTML=['fast','ultrafast'].map(tier=>'<button type="button" data-codex-speed="'+tier+'" aria-label="'+(tier==='fast'?'Fast mode':'Ultrafast mode')+'" aria-pressed="'+(codexSpeed()===tier)+'" title="'+(tier==='fast'?fastTip:ultraTip)+'">'+bolt+(tier==='ultrafast'?bolt:'')+'</button>').join('');
    row.querySelector('[data-codex-speed="ultrafast"]').disabled=!codexSpeedSupport().ultrafast;
    row.onclick=event=>{const button=event.target.closest('[data-codex-speed]');if(button&&!button.disabled)saveCodexSpeed(codexSpeed()===button.dataset.codexSpeed?'standard':button.dataset.codexSpeed,content);};
    content.querySelector('.goo-head').append(row);
  }
  function openGooReasoning(){
    if(state.item?.kind==="group"){openReasoning();return;}
    const levels=selectedEffortLevels();if(!levels.length){openReasoning();return;}
    const kit=window.PhoenixAgentKit,choices=[null,...levels],stops=choices.map((value)=>({value,label:value?value[0].toUpperCase()+value.slice(1):"Default"})),last=stops.length-1;
    const content=document.createElement("div");content.className="goo-content";
    content.innerHTML=`<div class="goo-head"><strong>Reasoning effort</strong><small>${escape(state.selectedModel?.name||state.selectedModel?.id||"")}</small></div>`;
    const slider=kit.inlineSlider({label:"Reasoning",stops,index:Math.max(0,choices.indexOf(state.reasoning)),ariaLabel:"Reasoning effort",onInput:(_,i)=>pushRange("reasoningRange",i,false),onCommit:(_,i)=>pushRange("reasoningRange",i,true)});
    // The top effort glows a lot, the one before it a little: the whole bar.
    const glow=(i)=>{slider.root.dataset.glow=i===last&&last>=2?"max":i===last-1&&last>=3?"pre":"";};
    slider.root.addEventListener("is-change",(event)=>glow(event.detail.index));glow(Math.max(0,choices.indexOf(state.reasoning)));
    content.append(slider.root);
    addCodexSpeedControls(content);
    kit.gooPopover($("reasoningControl"),content,{side:"top",align:"start"});
  }
  function openModelContext(){
    if(state.item?.kind==="group"){const rows=groupModelRows();ui.openPopover($("contextControl"),`<div class="popover-label">Context windows in ${escape(currentName())}</div><div class="model-context-team">${rows.map(({profile,lane})=>`<div><span class="mini-avatar">${ui.avatarSvg(profile)}</span><span><strong>${escape(profile.display_name)}</strong><small>${escape(lane?.model||"Company model")}</small></span><b>${formatContextWindow(lane?.context_window||lane?.max_context_window||0)}</b></div>`).join("")||'<div class="popover-empty">No active coworkers in this group.</div>'}</div><p class="model-context-note">Open a coworker conversation to set a custom ceiling for that model.</p>`,`model-context-popover`,{align:"start"});return;}
    const context=selectedModelContext(),maximum=context.maximum;if(!maximum){ui.openPopover($("contextControl"),'<div class="popover-label">Model context</div><div class="popover-empty">Choose a catalog model before setting its context window.</div>',"model-context-popover",{align:"start"});return;}
    const values=[maximum,.75,.5,.25].map((entry,index)=>index===0?maximum:Math.max(8192,Math.floor(maximum*entry/1024)*1024)).filter((value,index,array)=>array.indexOf(value)===index),rows=values.map((value,index)=>`<button type="button" class="model-context-choice ${context.effective===value?"selected":""}" data-model-context="${index===0?"":value}"><span><strong>${index===0?"Model maximum":`${Math.round(value/maximum*100)}%`}</strong><small>${formatContextWindow(value)} tokens</small></span>${context.effective===value?icons.check:""}</button>`).join("");
    const pop=ui.openPopover($("contextControl"),`<div class="model-context-head"><span><strong>${escape(state.selectedModel?.name||state.selectedModel?.display_name||state.selectedModel?.id||"Current model")}</strong><small>Official maximum · ${formatContextWindow(maximum)}</small></span></div><div class="model-context-choices">${rows}<button type="button" class="model-context-choice ${context.override&&!values.includes(context.effective)?"selected":""}" data-model-context-custom><span><strong>Custom</strong><small>Any lower ceiling from 8K to ${formatContextWindow(maximum)}</small></span></button></div><form class="model-context-custom" hidden><label><span>Tokens</span><input type="number" min="8192" max="${maximum}" step="1024" value="${context.effective||maximum}" inputmode="numeric"></label><button type="submit">Apply</button></form><p class="model-context-note">Phoenix compacts this conversation before it reaches the selected ceiling.</p>`,`model-context-popover`,{align:"start"});
    pop.onclick=(event)=>{const preset=event.target.closest("[data-model-context]");if(preset){saveModelContextWindow(preset.dataset.modelContext?Number(preset.dataset.modelContext):null);return;}if(event.target.closest("[data-model-context-custom]")){const form=pop.querySelector(".model-context-custom");form.hidden=false;form.querySelector("input").focus();form.querySelector("input").select();}};pop.querySelector(".model-context-custom").onsubmit=(event)=>{event.preventDefault();saveModelContextWindow(Number(event.currentTarget.querySelector("input").value));};
  }
  function selectedEffortLevels(model=state.selectedModel){return Array.isArray(model?.effort_levels)?model.effort_levels:[];}
  function normalizedEffort(model,candidate){return selectedEffortLevels(model).includes(candidate)?candidate:null;}
  function workspaceStorageKey(){return "phoenix-workspace:shared";}
  function workspaceName(path){const parts=String(path||"").replace(/[\\/]+$/,"").split(/[\\/]/).filter(Boolean);return parts[parts.length-1]||"";}
  async function loadWorkspace(){
    // Every conversation works in Phoenix's default workspace (~/.phoenix/
    // workspace); there is no folder chooser. Folders saved by older builds
    // are ignored.
    if(preview)state.workspace="/home/user/.phoenix/workspace";
    else{
      try{state.workspace=await ui.invoke("workspace_default");}
      catch{state.workspace="";}
    }
    renderWorkspaceChip();if(state.summaryOpen)refreshEnvironmentSnapshot(true);
  }
  function saveWorkspace(path){
    state.workspace=String(path||"").trim();
    if(state.workspace){localStorage.setItem(workspaceStorageKey(),state.workspace);localStorage.setItem("phoenix-workspace:last",state.workspace);}
    else{localStorage.removeItem(workspaceStorageKey());localStorage.removeItem("phoenix-workspace:last");}
    state.environmentSnapshot=null;renderWorkspaceChip();updateComposerLabels();if(state.summaryOpen)refreshEnvironmentSnapshot(true);
  }
  function renderWorkspaceChip(){
    const label=$("workspaceLabel"),button=$("workspaceButton"),bar=$("workspaceBar");
    if(!label||!button)return;
    const name=workspaceName(state.workspace);
    label.textContent=name||"Choose folder";
    button.title=state.workspace||"Choose the folder this conversation may use";
    button.classList.toggle("empty",!state.workspace);
    button.setAttribute("aria-label",state.workspace?`Workspace ${name}`:"Choose workspace");
    if(bar)bar.hidden=true;
    syncComposerEnd();
  }
  function syncComposerEnd(){
    const zone=$("composerZone"),stage=$("conversationStage");
    if(!zone||!stage)return;
    if(state.composerMeasureFrame)return;
    state.composerMeasureFrame=requestAnimationFrame(()=>{
      state.composerMeasureFrame=0;
      const height=Math.max(120,Math.ceil(zone.getBoundingClientRect().height)+16);
      if(height===state.composerEndHeight)return;
      state.composerEndHeight=height;stage.style.setProperty("--composer-end",`${height}px`);scrollLatest();
    });
  }
  // No folder chooser: coworkers always use Phoenix's default workspace.
  function pickWorkspace(){return loadWorkspace();}
  function updateComposerLabels(){
    $("composerInput").placeholder=`Ask ${currentName()} anything`;
    const access=state.permission==="full_access"?"Full access":state.permission==="talk"?"Talk":"Workspace";
    const location=state.workspace?` · ${workspaceName(state.workspace)}`:"";
    $("composerPermission").textContent=access;
    $("permissionButton").classList.toggle("full-access",state.permission==="full_access");
    $("permissionButton").setAttribute("aria-label",`Access: ${access}${location}. Change access`);
    $("permissionButton").title=`${access}${location}`;
    syncModelContextLabel();syncReasoningSlider();
  }
  function applySettings(snapshot){
    const configured=(snapshot?.settings||[]).find((row)=>row.definition?.key==="composer.default_permission")?.value;
    if(!localStorage.getItem(permissionKey())&&["talk","workspace","full_access"].includes(configured))state.permission=configured;
    const showReason=(snapshot?.settings||[]).find((row)=>row.definition?.key==="composer.show_reasoning")?.value;
    if(typeof showReason==="boolean")document.documentElement.dataset.reasoning=String(showReason);
    const visibleTurns=(snapshot?.settings||[]).find((row)=>row.definition?.key==="conversation.initial_visible_turns")?.value;
    if(Number.isFinite(Number(visibleTurns)))setInitialVisibleTurns(visibleTurns);
    updateComposerLabels();
  }
  function setInitialVisibleTurns(value){state.initialVisibleTurns=Math.max(5,Math.min(50,Number(value)||5));localStorage.setItem("phoenix-conversation-initial-turns",String(state.initialVisibleTurns));}
  // The access picked here is this coworker's access everywhere: saved as
  // its composer.default_permission so scheduled runs, coworker handoffs and
  // chat-app messages run with it too (not only turns typed in this window).
  const syncedPermissions=new Set();
  async function persistAgentPermission(value){
    if(preview||!state.item||!["agent","group"].includes(state.item.kind)||!["talk","workspace","full_access"].includes(value))return;
    const scope={kind:state.item.kind,id:state.item.id},key=`${scope.kind}:${scope.id}:${value}`;if(syncedPermissions.has(key))return;
    try{
      const snap=(await rpc({Settings:{action:"snapshot",scope}},6000)).Settings.snapshot;
      const current=(snap.settings||[]).find((row)=>row.definition?.key==="composer.default_permission")?.value;
      if(current!==value)await rpc({Settings:{action:"set",key:"composer.default_permission",value,scope,expected_revision:snap.revision}},6000);
      syncedPermissions.add(key);
    }catch{}
  }
  function openPermission(){
    // Clicking the access button again closes its menu.
    if(state.permissionPopover?.isConnected||state.permissionWasOpen){state.permissionWasOpen=false;ui.closeLayers();state.permissionPopover=null;return;}
    const folder=state.workspace?workspaceName(state.workspace):"";const values=[["talk","Talk","Conversation only"],["workspace","Workspace",folder?`Company tools inside ${folder}`:"Pick a folder above the composer first"],["full_access","Full access",folder?`Workspace ${folder} is the default cwd; absolute and outside paths are allowed.`:"Choose a workspace for the default cwd; absolute and outside paths are allowed."]];const pop=ui.openPopover($("permissionButton"),`<div class="popover-label">Access for ${escape(currentName())}</div>${values.map(([v,n,d])=>`<button data-value="${v}" class="choice-row ${state.permission===v?"selected":""}"><span><strong>${n}</strong><small>${d}</small></span>${state.permission===v?icons.check:""}</button>`).join("")}`);state.permissionPopover=pop;pop.onclick=(event)=>{const value=event.target.closest("button")?.dataset.value;if(!value)return;state.permission=value;localStorage.setItem(permissionKey(),value);updateComposerLabels();ui.closeLayers();state.permissionPopover=null;persistAgentPermission(value);};}
  function laneSetting(lane = modelLane()) { return (state.modelSnapshot?.lanes || []).find((entry) => entry.lane === lane || (lane === "phoenix" && entry.lane === "orchestrator")) || null; }
  function groupModelRows() {
    if (state.item?.kind !== "group") return [];
    const ids = (ui.state.view?.directory.members || []).filter((member) => member.group_id === state.item.id).sort((a,b) => a.sort_order-b.sort_order).map((member) => member.agent_id);
    return ids.map((id) => { const profile = agentProfile(id), lane = laneSetting(id)||laneSetting("specialist"); return { profile, lane }; }).filter((row) => row.profile);
  }
  async function persistModelLane(model, effort = state.reasoning) {
    const lane = modelLane(); if (!lane || !model) return;
    const providerId = model.provider || model.provider_id, current = laneSetting(lane), accounts = state.modelSnapshot?.accounts || [];
    const matchingAccount = accounts.find((account) => account.provider_id === providerId && !account.cooling_down_until);
    const authProfile = current?.provider_id === providerId ? current.auth_profile_id : matchingAccount?.profile_id || null;
    if (lane !== "phoenix" && current?.provider_id !== providerId && !authProfile) throw new Error(`Connect ${providerId} in Settings → Models before assigning it to ${currentName()}.`);
    effort=normalizedEffort(model,effort);
    await rpc({Settings:{action:"set_model_lane",lane,provider_id:providerId,model:model.id,reasoning_effort:effort,auth_profile_id:authProfile,expected_config_revision:state.modelSnapshot?.config_revision||null}});
    await refreshModels();
  }
  function openReasoning() {
    if (state.item?.kind === "group") {
      const rows = groupModelRows(); ui.openPopover($("reasoningControl"),`<div class="popover-label">Reasoning by coworker</div>${rows.map(({profile,lane})=>`<div class="team-model-row"><span class="mini-avatar">${ui.avatarSvg(profile)}</span><span><strong>${escape(profile.display_name)}</strong><small>${escape(lane?.reasoning_effort||"Company default")}</small></span></div>`).join("")||'<div class="popover-empty">No active coworkers in this group.</div>'}`,"team-model-popover"); return;
    }
    const levels=selectedEffortLevels(),choices=[null,...levels],pop=ui.openPopover($("reasoningControl"),`<div class="popover-label">Reasoning effort</div>${choices.map((v)=>`<button data-value="${v||""}" class="${state.reasoning===v?"selected":""}"><span>${v?v[0].toUpperCase()+v.slice(1):"Provider default"}</span>${state.reasoning===v?icons.check:""}</button>`).join("")}`);
    pop.onclick=async(e)=>{const button=e.target.closest("button");if(!button)return;const v=button.dataset.value||null,previous=state.reasoning;state.reasoning=v;updateComposerLabels();if(v)localStorage.setItem(`phoenix-reasoning:${modelLane()}`,v);else localStorage.removeItem(`phoenix-reasoning:${modelLane()}`);ui.closeLayers();if(preview||!state.selectedModel)return;try{await persistModelLane(state.selectedModel,v);}catch(error){state.reasoning=previous;if(previous)localStorage.setItem(`phoenix-reasoning:${modelLane()}`,previous);else localStorage.removeItem(`phoenix-reasoning:${modelLane()}`);updateComposerLabels();ui.toast(error.message,true);}};
  }
  async function refreshModels(token=null) {
    try {
      const identity=conversationIdentity(),value=await rpc({Settings:{action:"models_snapshot"}},8000,token?.signal);if(token?!selectionIsCurrent(token):conversationIdentity()!==identity)return;
      const payload=value.Settings?.models||value.Settings?.snapshot||value.Settings||{};
      const direct=Array.isArray(payload.catalog)?payload.catalog:Array.isArray(payload.models)?payload.models:null,configuredIds=new Set((payload.accounts||[]).map((account)=>account.provider_id)),providers=Array.isArray(payload.accounts)?(payload.providers||[]).filter((provider)=>configuredIds.has(provider.id)):(payload.providers||[]);
      // Only providers with a connected account show in the model picker.
      state.connectedProviders=Array.isArray(payload.accounts)?new Set(payload.accounts.map((account)=>account.provider_id)):null;
      state.models=(direct||providers.flatMap((provider)=>(provider.models||[]).map((model)=>({...model,provider:provider.id,provider_id:provider.id,provider_name:provider.name})))).map((model)=>({...model,provider:model.provider||model.provider_id,provider_id:model.provider_id||model.provider}));
      state.modelSnapshot=payload;state.selectedLane=laneSetting();
      if(state.item?.kind!=="group"){
        const activity=ui.activityFor(state.item),targetModel=state.selectedLane?.model||activity?.model,targetProvider=state.selectedLane?.provider_id||activity?.provider_id;
        state.selectedModel=state.models.find((model)=>model.id===targetModel&&(!targetProvider||model.provider_id===targetProvider))||state.models.find((model)=>model.id===targetModel)||state.selectedModel||state.models[0]||null;
        state.reasoning=normalizedEffort(state.selectedModel,state.selectedLane?.reasoning_effort||localStorage.getItem(`phoenix-reasoning:${modelLane()}`)||state.reasoning);if(state.reasoning)localStorage.setItem(`phoenix-reasoning:${modelLane()}`,state.reasoning);else localStorage.removeItem(`phoenix-reasoning:${modelLane()}`);
      } else { state.selectedModel=null; state.selectedLane=null; }
      updateComposerLabels();renderModelLabel();
    } catch { if(!token||selectionIsCurrent(token))renderModelLabel(); }
  }
  function renderModelLabel() {
    if(state.item?.kind==="group"){$("composerModel").textContent="Team models";$("composerProviderIcon").innerHTML=ui.providerIcon("server");return;}
    const model=state.selectedModel,activity=ui.activityFor(state.item);$("composerModel").textContent=model?.name||model?.display_name||activity?.model||"Default model";$("composerProviderIcon").innerHTML=ui.providerIcon(model?.provider||model?.provider_id||activity?.provider_id||"openai",model?.id||model?.model||activity?.model||"");syncModelContextLabel();
  }
  function openModels() {
    if(state.item?.kind==="group"){
      const rows=groupModelRows();ui.openPopover($("modelButton"),`<div class="popover-label">Models in ${escape(currentName())}</div>${rows.map(({profile,lane})=>`<div class="team-model-row"><span class="mini-avatar">${ui.avatarSvg(profile)}</span><span><strong>${escape(profile.display_name)}</strong><small>${escape(lane?.model||"Company default")}</small></span>${ui.providerIcon(lane?.provider_id||"server",lane?.model||"")}</div>`).join("")||'<div class="popover-empty">No active coworkers in this group.</div>'}`,"team-model-popover");return;
    }
    // assistant-ui ModelSelector (search + provider filters + groups): only
    // providers with a connected account, grouped under their name, no
    // reasoning section (reasoning has its own control).
    const kit=window.PhoenixAgentKit,connected=state.connectedProviders,models=(state.models||[]).filter((model)=>!connected||connected.has(model.provider||model.provider_id||"custom")),providers=[];
    for(const model of models){const id=model.provider||model.provider_id||"custom";if(!providers.some((provider)=>provider.id===id))providers.push({id,name:model.provider_name||ui.providerLabel(id)});}
    let activeProvider="all";
    const itemMarkup=(model)=>{const index=(state.models||[]).indexOf(model),provider=model.provider||model.provider_id||"custom",name=model.name||model.display_name||model.id,selected=state.selectedModel===model,context=Number(model.context_window)||0;return`<button type="button" role="option" aria-selected="${selected}" class="ms-item${selected?" selected":""}" data-index="${index}" data-model-provider="${escape(provider)}" data-model-search="${escape(`${name} ${model.id||""} ${model.provider_name||provider}`.toLowerCase())}"><span class="ms-icon">${ui.providerIcon(provider,model.id||model.model||name)}</span><span class="ms-copy"><span class="ms-name">${escape(name)}</span><span class="ms-desc">${escape(context?`${formatContextWindow(context)} context`:(model.provider_name||ui.providerLabel(provider)))}</span></span>${selected?`<span class="ms-check">${kit.icon("check")}</span>`:""}</button>`;};
    const groups=providers.map((provider)=>`<div class="ms-group" role="group" data-group-provider="${escape(provider.id)}" aria-label="${escape(provider.name)}"><div class="ms-heading">${escape(provider.name)}</div>${models.filter((model)=>(model.provider||model.provider_id||"custom")===provider.id).map(itemMarkup).join("")}</div>`).join("");
    const filters=providers.length>1?`<div class="ms-filters" role="tablist" aria-label="Provider"><button type="button" role="tab" data-ms-filter="all" aria-selected="true">All</button>${providers.map((provider)=>`<button type="button" role="tab" data-ms-filter="${escape(provider.id)}" aria-selected="false">${ui.providerIcon(provider.id)}<span>${escape(provider.name)}</span></button>`).join("")}</div>`:"";
    const pop=ui.openPopover($("modelButton"),`<div class="ms-search">${icons.search}<input type="text" role="combobox" aria-expanded="true" aria-autocomplete="list" placeholder="Search models..." aria-label="Search models" autocomplete="off" spellcheck="false" ${models.length?"":"disabled"}></div>${filters}<div class="ms-list" role="listbox" aria-label="Models">${models.length?groups:""}<div class="ms-empty" ${models.length?"hidden":""}>${models.length?"No models found.":"No connected providers yet. Open Settings → Models & Providers."}</div></div>`,"model-selector-popover",{align:"start"});
    const search=pop.querySelector(".ms-search input"),items=[...pop.querySelectorAll(".ms-item")],empty=pop.querySelector(".ms-empty"),groupNodes=[...pop.querySelectorAll(".ms-group")];
    let activeIndex=-1;
    const visibleItems=()=>items.filter((item)=>!item.hidden);
    const setActive=(index)=>{const list=visibleItems();items.forEach((item)=>item.classList.remove("active"));if(!list.length){activeIndex=-1;return;}activeIndex=(index+list.length)%list.length;const item=list[activeIndex];item.classList.add("active");item.scrollIntoView({block:"nearest"});search?.setAttribute("aria-activedescendant","");};
    const filterModels=()=>{const words=String(search?.value||"").trim().toLowerCase().split(/\s+/).filter(Boolean);let visible=0;items.forEach((item)=>{const match=(activeProvider==="all"||item.dataset.modelProvider===activeProvider)&&words.every((word)=>item.dataset.modelSearch.includes(word));item.hidden=!match;if(match)visible+=1;});groupNodes.forEach((group)=>{group.hidden=!group.querySelector(".ms-item:not([hidden])");});if(models.length)empty.hidden=visible!==0;const selectedVisible=visibleItems().findIndex((item)=>item.classList.contains("selected"));setActive(selectedVisible>=0&&!words.length?selectedVisible:0);};
    const choose=async(item)=>{const index=item?.dataset.index;if(index==null)return;const previous=state.selectedModel,previousReasoning=state.reasoning,next=(state.models||[])[Number(index)];state.selectedModel=next;state.reasoning=normalizedEffort(next,state.reasoning);renderModelLabel();updateComposerLabels();ui.closeLayers();if(preview)return;try{await persistModelLane(next,state.reasoning);}catch(error){state.selectedModel=previous;state.reasoning=previousReasoning;renderModelLabel();updateComposerLabels();ui.toast(error.message,true);}};
    if(search){search.oninput=filterModels;search.onkeydown=(event)=>{if(event.key==="ArrowDown"){event.preventDefault();setActive(activeIndex+1);}else if(event.key==="ArrowUp"){event.preventDefault();setActive(activeIndex-1);}else if(event.key==="Enter"){event.preventDefault();choose(visibleItems()[activeIndex]);}};requestAnimationFrame(()=>search.focus({preventScroll:true}));}
    filterModels();
    pop.addEventListener("pointermove",(event)=>{const item=event.target.closest(".ms-item");if(item&&!item.classList.contains("active"))setActive(visibleItems().indexOf(item));});
    pop.onclick=(event)=>{const filter=event.target.closest("[data-ms-filter]");if(filter){activeProvider=filter.dataset.msFilter;pop.querySelectorAll("[data-ms-filter]").forEach((button)=>button.setAttribute("aria-selected",String(button===filter)));filterModels();search?.focus({preventScroll:true});return;}const item=event.target.closest(".ms-item");if(item)choose(item);};
  }

  const SLASH_COMMANDS=[
    ["help","Show desktop commands"],["status","Show this task and access state"],["session","Show the canonical task id"],["model","Open the model picker"],["reasoning","Set effort: low, medium, high, xhigh, or max"],["compact","Show context and compaction state"],["memory","Explain live recall and durable saves"],["safe","Use workspace-confined tools"],["workspace","Use workspace-confined tools"],["yolo","Use Full Access"],["stop","Stop the running agent"],["clear","Clear this visible view (history stays durable)"],["new","Explain canonical desktop tasks"],
  ];
  function slashNotice(text){const node=feedNode("story-notice slash-notice",`<div class="markdown">${markdown(text)}</div>`);scrollLatest();return node;}
  function slashNumber(value){const n=Number(value)||0;return n>=1e6?`${(n/1e6).toFixed(1)}M`:n>=1e3?`${Math.round(n/1e3)}K`:String(n);}
  async function handleSlashCommand(text){
    const source=String(text||"").trim();if(!source.startsWith("/"))return false;
    const [raw,...args]=source.slice(1).split(/\s+/),command=raw.toLowerCase();
    if(command==="help"||command==="?"){slashNotice(`Desktop commands: ${SLASH_COMMANDS.map(([name])=>`\`/${name}\``).join(", ")}. Start typing \`/\` to search them.`);return true;}
    if(command==="status"){slashNotice(`${currentName()} is ${state.working?"working":"idle"}. Access: ${state.permission.replaceAll("_"," ")}. ${state.activeTools.size} tool call${state.activeTools.size===1?" is":"s are"} active.`);return true;}
    if(command==="session"){slashNotice(`This is the canonical desktop task \`${state.sessionId||"not loaded"}\` for ${currentName()}.`);return true;}
    if(command==="model"){requestAnimationFrame(openModels);return true;}
    if(command==="reasoning"){
      const level=String(args[0]||"").toLowerCase(),levels=selectedEffortLevels();
      if(!level){requestAnimationFrame(openGooReasoning);return true;}
      if(!levels.includes(level)){ui.toast(`Reasoning for this model: ${levels.join(", ")||"built in"}.`,true);return true;}
      const previous=state.reasoning;state.reasoning=level;localStorage.setItem(`phoenix-reasoning:${modelLane()}`,level);updateComposerLabels();
      try{if(!preview&&state.selectedModel)await persistModelLane(state.selectedModel,level);ui.toast(`Reasoning set to ${level}.`);}catch(error){state.reasoning=previous;updateComposerLabels();ui.toast(error.message||String(error),true);}return true;
    }
    if(command==="compact"){
      const percent=Math.min(100,Math.round(state.usage.used/state.usage.limit*100));
      slashNotice(`Automatic compaction is active. The model currently reports ${slashNumber(state.usage.used)} / ${slashNumber(state.usage.limit)} tokens (${percent}%). Folded rows are archived for exact \`recall\`; the readable desktop transcript intentionally stays visible.`);return true;
    }
    if(command==="memory"){slashNotice("Before-turn memory recall is automatic and local. `memory_save` durably ingests a note first; graph indexing runs separately, so a newly saved note can take a maintenance pass before semantic recall finds it.");return true;}
    if(command==="safe"||command==="workspace"){state.permission="workspace";localStorage.setItem(permissionKey(),state.permission);updateComposerLabels();ui.toast("Workspace access enabled.");return true;}
    if(command==="yolo"){state.permission="full_access";localStorage.setItem(permissionKey(),state.permission);updateComposerLabels();ui.toast("Full Access enabled for this conversation.");return true;}
    if(command==="stop"){if(state.working)await stopTurn();else ui.toast(`${currentName()} is not running.`);return true;}
    if(command==="clear"){if(state.working){ui.toast("Stop the running agent before clearing this view.",true);return true;}clearFeed();renderEmpty();ui.toast("Visible task view cleared. Durable history returns when this conversation reloads.");return true;}
    if(command==="new"){slashNotice("Phoenix desktop keeps one endless canonical task per coworker or group. Choose another coworker/group for a separate task; `/new` remains a terminal-only session command.");return true;}
    ui.toast(`Unknown command /${command}. Type /help.`,true);return true;
  }
  function autosize(){const input=$("composerInput");input.style.height="0";input.style.height=`${Math.min(180,Math.max(36,input.scrollHeight))}px`;detectSlashCommand();detectMention();syncSendMode();}
  function detectSlashCommand(){
    const value=composerText().slice(0,composerCaretOffset()),match=value.match(/^\/([^\s]*)$/),picker=$("slashPicker");
    if(!match){closeSlash();return;}
    closeMention();const query=match[1].toLowerCase(),commands=SLASH_COMMANDS.filter(([name])=>name.startsWith(query));
    picker.hidden=!commands.length;picker.innerHTML=commands.map(([name,description],index)=>`<button type="button" data-slash="${escape(name)}" class="${index===0?"active":""}"><b>/${escape(name)}</b><span><small>${escape(description)}</small></span></button>`).join("");
  }
  function detectMention(){const before=composerText().slice(0,composerCaretOffset()),match=before.match(/@([\w-]*)$/);if(!match||before.startsWith("/")){closeMention();return;}const q=match[1].toLowerCase(),isGroup=state.item?.kind==="group",memberIds=isGroup?new Set((ui.state.view?.directory.members||[]).filter((member)=>member.group_id===state.item.id).map((member)=>member.agent_id)):null,mentioned=new Set(state.mentions),agents=(ui.state.view?.directory.agents||[]).filter((a)=>activatableCoworker(a)&&(!memberIds||memberIds.has(a.agent_id))&&a.agent_id!==state.item?.id&&!mentioned.has(a.agent_id)&&(a.display_name.toLowerCase().includes(q)||a.agent_id.toLowerCase().includes(q)));const everyone=isGroup&&!state.groupEveryone&&"everyone".startsWith(q)?`<button data-id="everyone" class="mention-everyone"><span class="mini-avatar mention-everyone-icon">@</span><span><strong>Everyone</strong><small>Wake all current group members</small></span></button>`:"",picker=$("mentionPicker");picker.hidden=!everyone&&!agents.length;picker.innerHTML=`<div class="mention-picker-head">${isGroup?"Bring someone in":"Mention a coworker"}</div>`+everyone+agents.map((a)=>`<button data-id="${escape(a.agent_id)}"><span class="mini-avatar">${ui.avatarSvg(a)}</span><span><strong>${escape(a.display_name)}</strong><small>${escape(a.role_title)}</small></span></button>`).join("");picker.querySelector("button")?.classList.add("active");}
  function closeMention(){$("mentionPicker").hidden=true;$("mentionPicker").replaceChildren();}
  function closeSlash(){$("slashPicker").hidden=true;$("slashPicker").replaceChildren();}
  function chooseSlash(button){const input=$("composerInput"),name=button.dataset.slash||"help",needsArgument=["reasoning"].includes(name);input.value=`/${name}${needsArgument?" ":""}`;setComposerCaretOffset(input.value.length);closeSlash();input.focus();autosize();}
  function renderMentionTray(){
    const tray=$("mentionTray"),input=$("composerInput");tray.hidden=true;tray.replaceChildren();
    if(state.item?.kind==="group"){
      if(state.groupEveryone&&!input.querySelector('[data-everyone="true"]')){const text=composerText("tokens");renderComposerText(`${CHIP_OPEN}everyone${CHIP_CLOSE}${text?` ${text}`:""}`,{allowEnd:true});}
      else if(!state.groupEveryone){const allowed=new Set(composerGroupProfiles().map((profile)=>profile.agent_id)),existing=new Set([...input.querySelectorAll("[data-composer-agent]")].map((node)=>node.dataset.composerAgent)),missing=state.mentions.map(knownAgentProfile).filter(Boolean).filter((profile)=>allowed.has(profile.agent_id)&&!existing.has(profile.agent_id));if(missing.length){const fragment=document.createDocumentFragment();missing.forEach((profile,index)=>{if(index)fragment.append(document.createTextNode(" "));fragment.append(composerAgentToken(profile));});if(composerText())fragment.append(document.createTextNode(" "));while(input.firstChild)fragment.append(input.firstChild);input.append(fragment);}}
    }
    state.mentions=[...input.querySelectorAll("[data-composer-agent]")].map((node)=>node.dataset.composerAgent).filter((id,index,rows)=>id&&rows.indexOf(id)===index);state.groupEveryone=Boolean(input.querySelector('[data-everyone="true"]'));
    persistComposerDraft();syncSendMode();
  }
  // Replace the typed "@query" with a real chip; the chip, not the name, is
  // what carries the ping to the gateway.
  function chooseMention(button){const input=$("composerInput"),[before,after]=composerSplitAtCaret(),at=before.lastIndexOf("@"),id=button.dataset.id;if(at<0)return;if(id!=="everyone"&&!knownAgentProfile(id))return;const head=`${before.slice(0,at)}${CHIP_OPEN}${id}${CHIP_CLOSE}`,tail=/^\s/.test(after)?after:` ${after}`;renderComposerText(`${head}${tail}`,{allowEnd:true});setComposerCaretOffset(composerTokensDisplayLength(head)+1);closeMention();input.focus();autosize();persistComposerDraft();syncSendMode();}
  function handlePickerKeys(event,picker,onChoose){const buttons=[...picker.querySelectorAll("button")];if(picker.hidden||!buttons.length)return false;if(["ArrowDown","ArrowUp","Tab"].includes(event.key)){event.preventDefault();const current=Math.max(0,buttons.findIndex((button)=>button.classList.contains("active"))),step=event.key==="ArrowUp"?-1:1,next=(current+step+buttons.length)%buttons.length;buttons.forEach((button,index)=>button.classList.toggle("active",index===next));buttons[next].scrollIntoView({block:"nearest"});return true;}if(event.key==="Enter"&&!event.shiftKey){event.preventDefault();onChoose(buttons.find((button)=>button.classList.contains("active"))||buttons[0]);return true;}return false;}
  function removeMention(button){if(button.dataset.removeMention==="everyone")state.groupEveryone=false;else state.mentions=state.mentions.filter((id)=>id!==button.dataset.removeMention);renderMentionTray();$("composerInput").focus();}
  const COMPOSER_IMAGE_TYPES=new Set(["image/png","image/jpeg","image/gif","image/webp","image/avif"]),COMPOSER_IMAGE_EXTENSIONS=new Map([["png","image/png"],["jpg","image/jpeg"],["jpeg","image/jpeg"],["gif","image/gif"],["webp","image/webp"],["avif","image/avif"]]);
  function composerImageType(file){let type=String(file?.type||"").toLowerCase();if(type==="image/jpg")type="image/jpeg";if(COMPOSER_IMAGE_TYPES.has(type))return type;const extension=String(file?.name||"").toLowerCase().split(".").at(-1);return COMPOSER_IMAGE_EXTENSIONS.get(extension)||"";}
  function isComposerImage(file){return Boolean(composerImageType(file));}
  function clipboardImageFile(file,index){const type=composerImageType(file);if(!type)return null;const extension=type==="image/jpeg"?"jpg":type.slice("image/".length),name=String(file.name||"").trim()||`Pasted image ${index+1}.${extension}`;return file.name&&file.type===type?file:new File([file],name,{type,lastModified:file.lastModified||Date.now()});}
  function composerIngressTarget(){return{identity:conversationIdentity(),generation:state.composerGeneration};}
  function composerIngressCurrent(target){return Boolean(target?.identity)&&target.identity===conversationIdentity()&&target.generation===state.composerGeneration;}
  function invalidateComposerIngress(){state.composerGeneration++;void cancelVoiceInput();}
  function canceledComposerIngress(){ui.toast("The conversation or draft changed. Select that input again in the intended conversation.");}
  async function addAttachments(files){
    const target=composerIngressTarget();
    for(const file of files){
      if(!composerIngressCurrent(target)){canceledComposerIngress();return;}
      const type=composerImageType(file);
      if(!type){ui.toast(`${file.name||"That clipboard image"} is not a supported image. Use PNG, JPEG, WebP, GIF, or AVIF.`,true);continue;}
      try{
        const raw=await new Promise((resolve,reject)=>{const reader=new FileReader();reader.onload=()=>resolve(reader.result);reader.onerror=reject;reader.readAsDataURL(file);});
        if(!composerIngressCurrent(target)){canceledComposerIngress();return;}
        const src=String(raw).replace(/^data:[^;,]+;/i,`data:${type};`),path=preview?`/tmp/${file.name||"pasted-image"}`:await ui.invoke("save_attachment",{dataUrl:src});
        if(!composerIngressCurrent(target)){canceledComposerIngress();return;}
        state.attachments.push({name:file.name||"Pasted image",path,size:file.size,type,preview:src});renderAttachments();
      }catch(error){if(composerIngressCurrent(target))ui.toast(`Could not attach ${file.name||"that image"}: ${error}`,true);}
    }
  }
  async function pasteComposerImages(event){const clipboard=event.clipboardData;if(!clipboard)return;const itemFiles=[...(clipboard.items||[])].filter((item)=>item.kind==="file"&&String(item.type||"").startsWith("image/")).map((item)=>item.getAsFile()).filter(Boolean),files=(itemFiles.length?itemFiles:[...(clipboard.files||[])].filter((file)=>String(file.type||"").startsWith("image/"))).map(clipboardImageFile).filter(Boolean);if(!files.length)return;event.preventDefault();await addAttachments(files);}
  function draggedComposerImages(transfer){return[...(transfer?.files||[])].filter((file)=>String(file.type||"").startsWith("image/")||/\.(?:png|jpe?g|webp|gif|avif)$/i.test(file.name||""));}
  function setComposerDropTarget(active){$("composer")?.classList.toggle("drop-target",Boolean(active));}
  async function dropComposerImages(event){const files=draggedComposerImages(event.dataTransfer);setComposerDropTarget(false);if(!files.length)return;event.preventDefault();event.stopPropagation();const target=composerIngressTarget();await addAttachments(files);if(composerIngressCurrent(target))$("composerInput").focus();}

  function browserStoryOwner(event){return canonicalAgentId(event?.agent||targetAgent()||state.item?.id||"phoenix")||"phoenix";}
  // Settings covers the conversation, so an agent's browser popping open
  // there shows beside the wrong page. Hold the reveal until Settings closes
  // and show it in that agent's conversation instead.
  let deferredBrowserReveal=null;
  window.addEventListener("phoenix:settings-visibility",(event)=>{if(event.detail?.open||!deferredBrowserReveal)return;const pending=deferredBrowserReveal;deferredBrowserReveal=null;if(conversationIdentity()===pending.identity)autoRevealAgentBrowser(pending.event);});
  function autoRevealAgentBrowser(event){
    const identity=conversationIdentity(),owner=browserStoryOwner(event);
    // In one coworker's chat, only that coworker's own browser opens. A
    // delegate's browsing (Theo working for Tibo) stays in the delegate's chat.
    if(state.item?.kind==="agent"){
      const self=agentProfile(state.item.id==="orchestrator"?"phoenix":state.item.id)?.agent_id||state.item.id;
      const who=agentProfile(owner==="orchestrator"?"phoenix":owner)?.agent_id||owner;
      if(who&&self&&who!==self)return;
    }
    if(document.body.classList.contains("settings-open")){deferredBrowserReveal={identity,event};return;}
    queueMicrotask(async()=>{if(conversationIdentity()!==identity)return;try{await openBrowser(owner,"browse");}catch(error){if(conversationIdentity()===identity)ui.toast(`Could not show ${agentLabel(owner)}'s browser: ${error.message||error}`,true);}});
  }
  function setAttachmentMenuOpen(open){const expanded=Boolean(open);if(globalThis.phoenixLiquidAttachmentMenu?.setOpen)globalThis.phoenixLiquidAttachmentMenu.setOpen(expanded);else $("attachmentMenu").classList.toggle("open",expanded);return expanded;}
  function closeAttachmentMenu(){const wasOpen=$("attachmentMenu").classList.contains("open");setAttachmentMenuOpen(false);return wasOpen;}
  function toggleAttachmentMenu(){setAttachmentMenuOpen(!$("attachmentMenu").classList.contains("open"));}
  function attachmentPathIcon(file){return file.type==="inode/directory"?'<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M2.8 5.7A1.7 1.7 0 0 1 4.5 4h4l1.7 2h5.3a1.7 1.7 0 0 1 1.7 1.7v6.8a1.7 1.7 0 0 1-1.7 1.7h-11a1.7 1.7 0 0 1-1.7-1.7V5.7Z"/></svg>':'<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M5 2.8h6l4 4V17H5Z"/><path d="M11 2.8V7h4"/></svg>';}
  function addPickedAttachment(file){if(!file?.path)return;if(state.attachments.some((attachment)=>attachment.path===file.path)){ui.toast(`${file.name||"That item"} is already attached.`);return;}state.attachments.push({name:file.name||file.path.split(/[\\/]/).filter(Boolean).at(-1)||"Attachment",path:file.path,size:Number(file.size)||0,type:file.type||"application/octet-stream",preview:""});renderAttachments();}
  async function pickImages(){
    const target=composerIngressTarget();
    closeAttachmentMenu();
    if(preview){$("attachmentInput").click();return;}
    try{
      const files=await ui.invoke("attachment_pick_images");
      if(!composerIngressCurrent(target)){canceledComposerIngress();return;}
      for(const file of files||[]){
        if(!file?.path||state.attachments.some((attachment)=>attachment.path===file.path))continue;
        let source="";try{source=await ui.invoke("image_data_url",{path:file.path});}catch{}
        if(!composerIngressCurrent(target)){canceledComposerIngress();return;}
        state.attachments.push({name:file.name||"Image",path:file.path,size:Number(file.size)||0,type:file.type||composerImageType(file)||"image/*",preview:source});
        renderAttachments();
      }
      renderAttachments();
    }catch(error){if(composerIngressCurrent(target))ui.toast(`Could not attach images: ${error}`,true);}
  }
  async function pickPathAttachment(kind){const target=composerIngressTarget();ui.closeModal();try{const file=preview?{name:kind==="folder"?"Project folder":"brief.pdf",path:kind==="folder"?"/tmp/project-folder":"/tmp/brief.pdf",size:kind==="folder"?0:48120,type:kind==="folder"?"inode/directory":"application/pdf"}:await ui.invoke(kind==="folder"?"attachment_pick_folder":"attachment_pick_file");if(!composerIngressCurrent(target)){canceledComposerIngress();return;}if(file)addPickedAttachment(file);}catch(error){if(composerIngressCurrent(target))ui.toast(`Could not attach that ${kind}: ${error}`,true);}}
  function openPathAttachmentChoice(){closeAttachmentMenu();ui.showModal('<section class="modal attachment-kind-modal" role="dialog" aria-modal="true" aria-labelledby="attachmentKindTitle"><header class="modal-header"><span><strong id="attachmentKindTitle">Attach a file or folder</strong><small>Choose one item to give the current agent its exact path.</small></span><button class="modal-close" type="button" aria-label="Cancel">×</button></header><div class="modal-body attachment-kind-grid"><button type="button" data-attachment-kind="file"><span class="attachment-kind-icon"><svg viewBox="0 0 20 20"><path d="M5 2.8h6l4 4V17H5Z"/><path d="M11 2.8V7h4"/></svg></span><span><strong>File</strong><small>Document, code, archive, or any other file</small></span></button><button type="button" data-attachment-kind="folder"><span class="attachment-kind-icon"><svg viewBox="0 0 20 20"><path d="M2.8 5.7A1.7 1.7 0 0 1 4.5 4h4l1.7 2h5.3a1.7 1.7 0 0 1 1.7 1.7v6.8a1.7 1.7 0 0 1-1.7 1.7h-11a1.7 1.7 0 0 1-1.7-1.7V5.7Z"/></svg></span><span><strong>Folder</strong><small>A directory the agent can inspect or work in</small></span></button></div></section>');$("modalLayer").querySelectorAll("[data-attachment-kind]").forEach((button)=>button.onclick=()=>pickPathAttachment(button.dataset.attachmentKind));}
  function composerCommentGroups(){return state.attachments.map((file,index)=>({file,index,comments:normalizedImageComments(file.comments)})).filter((group)=>group.comments.length);}
  function composerCommentPopoverMarkup(groups){return groups.map(({file,comments})=>`<section><strong>${escape(file.name||"Image")}</strong><ol>${comments.map((comment)=>`<li><span>${Math.round(comment.x)}%, ${Math.round(comment.y)}%</span><p>${escape(comment.text)}</p></li>`).join("")}</ol></section>`).join("");}
  function positionComposerCommentPopover(){const popover=$("composerCommentPopover"),trigger=$("attachmentTray")?.querySelector("[data-composer-comments]"),composer=$("composer");if(!popover||!trigger||!composer||popover.hidden)return;const host=composer.getBoundingClientRect(),chip=trigger.getBoundingClientRect(),card=popover.getBoundingClientRect(),left=Math.max(6,Math.min(host.width-card.width-6,chip.right-host.left-card.width));popover.style.left=`${left}px`;popover.style.bottom=`${Math.max(48,host.bottom-chip.top+4)}px`;}
  function showComposerCommentPopover(){clearTimeout(state.composerCommentPopoverTimer);const popover=$("composerCommentPopover");if(!popover?.innerHTML)return;popover.hidden=false;positionComposerCommentPopover();}
  function hideComposerCommentPopover(immediate=false){clearTimeout(state.composerCommentPopoverTimer);const hide=()=>{const popover=$("composerCommentPopover");if(popover)popover.hidden=true;};if(immediate)hide();else state.composerCommentPopoverTimer=setTimeout(hide,260);}
  function hydrateComposerAttachmentImages(){$("attachmentTray")?.querySelectorAll("[data-inspect-attachment] img:not([src])").forEach(async(image)=>{const button=image.closest("[data-inspect-attachment]"),file=state.attachments[Number(button?.dataset.inspectAttachment)];if(!file?.path)return;try{const source=preview?ui.phoenixLogoSource():await ui.invoke("image_data_url",{path:file.path});if(!image.isConnected)return;file.preview=source;image.src=source;persistComposerDraft();}catch{image.closest(".attachment-card")?.classList.add("failed");}});}
  function renderAttachments(){
    const tray=$("attachmentTray"),popover=$("composerCommentPopover"),groups=composerCommentGroups(),commentCount=groups.reduce((sum,group)=>sum+group.comments.length,0),populated=Boolean(state.attachments.length);
    tray.innerHTML=state.attachments.map((file,index)=>isComposerImage(file)?`<span class="attachment-card attachment-image" title="${escape(file.name)}"><button type="button" class="attachment-media" data-inspect-attachment="${index}" aria-label="Open ${escape(file.name)} in activity sidebar"><img${file.preview?` src="${escape(file.preview)}"`:""} alt="${escape(file.name)}"></button><button type="button" class="attachment-remove" data-remove="${index}" aria-label="Remove ${escape(file.name)}">×</button></span>`:`<span class="attachment-card attachment-file" title="${escape(file.name)}"><span class="attachment-media">${attachmentPathIcon(file)}<strong>${escape(file.name)}</strong></span><button type="button" class="attachment-remove" data-remove="${index}" aria-label="Remove ${escape(file.name)}">×</button></span>`).join("")+(commentCount?`<button type="button" class="composer-comment-summary" data-composer-comments aria-label="Show ${commentCount} image comments"><span aria-hidden="true">⊕</span>${commentCount} comment${commentCount===1?"":"s"}</button>`:"");
    popover.innerHTML=composerCommentPopoverMarkup(groups);if(!commentCount)hideComposerCommentPopover(true);const trigger=tray.querySelector("[data-composer-comments]");if(trigger){trigger.onpointerenter=showComposerCommentPopover;trigger.onpointerleave=()=>hideComposerCommentPopover();trigger.onfocus=showComposerCommentPopover;trigger.onblur=()=>hideComposerCommentPopover();}
    tray.classList.toggle("populated",populated);tray.setAttribute("aria-hidden",String(!populated));$("attachmentMenu").classList.toggle("has-files",populated);persistComposerDraft();syncSendMode();syncComposerEnd();hydrateComposerAttachmentImages();renderImageCommentState();
  }
  function renderVoiceState(){
    const button=$("voiceButton"),phase=state.voiceCapture?.phase;
    const label=state.voiceBusy?(phase==="transcribing"?"Transcribing voice input":phase==="stopping"?"Stopping voice input":"Starting voice input"):phase==="stop_failed"?"Retry stopping voice input":state.voiceActive?"Stop voice input":"Voice input";
    button.disabled=state.voiceBusy;button.classList.toggle("voice-button-recording",state.voiceActive);
    button.setAttribute("aria-pressed",String(state.voiceActive));button.setAttribute("aria-busy",String(state.voiceBusy));button.setAttribute("aria-label",label);button.title=label;
  }
  function forgetVoiceCapture(id){
    try{if(sessionStorage.getItem("phoenix-voice-capture:v1")===id)sessionStorage.removeItem("phoenix-voice-capture:v1");}catch{}
  }
  async function recoverVoiceCapture(){
    let id;try{id=sessionStorage.getItem("phoenix-voice-capture:v1");}catch{return;}
    if(!id||state.voiceCapture)return;
    if(!/^[a-zA-Z0-9_-]{1,128}$/.test(id)){forgetVoiceCapture(id);return;}
    // Only an opaque capture identity survives reload. Recovery stops that
    // recorder; it never uploads or restores old voice text into a new draft.
    state.voiceCapture={id,target:composerIngressTarget(),phase:"stopping",canceled:true};
    await cancelVoiceInput();
  }
  async function releaseVoiceCapture(capture){
    const receipt=preview?true:await ui.invoke("voice_capture_cancel",{captureId:capture.id});
    if(typeof receipt!=="boolean")throw Error("Voice cancellation was not confirmed.");
    forgetVoiceCapture(capture.id);
    if(state.voiceCapture===capture)state.voiceCapture=null;
  }
  async function cancelVoiceInput(){
    const capture=state.voiceCapture;if(!capture)return;
    capture.canceled=true;state.voiceActive=false;
    // A pending start cancels its own exact capture after its acknowledgement.
    // A pending transcription is ignored, never pasted into the new draft.
    if(state.voiceBusy){renderVoiceState();return;}
    state.voiceBusy=true;capture.phase="stopping";renderVoiceState();
    try{await releaseVoiceCapture(capture);}
    catch(error){capture.phase="stop_failed";ui.toast(`Could not stop voice input: ${error}`,true);}
    finally{state.voiceBusy=false;renderVoiceState();}
  }
  async function toggleVoice(){
    if(state.voiceBusy)return;
    if(!state.voiceCapture){
      const capture={id:crypto.randomUUID(),target:composerIngressTarget(),phase:"starting",canceled:false};
      if(!capture.target.identity)return;
      try{sessionStorage.setItem("phoenix-voice-capture:v1",capture.id);}
      catch{ui.toast("Voice input could not save its recovery identity. Reload Phoenix before recording.",true);return;}
      state.voiceCapture=capture;state.voiceBusy=true;renderVoiceState();
      try{
        const prepared=preview?capture.id:await ui.invoke("voice_capture_prepare_owned",{captureId:capture.id});
        if(prepared!==capture.id)throw Error("Voice capture preparation was not confirmed.");
        if(!capture.canceled&&composerIngressCurrent(capture.target)){
          const receipt=preview?capture.id:await ui.invoke("voice_capture_start_owned",{captureId:capture.id});
          if(receipt!==capture.id)throw Error("Voice capture did not confirm its request.");
          if(!capture.canceled&&composerIngressCurrent(capture.target)){capture.phase="recording";state.voiceActive=true;}
        }
      }catch(error){if(composerIngressCurrent(capture.target))ui.toast(String(error),true);}
      finally{
        if(!state.voiceActive){
          capture.canceled=true;capture.phase="stopping";
          try{await releaseVoiceCapture(capture);}catch(error){capture.phase="stop_failed";ui.toast(`Could not stop voice input: ${error}`,true);}
        }
        state.voiceBusy=false;renderVoiceState();
      }
      return;
    }
    if(state.voiceCapture.canceled){await cancelVoiceInput();return;}
    const capture=state.voiceCapture;state.voiceActive=false;state.voiceBusy=true;capture.phase="transcribing";renderVoiceState();
    let stopped=false;
    try{
      const text=preview?"Draft a concise update from this voice note.":await ui.invoke("voice_capture_stop_owned",{captureId:capture.id});
      stopped=true;
      if(!capture.canceled&&composerIngressCurrent(capture.target)){
        if(typeof text!=="string"||!text.trim())throw Error("Voice input returned no transcript.");
        $("composerInput").value+=`${$("composerInput").value?" ":""}${text}`;autosize();persistComposerDraft();
      }else if(!capture.canceled)canceledComposerIngress();
    }catch(error){if(!capture.canceled&&composerIngressCurrent(capture.target))ui.toast(String(error),true);}
    finally{
      if(stopped){forgetVoiceCapture(capture.id);if(state.voiceCapture===capture)state.voiceCapture=null;}
      else{
        capture.canceled=true;capture.phase="stopping";
        try{await releaseVoiceCapture(capture);}catch(error){capture.phase="stop_failed";ui.toast(`Could not stop voice input: ${error}`,true);}
      }
      state.voiceBusy=false;renderVoiceState();
    }
  }

  function ownerProfile(ownerId=state.browserOwnerAgentId){return agentProfile(ownerId)||currentProfile();}
  async function browserCommand(browserAction){
    const context={instance:state.browserOwnerId,boundKey:state.browserBoundKey,generation:state.browserSurfaceGeneration};
    if(!context.instance||context.boundKey!==conversationKeyOf(state.item))throw new Error("This browser belongs to another conversation. Open this conversation's browser first.");
    const run=async()=>{
      if(context.instance!==state.browserOwnerId||context.boundKey!==state.browserBoundKey||context.generation!==state.browserSurfaceGeneration||context.boundKey!==conversationKeyOf(state.item))throw new Error("Browser action was superseded by a conversation switch.");
      const prepared={...browserAction};
      if(prepared.action==="type"){
        const target=state.browserTarget||{};
        prepared.target_hint=Object.keys(target).length?{...target,text:""}:null;
      }
      if(state.browserMode==="teach")showTeachingActivity(prepared);
      // Native input is captured once by the isolated-world observer. Sending
      // the same toolbar action through TeachWorkflow would record it here and
      // then record the resulting native navigation a second time.
      const transportOnly=["resize","switch_tab","close_tab"].includes(prepared.action);
      const request=state.browserMode==="teach"&&!state.browserNative&&!transportOnly
        ? {TeachWorkflow:{action:"interact",teaching_id:state.teaching?.teaching?.teaching_id,browser_action:prepared}}
        : {BrowserInteract:{instance:context.instance,browser_action:prepared}};
      const value=await browserControlRpc(request,20000),reply=value.TeachWorkflow||value.BrowserInteraction||{};
      if(context.instance!==state.browserOwnerId||context.boundKey!==state.browserBoundKey||context.generation!==state.browserSurfaceGeneration||context.boundKey!==conversationKeyOf(state.item))throw new Error("Browser action completed after its conversation was closed.");
      if(value.TeachWorkflow)state.teaching=value.TeachWorkflow;
      const receipt=value.BrowserInteraction||value.TeachWorkflow?.teaching?.steps?.at(-1)?.receipt||{};
      if(receipt.target)state.browserTarget=receipt.target;
      else if(prepared.action==="navigate"||prepared.action==="go_back")state.browserTarget=null;
      if(receipt.after_url)applyBrowserLocation(receipt.after_url);
      updateTeachingSteps();return reply;
    };
    const pending=state.browserActionQueue.then(run,run);state.browserActionQueue=pending.catch(()=>{});return pending;
  }
  function targetName(target={}){const value=String(target.label||target.name||target.text||target.role||target.tag||"").trim().replace(/\s+/g," ").slice(0,54);return value||"field";}
  function teachingStepLabel(step){
    if(!step)return"Ready for your next action";
    if(step.action==="navigate")return`Opened ${hostLabel(step.url)}`;
    if(step.action==="go_back")return"Went back";
    if(step.action==="click")return`Clicked ${targetName(step.target)}`;
    if(step.action==="type")return`Filled ${targetName(step.target)}`;
    if(step.action==="send_keys"){const key=String(step.keys||"").toUpperCase(),names={ENTER:"Enter",TAB:"Tab",ESCAPE:"Escape",ARROWUP:"Arrow up",ARROWDOWN:"Arrow down",ARROWLEFT:"Arrow left",ARROWRIGHT:"Arrow right",BACKSPACE:"Backspace",DELETE:"Delete"};return`Pressed ${names[key]||"a key"}`;}
    if(step.action==="select")return`Selected an option in ${targetName(step.target)}`;
    return"Learned an action";
  }
  function showTeachingActivity(action){
    const label=action.action==="type"?`Typing in ${targetName(state.browserTarget||{})}…`:action.action==="click"?"Learning that click…":action.action==="navigate"?"Opening that page…":"Learning that action…";
    $("teachingLatestStep").textContent=label;
  }
  function hostLabel(url){try{return new URL(url).hostname||"New tab";}catch{return url||"New tab";}}
  // A new tab opens empty with the caret in the address bar, like Chrome:
  // click + and just type.
  function focusBrowserAddress(){const input=$("browserAddress");if(!input)return;if(isBlankTabUrl(state.browserFrameUrl))input.value="";requestAnimationFrame(()=>{input.focus({preventScroll:true});input.select();});}
  function syncBrowserAddress(url,force=false){const value=String(url||"").trim()||"about:blank",input=$("browserAddress");state.browserFrameUrl=value;if(input&&(force||(!state.browserAddressEditing&&!state.browserAddressPending)))input.value=isBlankTabUrl(value)?"":value;return value;}
  function setBrowserTabTitle(url,title=""){const node=$("browserTabTitle");if(node)node.textContent=title||hostLabel(url);syncActivitySummary();}
  function applyBrowserLocation(url,title=""){
    const value=syncBrowserAddress(url);
    const active=state.browserTabs.find((tab)=>tab.active)||state.browserTabs[0];if(active){active.url=value;active.title=title||hostLabel(value);renderInspectionBrowserTabs({tabs:state.browserTabs});}else setBrowserTabTitle(value,title);
    rememberInspectionBrowser();
  }
  function formatDownloadBytes(bytes){const value=Number(bytes)||0;if(value<1024)return`${value} B`;if(value<1024*1024)return`${(value/1024).toFixed(value<10240?1:0)} KB`;return`${(value/(1024*1024)).toFixed(value<10*1024*1024?1:0)} MB`;}
  function showBrowserDownload(file){
    state.browserDownloadCurrent=file;const shelf=$("browserDownloadShelf"),complete=Boolean(file.complete);
    $("browserDownloadName").textContent=file.name||"Website download";
    $("browserDownloadStatus").textContent=complete?`Saved · ${formatDownloadBytes(file.bytes)}`:`Downloading · ${formatDownloadBytes(file.bytes)}`;
    $("browserDownloadOpen").disabled=!complete;$("browserDownloadReveal").disabled=!complete;shelf.hidden=false;scheduleNativeBrowserBounds();
  }
  async function pollBrowserDownloads(seed=false){
    if(preview||!state.browserOwnerId||state.browserDownloadPending)return;
    state.browserDownloadPending=true;
    try{
      const files=await ui.invoke("browser_downloads_list",{profileId:state.browserOwnerId});
      if(seed){state.browserDownloadSnapshot=new Map((files||[]).map((file)=>[file.path,file.modified_ms]));return;}
      const fresh=(files||[]).find((file)=>state.browserDownloadSnapshot.get(file.path)!==file.modified_ms);
      if(fresh)showBrowserDownload(fresh);
    }catch(error){if(!$("browserOverlay").hidden)console.warn("Phoenix download monitor:",error);}
    finally{state.browserDownloadPending=false;}
  }
  function startDownloadMonitor(){
    clearInterval(state.browserDownloadTimer);state.browserDownloadSnapshot=new Map();state.browserDownloadCurrent=null;$("browserDownloadShelf").hidden=true;
    if(preview)return;
    pollBrowserDownloads(true);state.browserDownloadTimer=setInterval(()=>pollBrowserDownloads(false),500);
  }
  async function openBrowserDownload(reveal){const file=state.browserDownloadCurrent;if(!file?.complete)return;try{await ui.invoke("browser_download_open",{path:file.path,reveal});}catch(error){ui.toast(`Could not ${reveal?"show":"open"} the download: ${error}`,true);}}
  function subscribeBrowserFrames(){
    state.browserSocket?.close();state.browserControlEnabled=false;state.browserControlWaiters.splice(0).forEach((resolve)=>resolve(false));rejectBrowserControl(new Error("Browser control reconnected."));if(preview)return;
    const instance=state.browserOwnerId,socket=new WebSocket(ui.wsUrl());socket.binaryType="arraybuffer";state.browserSocket=socket;
    socket.onopen=()=>{if(state.browserSocket!==socket||state.browserOwnerId!==instance){socket.close();return;}socket.send(JSON.stringify(window.PhoenixIsolatedBackend?.current?{SubscribeBrowser:{instance}}:"SubscribeBrowser"));};
    socket.onerror=()=>{if(state.browserSocket!==socket||state.browserOwnerId!==instance||state.browserFrame)return;const empty=$("browserEmpty");empty.hidden=false;empty.innerHTML='<div class="browser-native-error"><strong>Could not display this browser</strong><button type="button">Try again</button></div>';empty.querySelector("button").onclick=()=>{if(state.browserSocket===socket&&state.browserOwnerId===instance)subscribeBrowserFrames();};};
    socket.onmessage=(message)=>{if(state.browserSocket!==socket)return;try{
      let frame;
      if(message.data instanceof ArrayBuffer){
        const bytes=new Uint8Array(message.data),view=new DataView(message.data);
        if(bytes.length<8||bytes[0]!==80||bytes[1]!==72||bytes[2]!==88||bytes[3]!==70)return;
        const headerLength=view.getUint32(4),jpegStart=8+headerLength;if(jpegStart>bytes.length)return;
        frame=JSON.parse(new TextDecoder().decode(bytes.subarray(8,jpegStart)));
        frame.blob=new Blob([bytes.subarray(jpegStart)],{type:"image/jpeg"});
      }else{
        const value=JSON.parse(message.data);
        if(value.BrowserSurface){if(value.BrowserSurface.instance===state.browserOwnerId)renderInspectionBrowserTabs(value.BrowserSurface);return;}
        if(value.BrowserStreamReady){state.browserControlEnabled=Boolean(value.BrowserStreamReady.control);state.browserControlWaiters.splice(0).forEach((resolve)=>resolve(state.browserControlEnabled));return;}
        if(value.BrowserInteraction||value.TeachWorkflow||value.Error){const pending=state.browserControlPending;state.browserControlPending=null;if(pending){clearTimeout(pending.timer);value.Error?pending.reject(new Error(value.Error.message)):pending.resolve(value);}return;}
        frame=value.BrowserFrame;
      }
      if(!frame||frame.instance!==state.browserOwnerId)return;state.browserFrameRetry=0;state.browserFramePending=frame;queueBrowserFramePaint();
    }catch{}};
    socket.onclose=()=>{if(state.browserSocket!==socket)return;state.browserControlEnabled=false;state.browserControlWaiters.splice(0).forEach((resolve)=>resolve(false));rejectBrowserControl(new Error("Browser control disconnected."));if(!$("browserOverlay").hidden&&state.browserOwnerId===instance){
      // Back off instead of reconnecting every 600ms forever when the frame
      // lane keeps dropping (a dead or relaunching browser): 0.6s, 1.2s, ... 10s.
      const attempt=state.browserFrameRetry=(state.browserFrameRetry||0)+1,delay=Math.min(10000,600*2**Math.min(attempt-1,5));
      if(attempt>3)console.warn(`Phoenix browser frame lane closed ${attempt} times; reconnecting in ${delay}ms`);
      clearTimeout(state.browserFrameRetryTimer);state.browserFrameRetryTimer=setTimeout(()=>{if(state.browserSocket===socket&&state.browserOwnerId===instance&&!$("browserOverlay").hidden)subscribeBrowserFrames();},delay);
    }};
  }
  function rejectBrowserControl(error){const pending=state.browserControlPending;state.browserControlPending=null;if(pending){clearTimeout(pending.timer);pending.reject(error);}}
  function awaitBrowserControl(timeout=450){if(state.browserControlEnabled&&state.browserSocket?.readyState===WebSocket.OPEN)return Promise.resolve(true);return new Promise((resolve)=>{const done=(value)=>{clearTimeout(timer);resolve(value);};const timer=setTimeout(()=>{const index=state.browserControlWaiters.indexOf(done);if(index>=0)state.browserControlWaiters.splice(index,1);resolve(false);},timeout);state.browserControlWaiters.push(done);});}
  async function browserControlRpc(request,timeout=20000){
    if(state.browserNative)return rpc(request,timeout);
    const direct=await awaitBrowserControl();if(state.browserNative||!direct)return rpc(request,timeout);
    if(state.browserControlPending)return rpc(request,timeout);
    return new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>{if(state.browserControlPending?.timer===timer)state.browserControlPending=null;reject(new Error("Browser control did not answer in time."));},timeout);
      state.browserControlPending={resolve,reject,timer};
      try{state.browserSocket.send(JSON.stringify(request));}catch(error){state.browserControlPending=null;clearTimeout(timer);reject(error);}
    });
  }
  function queueBrowserTransport(browserAction,timeout=5000){
    const instance=state.browserOwnerId,boundKey=state.browserBoundKey,generation=state.browserSurfaceGeneration;
    const run=()=>{if(!instance||instance!==state.browserOwnerId||boundKey!==state.browserBoundKey||generation!==state.browserSurfaceGeneration||boundKey!==conversationKeyOf(state.item))throw new Error("Browser transport was superseded by a conversation switch.");const request={BrowserInteract:{instance,browser_action:browserAction}};return window.PhoenixIsolatedBackend?.current?ui.invoke("backend_rpc",{request}):browserControlRpc(request,timeout);};
    const pending=state.browserActionQueue.then(run,run);state.browserActionQueue=pending.catch(()=>{});return pending;
  }
  function browserSurfaceRect(){
    const rect=$("browserViewport").getBoundingClientRect(),shelf=$("browserDownloadShelf"),shelfSpace=shelf&&!shelf.hidden?76:0;
    let top=Math.max(0,rect.top),bottom=Math.min(innerHeight,rect.bottom);
    const pane=$("inspectionSidebar").getBoundingClientRect();
    // Hidden or transitioning panels measure zero. They must not shrink a
    // valid page to 1px and force the browser into a different rendering lane.
    if(pane.width>0&&pane.height>0)bottom=Math.min(bottom,pane.bottom-11);
    const terminal=$("termPanel").getBoundingClientRect();
    if(document.body.classList.contains("term-open")&&terminal.height>0)bottom=Math.min(bottom,terminal.top-10);
    // Native browser views sit above DOM notifications. Reserve the occupied
    // notification area in the real native bounds rather than relying on CSS.
    for(const toast of document.querySelectorAll("#toastRegion .toast")){
      const r=toast.getBoundingClientRect();if(r.width&&r.height&&r.right>rect.left&&r.left<rect.right&&r.bottom>top&&r.top<rect.bottom)top=Math.min(rect.bottom-1,r.bottom+8);
    }
    const left=Math.max(0,rect.left),right=Math.min(innerWidth,rect.right);
    return{x:Math.round(left),y:Math.round(top),width:Math.max(1,Math.round(right-left)),height:Math.max(1,Math.round(bottom-top-shelfSpace))};
  }
  function queueBrowserSurfaceOperation(operation){
    const pending=state.browserSurfaceOperationChain.then(operation,operation);
    state.browserSurfaceOperationChain=pending.catch(()=>{});
    return pending;
  }
  // The page is a native layer above this UI, so a menu dropping over it
  // would be hidden: hide the page while a browser menu is open.
  function setBrowserMenuOpen(id,open){
    const menu=$(id);if(!menu||menu.hidden===!open)return;
    for(const other of ["browserSettingsMenu","browserExtensionsMenu"])if(other!==id&&open)$(other).hidden=true;
    menu.hidden=!open;
    const anyOpen=!$("browserSettingsMenu").hidden||!$("browserExtensionsMenu").hidden;
    setNativeTeachingSurfaceVisible(!anyOpen).catch(()=>{});
  }
  async function renderBrowserExtensionsMenu(){
    const menu=$("browserExtensionsMenu");menu.innerHTML='<div class="ext-empty">Loading extensions…</div>';
    const result=await Promise.allSettled([ui.invoke("browser_extensions_list",{}),ui.invoke("browser_extension_blocking_status",{})]);
    if(result[0].status!=="fulfilled"){const error=result[0].reason;menu.innerHTML=`<div class="ext-empty">${escape(error.message||String(error))}</div>`;return;}
    const list=result[0].value||[],blocking=result[1].status==="fulfilled"?result[1].value:null;
    const nativeBlocking=blocking?`<div class="ext-row"><span class="ext-icon-fallback" aria-hidden="true"><svg viewBox="0 0 22 22" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M11 2 3.5 5v5c0 4.5 7.5 9.5 7.5 9.5s7.5-5 7.5-9.5V5Z"/><path d="m7.5 10 2.5 2.5 4.5-4.5"/></svg></span><span class="ext-name"><strong>Block ads and trackers</strong><small>Built-in · ${Number(blocking.blocked)||0} requests blocked</small></span><input class="ph-switch" type="checkbox" data-native-blocking aria-label="Block ads and trackers" title="Blocks ad and tracking requests. Reload to refresh already loaded content." ${blocking.enabled?"checked":""}></div>${blocking.error?`<div class="ext-empty">${escape(blocking.error)}</div>`:""}`:"";
    const rows=list.map((ext)=>`<div class="ext-row" data-ext-id="${escape(ext.id)}">${ext.icon?`<img src="${escape(ext.icon)}" alt="">`:'<span class="ext-icon-fallback"></span>'}<span class="ext-name"><strong>${escape(ext.name)}</strong><small>${escape(ext.version)}</small></span><input class="ph-switch" type="checkbox" data-ext-toggle aria-label="${ext.enabled?"Turn off":"Turn on"} ${escape(ext.name)}" ${ext.enabled?"checked":""}>${ext.popup&&ext.enabled?`<button type="button" data-ext-open title="Open ${escape(ext.name)}" aria-label="Open ${escape(ext.name)}"><svg viewBox="0 0 20 20"><path d="m7.8 12.2 6-6M8.4 6.2H14v5.6"/></svg></button>`:""}<button type="button" data-ext-remove title="Remove ${escape(ext.name)}" aria-label="Remove ${escape(ext.name)}"><svg viewBox="0 0 20 20"><path d="m6 6 8 8M14 6l-8 8"/></svg></button></div>`).join("");
    menu.innerHTML=`<div class="ext-head">Extensions</div>${nativeBlocking}${rows||'<div class="ext-empty">No extensions yet.</div>'}<button type="button" class="ext-store" data-ext-store>Get extensions from the Chrome Web Store</button>`;
  }
  async function browserExtensionsMenuClick(event){
    event.stopPropagation();
    const id=event.target.closest("[data-ext-id]")?.dataset.extId;
    try{
      if(event.target.closest("[data-ext-store]")){setBrowserMenuOpen("browserExtensionsMenu",false);await navigateBrowser("https://chromewebstore.google.com/",true);return;}
      if(id&&event.target.closest("[data-ext-open]")){setBrowserMenuOpen("browserExtensionsMenu",false);await ui.invoke("browser_extension_open",{instance:state.browserOwnerId,id});return;}
      if(id&&event.target.closest("[data-ext-remove]")){await ui.invoke("browser_extension_remove",{id});await renderBrowserExtensionsMenu();}
    }catch(error){ui.toast(error.message||String(error),true);}
  }
  async function browserExtensionsMenuChange(event){
    const nativeToggle=event.target.closest("[data-native-blocking]");
    if(nativeToggle){nativeToggle.disabled=true;try{await ui.invoke("browser_extension_blocking_set",{enabled:nativeToggle.checked});}catch(error){nativeToggle.checked=!nativeToggle.checked;ui.toast(error.message||String(error),true);}finally{await renderBrowserExtensionsMenu();}return;}
    const toggle=event.target.closest("[data-ext-toggle]"),id=toggle?.closest("[data-ext-id]")?.dataset.extId;if(!id)return;
    try{await ui.invoke("browser_extension_set_enabled",{id,enabled:toggle.checked});await renderBrowserExtensionsMenu();}catch(error){toggle.checked=!toggle.checked;ui.toast(error.message||String(error),true);}
  }
  function scheduleNativeBrowserBounds(){
    if(!state.browserNative||!state.browserOwnerId||$("browserOverlay").hidden||state.browserSurfaceBoundsFrame)return;
    state.browserSurfaceBoundsFrame=setTimeout(()=>{state.browserSurfaceBoundsFrame=0;pushNativeBrowserBounds();},16);
  }
  // Send the page's real size to the shell. A panel that is hidden or still
  // laying out measures ~0; sending that parked the page at 1×1 (blank) until
  // something else resized it, so tiny sizes are never sent.
  function pushNativeBrowserBounds(){
    if(!state.browserNative||!state.browserOwnerId||$("browserOverlay").hidden)return;
    const rect=browserSurfaceRect();if(rect.width<64||rect.height<64)return;
    state.browserSurfaceBoundsPending=rect;flushNativeBrowserBounds();
  }
  async function flushNativeBrowserBounds(){
    if(state.browserSurfaceBoundsInFlight||!state.browserNative||!state.browserSurfaceBoundsPending)return;
    const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration,rect=state.browserSurfaceBoundsPending;state.browserSurfaceBoundsPending=null;state.browserSurfaceBoundsInFlight=true;
    let retry=false;
    try{
      await queueBrowserSurfaceOperation(async()=>{if(!state.browserNative||state.browserOwnerId!==instance||state.browserSurfaceGeneration!==generation)return;await ui.invoke("browser_surface_set_bounds",{instance,rect});});
      state.browserSurfaceBoundsFailures=0;
    }
    catch(error){
      // A GTK/X11 configure event can briefly race the first child reparent.
      // Tearing down the native surface here silently replaced a real browser
      // with the slow JPEG remote-control path. Keep the attached surface and
      // retry its geometry a few times. A permanent desktop error must not
      // become an unbounded resize loop.
      if(state.browserNative&&state.browserOwnerId===instance&&state.browserSurfaceGeneration===generation){
        state.browserSurfaceBoundsFailures+=1;
        const lostMount=/escaped its Phoenix host|browser surface .* is not attached|embedded Chromium host is missing/i.test(String(error));
        retry=!lostMount&&state.browserSurfaceBoundsFailures<=4;
        console.warn(retry?"Phoenix native browser resize will retry:":"Phoenix native browser mount was lost:",error);
        if(lostMount)fallbackBrowserSurface(String(error?.message||error));
      }
    }
    finally{
      state.browserSurfaceBoundsInFlight=false;
      if(retry)setTimeout(()=>{if(state.browserNative&&state.browserOwnerId===instance&&state.browserSurfaceGeneration===generation){state.browserSurfaceBoundsPending=browserSurfaceRect();flushNativeBrowserBounds();}},120);
      else if(state.browserSurfaceBoundsPending)flushNativeBrowserBounds();
    }
  }
  function teachingVersion(reply){const teaching=reply?.teaching;return teaching?`${teaching.teaching_id||""}:${teaching.updated_at||""}:${teaching.steps?.length||0}`:"";}
  function inspectNativeTeaching(required=false){
    const teachingId=state.teaching?.teaching?.teaching_id;
    if(!teachingId||state.browserMode!=="teach")return Promise.resolve(state.teaching);
    const generation=state.browserSurfaceGeneration,instance=state.browserOwnerId,current=state.browserTeachingInspect;
    if(current?.teachingId===teachingId&&current.generation===generation)return required?current.promise:current.promise.catch(()=>null);
    const job={teachingId,generation,promise:null};
    job.promise=rpc({TeachWorkflow:{action:"inspect",teaching_id:teachingId}},5000).then((value)=>{
      const fresh=value.TeachWorkflow;
      if(state.browserSurfaceGeneration===generation&&state.browserOwnerId===instance&&state.teaching?.teaching?.teaching_id===teachingId&&teachingVersion(fresh)!==teachingVersion(state.teaching)){
        state.teaching=fresh;updateTeachingSteps();
      }
      return fresh;
    }).finally(()=>{if(state.browserTeachingInspect===job)state.browserTeachingInspect=null;});
    state.browserTeachingInspect=job;
    return required?job.promise:job.promise.catch(()=>null);
  }
  function startNativeBrowserStatus(){
    clearInterval(state.browserSurfaceStatusTimer);
    const tick=async()=>{if(state.browserSurfaceStatusPending||(!state.browserNative&&window.__PHOENIX_CHROMIUM_SHELL__?.enabled!==true)||!state.browserOwnerId||state.browserBoundKey!==conversationKeyOf(state.item)||$("browserOverlay").hidden)return;const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration,boundKey=state.browserBoundKey,job={instance,generation,boundKey};state.browserSurfaceStatusPending=job;try{const surface=await inspectBrowserSurface(instance);if(state.browserOwnerId!==instance||state.browserSurfaceGeneration!==generation||state.browserBoundKey!==boundKey||boundKey!==conversationKeyOf(state.item)||(!state.browserNative&&window.__PHOENIX_CHROMIUM_SHELL__?.enabled!==true))return;renderInspectionBrowserTabs(surface);syncActivitySummary();if(state.browserMode==="teach")await inspectNativeTeaching(false);}catch{}finally{if(state.browserSurfaceStatusPending===job)state.browserSurfaceStatusPending=null;}};
    state.browserSurfaceStatusTimer=setInterval(()=>{pushNativeBrowserBounds();tick();},1000);tick();
  }
  async function attachDesktopBrowserSurface(surface,generation,instance){
    const deadline=performance.now()+2200;let lastError;
    while(performance.now()<deadline){
      if(generation!==state.browserSurfaceGeneration||state.browserOwnerId!==instance)throw new Error("native browser attachment was superseded");
      const rect=browserSurfaceRect();
      if(rect.width<64||rect.height<64){lastError=new Error("Browser layout is not ready");await new Promise(resolve=>setTimeout(resolve,90));continue;}
      try{return await ui.invoke("browser_surface_attach",{instance,pid:surface.pid,windowToken:surface.window_token,rect});}catch(error){lastError=error;if(!/visible Chromium window .* was not found|browser surface bounds are outside|browser layout is not ready/i.test(String(error)))throw error;await new Promise((resolve)=>setTimeout(resolve,90));}
    }
    throw lastError||new Error("visible Chromium window did not appear in time");
  }
  async function activateBrowserSurface(generation,instance,attempt=0){
    if(preview){subscribeBrowserFrames();scheduleBrowserResize();return;}
    return queueBrowserSurfaceOperation(async()=>{
      if(generation!==state.browserSurfaceGeneration||state.browserOwnerId!==instance)return;
      try{
        const value=await rpc({BrowserSurface:{instance,action:"open"}},20000),surface=value.BrowserSurface;
        if(generation!==state.browserSurfaceGeneration||state.browserOwnerId!==instance){await rpc({BrowserSurface:{instance,action:"close"}},3000).catch(()=>{});return;}
        const chromiumShell=window.__PHOENIX_CHROMIUM_SHELL__?.enabled===true;
        if(!chromiumShell)renderInspectionBrowserTabs(surface);
        if(!surface?.supported||!surface.attached||(!chromiumShell&&!surface.pid)||!surface.window_token)throw new Error(surface?.reason||"native browser embedding is unavailable");
      state.browserSurfaceInfo=surface;
      const attached=await attachDesktopBrowserSurface(surface,generation,instance);
        if(generation!==state.browserSurfaceGeneration||state.browserOwnerId!==instance){await ui.invoke("browser_surface_hide",{instance}).catch(()=>{});await ui.invoke("browser_surface_detach",{instance}).catch(()=>{});await rpc({BrowserSurface:{instance,action:"close"}},3000).catch(()=>{});return;}
        if(!attached?.supported||attached?.embedded!==true||attached?.visible===false)throw new Error(attached?.reason||"Chromium did not embed inside Phoenix");
        clearTimeout(state.browserSurfaceRecoveryTimer);state.browserSurfaceRecoveryTimer=0;
        renderInspectionBrowserTabs(chromiumShell?attached:surface);
        clearTimeout(state.browserResizeTimer);state.browserResizeKey="";
        state.browserSurfaceInfo={...surface,desktop:attached};state.browserNative=true;delete $("browserViewport").dataset.nativeError;$("browserViewport").classList.add("native-surface");$("browserCanvas").hidden=true;$("browserFrame").hidden=true;$("browserEmpty").hidden=true;startNativeBrowserStatus();scheduleNativeBrowserBounds();
        // Mutter/XWayland may accept the initial reparent and undo it a frame
        // later. Revalidate after the compositor has settled so a browser that
        // escaped its host is hidden and replaced by the in-app frame surface.
        setTimeout(()=>{if(state.browserNative&&state.browserOwnerId===instance&&state.browserSurfaceGeneration===generation){state.browserSurfaceBoundsPending=browserSurfaceRect();flushNativeBrowserBounds();}},350);
      }catch(error){
        // "Busy" means a browser action holds the lane for a moment. Falling
        // back for good left a blank panel with a stale tab strip; wait and
        // attach the real page again instead.
        if(generation===state.browserSurfaceGeneration&&state.browserOwnerId===instance&&/browser surface is busy/i.test(String(error?.message||error))&&attempt<8){
          setTimeout(()=>{if(generation===state.browserSurfaceGeneration&&state.browserOwnerId===instance&&!state.browserNative)activateBrowserSurface(generation,instance,attempt+1);},2500);
          return;
        }
        if(generation===state.browserSurfaceGeneration&&state.browserOwnerId===instance){
          fallbackBrowserSurface(String(error?.message||error),attempt);
        }else await rpc({BrowserSurface:{instance,action:"close"}},3000).catch(()=>{});
      }
    });
  }
  function fallbackBrowserSurface(reason="",attempt=0){
    if(window.__PHOENIX_CHROMIUM_SHELL__?.enabled===true&&state.browserOwnerId){
      // Electron still owns every tab during attachment/reload failures. Keep
      // that browser and its tab inventory; a JPEG fallback reports only the
      // gateway's current target and makes the other tabs disappear.
      const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration;
      state.browserNative=false;clearTimeout(state.browserSurfaceBoundsFrame);state.browserSurfaceBoundsFrame=0;state.browserSurfaceBoundsPending=null;
      $("browserViewport").dataset.nativeError=String(reason).slice(0,500);
      const empty=$("browserEmpty");empty.hidden=false;empty.innerHTML='<div class="browser-native-error"><strong>Reconnecting browser view…</strong><span>Your tabs are still open.</span><button type="button">Try again</button></div>';
      const retry=()=>{if(state.browserOwnerId===instance&&state.browserSurfaceGeneration===generation&&!$("browserOverlay").hidden)activateBrowserSurface(generation,instance,attempt+1);};
      if(attempt>=8)empty.querySelector('strong').textContent='Browser view unavailable';
      empty.querySelector("button").onclick=()=>activateBrowserSurface(generation,instance,0);
      clearTimeout(state.browserSurfaceRecoveryTimer);
      if(attempt<8)state.browserSurfaceRecoveryTimer=setTimeout(retry,Math.min(2000,250*(attempt+1)));
      startNativeBrowserStatus();return;
    }
    if(!state.browserOwnerId||(state.browserNative===false&&state.browserSocket))return;
    const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration;state.browserNative=false;state.browserSurfaceInfo=null;state.browserSurfaceStatusPending=null;state.browserTeachingInspect=null;state.browserSurfaceBoundsFailures=0;$("browserViewport").dataset.nativeError=String(reason||"").slice(0,500);$("browserViewport").classList.remove("native-surface");clearInterval(state.browserSurfaceStatusTimer);state.browserSurfaceStatusTimer=null;clearTimeout(state.browserSurfaceBoundsFrame);state.browserSurfaceBoundsFrame=0;state.browserSurfaceBoundsPending=null;$("browserEmpty").hidden=false;$("browserEmpty").innerHTML='<div class="browser-native-error">'+loaderMarkup("comet",22)+'<strong>Opening the private browser here</strong><span>Phoenix is switching to its in-app browser surface.</span></div>';
    // Always park/detach the foreign X window before enabling the frame lane.
    // Login and teaching remain usable through the same direct CDP controls;
    // they no longer fail into a loose operating-system Chromium window.
    const privateNote=state.browserMode==="login"?" Your sign-in stays on this computer.":state.browserMode==="teach"?" The workflow demonstration stays on this computer.":"";
    $("browserEmpty").querySelector("span").textContent+=privateNote;
    queueBrowserSurfaceOperation(async()=>{await ui.invoke("browser_surface_hide",{instance}).catch(()=>{});await ui.invoke("browser_surface_detach",{instance}).catch(()=>{});await rpc({BrowserSurface:{instance,action:"close"}},3000).catch(()=>{});}).finally(()=>{if(!state.browserNative&&state.browserOwnerId===instance&&state.browserSurfaceGeneration===generation){subscribeBrowserFrames();scheduleBrowserResize();}});
    if(reason)console.warn("Phoenix native browser:",reason);
  }
  function releaseBrowserSurface(instance){
    clearTimeout(state.browserSurfaceRecoveryTimer);state.browserSurfaceRecoveryTimer=0;
    state.browserSurfaceGeneration+=1;state.browserNative=false;state.browserSurfaceInfo=null;state.browserSurfaceStatusPending=null;state.browserTeachingInspect=null;state.browserSurfaceBoundsFailures=0;$("browserViewport").classList.remove("native-surface");clearInterval(state.browserSurfaceStatusTimer);state.browserSurfaceStatusTimer=null;clearTimeout(state.browserSurfaceBoundsFrame);state.browserSurfaceBoundsFrame=0;state.browserSurfaceBoundsPending=null;if(!instance||preview)return Promise.resolve();return queueBrowserSurfaceOperation(async()=>{await ui.invoke("browser_surface_hide",{instance}).catch(()=>{});await ui.invoke("browser_surface_detach",{instance}).catch(()=>{});await rpc({BrowserSurface:{instance,action:"close"}},3000).catch(()=>{});});
  }
  // Keep one JPEG decode in flight and only the newest pending frame. Assigning
  // every incoming data URL made WebKit decode old screenshots for seconds
  // after Chrome had already moved on, which looked like a one-fps browser.
  function queueBrowserFramePaint(){
    if(state.browserFramePainting||state.browserFramePaintQueued)return;
    state.browserFramePaintQueued=true;
    requestAnimationFrame(()=>{state.browserFramePaintQueued=false;paintNewestBrowserFrame();});
  }
  async function paintNewestBrowserFrame(){
    if(state.browserFramePainting||!state.browserFramePending)return;
    const frame=state.browserFramePending;state.browserFramePending=null;state.browserFramePainting=true;
    if(frame.blob&&typeof createImageBitmap==="function"){
      try{
        const bitmap=await createImageBitmap(frame.blob);
        if(state.browserFramePending){bitmap.close?.();return;}
        const canvas=$("browserCanvas");
        if(canvas.width!==bitmap.width||canvas.height!==bitmap.height){canvas.width=bitmap.width;canvas.height=bitmap.height;state.browserCanvasContext=null;}
        if(!state.browserCanvasContext)state.browserCanvasContext=canvas.getContext("bitmaprenderer")||canvas.getContext("2d",{alpha:false,desynchronized:true});
        if(state.browserCanvasContext?.transferFromImageBitmap)state.browserCanvasContext.transferFromImageBitmap(bitmap);
        else{state.browserCanvasContext.drawImage(bitmap,0,0);bitmap.close?.();}
        state.browserFrame=frame;canvas.hidden=false;$("browserFrame").hidden=true;$("browserEmpty").hidden=true;
        if(frame.url&&frame.url!==state.browserFrameUrl){syncBrowserAddress(frame.url);setBrowserTabTitle(frame.url);}
        return;
      }catch(error){console.warn("Phoenix browser bitmap decode:",error);}
      finally{state.browserFramePainting=false;if(state.browserFramePending)queueBrowserFramePaint();}
    }
    paintBrowserImageFrame(frame);
  }
  function paintBrowserImageFrame(frame){
    state.browserFramePainting=true;
    const image=$("browserFrame");
    const settled=()=>{image.onload=null;image.onerror=null;state.browserFramePainting=false;if(state.browserFramePending)queueBrowserFramePaint();};
    const nextObjectUrl=frame.blob?URL.createObjectURL(frame.blob):"";
    image.onload=()=>{
      if(state.browserFrameObjectUrl)URL.revokeObjectURL(state.browserFrameObjectUrl);
      state.browserFrameObjectUrl=nextObjectUrl;
      state.browserFrame=frame;image.hidden=false;$("browserCanvas").hidden=true;$("browserEmpty").hidden=true;
      if(frame.url&&frame.url!==state.browserFrameUrl){syncBrowserAddress(frame.url);setBrowserTabTitle(frame.url);}
      settled();
    };
    image.onerror=()=>{if(nextObjectUrl)URL.revokeObjectURL(nextObjectUrl);settled();};
    image.src=nextObjectUrl||`data:image/jpeg;base64,${frame.data}`;
  }
  // Teaching/login chrome for the embedded browser. Shared by the fresh-open
  // path and the reuse path so switching mode on a live surface dresses it
  // identically without reopening the browser.
  function applyBrowserModeChrome(mode,profile){
    const guided=mode==="teach"||mode==="login";
    $("browserTeachingBadge").hidden=mode!=="teach";$("teachingTopbar").hidden=!guided;syncInspectionOverlayOffset();
    if(!guided)return;
    $("teachingStatusTitle").textContent=mode==="teach"?`Showing ${profile?.display_name||"this coworker"} a workflow`:`Signing in for ${profile?.display_name||"this coworker"}`;
    $("teachingLatestStep").textContent=mode==="teach"?"Ready for your first action":"Use the site's own sign-in. Phoenix does not read or save what you enter.";
    $("teachingStepCount").textContent=mode==="teach"?"0 actions learned":"Passkeys + 2FA supported";
    $("cancelTeaching").textContent=mode==="teach"?"Cancel":"Close";
    $("finishTeaching").textContent=mode==="teach"?"Finish teaching":"I’m logged in";
  }
  // A coworker's browser stays open until someone closes it. After a reload
  // or a conversation switch the panel shows its tabs again (without jumping
  // to them); clicking one reconnects through resumeInspectionBrowser.
  async function openLinkInAgentBrowser(url){
    // Capture the click's owner before any resume/activation can yield. A
    // conversation switch cancels this click; it must not navigate the new owner.
    const token=activeSelectionToken(),key=conversationKeyOf(state.item),clickedOwner=state.item?.kind==="agent"?state.item.id:"phoenix",owner=clickedOwner==="orchestrator"?"phoenix":clickedOwner;
    if(!token||!key)return;
    try{
      const hadConversationBrowser=Boolean(inspectionConversationState(key)?.browser||(state.browserOwnerId&&state.browserBoundKey===key));
      showInspectionSidebar("browser");
      if(state.browserBoundKey!==key)await resumeInspectionBrowser();
      if(!selectionIsCurrent(token))return;
      if(!state.browserOwnerId||state.browserBoundKey!==key)await openBrowser(owner,"browse");
      if(!selectionIsCurrent(token)||state.browserBoundKey!==key||state.browserOwnerAgentId!==owner)return;
      const active=state.browserTabs.find((tab)=>tab.active)||state.browserTabs[0],pristineFirstTab=!hadConversationBrowser&&state.browserTabs.length===1&&isBlankTabUrl(active?.url);
      await navigateBrowser(url,!pristineFirstTab);
    }catch(error){if(selectionIsCurrent(token))ui.toast(error.message||String(error),true);}
  }
  async function peekConversationBrowser(){
    if(preview)return;const key=conversationKeyOf(state.item);if(!key||state.browserBoundKey===key)return;
    const owner=state.item?.kind==="agent"?state.item.id:"phoenix",profile=agentProfile(owner==="orchestrator"?"phoenix":owner),instance=profile?.browser_profile_id||(owner==="phoenix"||owner==="orchestrator"?"agent-phoenix":`agent-${owner}`);
    const status=await ui.invoke("browser_surface_status",{instance}).catch(()=>null);
    if(key!==conversationKeyOf(state.item)||state.browserBoundKey===key)return;
    if(status?.instance&&status.instance!==instance)return;
    if(status?.tabs?.length)renderInspectionBrowserTabs(status);
    // Tabs saved from an earlier run (after a restart): one entry that
    // reopens them when clicked.
    else if(status?.restorable)renderInspectionBrowserTabs({tabs:[{id:"restore",title:"Saved tabs",url:"saved:tabs",active:true}]});
    // With the browser panel open, showing the tab list alone left the page
    // area blank: attach the live page too.
    if(status?.tabs?.length&&state.inspectionOpen&&state.inspectionTab==="browser")resumeInspectionBrowser();
  }
  async function resumeInspectionBrowser(){
    const token=activeSelectionToken(),key=conversationKeyOf(state.item);let saved=inspectionConversationState(key)?.browser;
    // After a reload or restart the UI has forgotten the browser, but the
    // shell still holds (or saved) this agent's tabs: reconnect to them.
    if(!saved&&state.inspectionTab==="browser"&&state.inspectionOpen&&!state.inspectionBrowserOpening&&!preview){
      const owner=state.item?.kind==="agent"?state.item.id:"phoenix",profile=agentProfile(owner==="orchestrator"?"phoenix":owner),instance=profile?.browser_profile_id||(owner==="phoenix"||owner==="orchestrator"?"agent-phoenix":`agent-${owner}`);
      const status=await ui.invoke("browser_surface_status",{instance}).catch(()=>null);
      if(!selectionIsCurrent(token))return;
      if(status?.instance&&status.instance!==instance)return;
      if(status?.tabs?.length||status?.restorable)saved={ownerAgentId:owner,mode:"browse"};
    }
    if(!selectionIsCurrent(token)||!saved||state.inspectionBrowserOpening||state.inspectionTab!=="browser"||!state.inspectionOpen)return;
    if(state.browserBoundKey===key&&state.browserOwnerId){if(state.browserActivationPromise)await state.browserActivationPromise;return;}
    if(state.inspectionBrowserResumePromise)return state.inspectionBrowserResumePromise;
    let settle;const pending=new Promise((resolve)=>{settle=resolve;});state.inspectionBrowserResumePromise=pending;
    try{await openBrowser(state.item?.kind==="agent"?state.item.id:(saved.ownerAgentId||"phoenix"),saved.mode||"browse",null,{restoring:true});}finally{settle();if(state.inspectionBrowserResumePromise===pending)state.inspectionBrowserResumePromise=null;}
  }
  async function openBrowser(ownerId,mode="login",startUrl=null,options={}){
    const token=activeSelectionToken(),requestedBoundKey=conversationKeyOf(state.item);if(!token||!requestedBoundKey)throw new Error("Select a conversation before opening its browser.");
    const requestedOwner=canonicalAgentId(ownerId)||canonicalAgentId(state.item?.id)||"phoenix";
    if(state.item?.kind==="agent"&&requestedOwner!==canonicalAgentId(state.item.id))throw new Error("This browser belongs to another conversation.");
    const requestedProfile=agentProfile(requestedOwner),requestedInstance=requestedProfile?.browser_profile_id||(requestedOwner==="phoenix"?"agent-phoenix":`agent-${requestedOwner}`);
    state.inspectionBrowserOpening=true;try{showInspectionSidebar("browser");}finally{state.inspectionBrowserOpening=false;}
    if(state.browserOwnerId===requestedInstance&&state.browserBoundKey===requestedBoundKey){
      // Same coworker, same browser profile: keep the surface. Only the mode
      // may differ (browse -> teach when "Teach agent" is pressed). Tearing the
      // surface down and reopening it for a mode change is what left a second
      // about:blank tab behind every time teaching started.
      if(state.browserMode!==mode){
        state.browserMode=mode;
        applyBrowserModeChrome(mode,ownerProfile());
      }
      syncBrowserChrome();
      if(state.browserActivationPromise)await state.browserActivationPromise;
      if(selectionIsCurrent(token)&&startUrl)await navigateBrowser(startUrl);
      return;
    }
    if(state.browserOwnerId){const preserve=state.browserBoundKey!==requestedBoundKey;if(preserve)captureInspectionConversation(state.browserBoundKey);await closeBrowser({preserveWorkspace:preserve});}
    if(!selectionIsCurrent(token))return;
    state.browserOwnerAgentId=requestedOwner;const profile=ownerProfile();state.browserOwnerId=profile?.browser_profile_id||(state.browserOwnerAgentId==="phoenix"?"agent-phoenix":`agent-${state.browserOwnerAgentId}`);state.browserMode=mode;state.browserTarget=null;state.browserTyping="";
    state.browserBoundKey=requestedBoundKey;state.browserNative=false;state.browserSurfaceInfo=null;const surfaceGeneration=++state.browserSurfaceGeneration,savedBrowser=inspectionConversationState(requestedBoundKey)?.browser,initialTabs=!startUrl&&savedBrowser?.ownerAgentId===requestedOwner?savedBrowser.tabs:[];renderInspectionBrowserTabs(initialTabs?.length?{tabs:initialTabs}:{url:startUrl||savedBrowser?.url||"about:blank"});
    $("browserOwner").innerHTML=ui.avatarSvg(profile);delete $("browserViewport").dataset.nativeError;syncBrowserChrome();
    applyBrowserModeChrome(mode,profile);
    $("browserCanvas").hidden=true;$("browserFrame").hidden=true;$("browserEmpty").hidden=false;$("browserEmpty").replaceChildren();startDownloadMonitor();const surfaceInstance=state.browserOwnerId;
    // Surface ownership must settle before the first navigation. Starting both
    // RPCs together let BrowserInteract arm the JPEG screencast immediately
    // before BrowserSurface installed its native lease.
    const activation=new Promise((resolve)=>{let settled=false;const settle=()=>{if(settled)return;settled=true;resolve();};requestAnimationFrame(settle);setTimeout(settle,120);}).then(()=>activateBrowserSurface(surfaceGeneration,surfaceInstance));
    state.browserActivationPromise=activation;
    try{await activation;}finally{if(state.browserActivationPromise===activation)state.browserActivationPromise=null;}
    if(surfaceGeneration!==state.browserSurfaceGeneration||state.browserOwnerId!==surfaceInstance)return;
    if(mode==="login"&&!state.browserNative){
      $("teachingStepCount").textContent="Private streamed session";
      $("teachingLatestStep").textContent="Passwords and codes remain private. A device passkey prompt may require Phoenix's native browser surface.";
    }
    if(startUrl){syncBrowserAddress(startUrl,true);await navigateBrowser(startUrl);}else if(!options.restoring)$("browserAddress").focus();
    rememberInspectionBrowser(requestedBoundKey);
    if(preview&&new URLSearchParams(location.search).get("shot")==="teach-download")setTimeout(()=>showBrowserDownload({name:"ChatGPT Image Aug 22, 2026.png",path:"/preview/image.png",bytes:5033165,modified_ms:Date.now(),complete:true}),120);
  }
  function closeBrowser({preserveWorkspace=false}={}){const surfaceInstance=state.browserOwnerId,boundKey=state.browserBoundKey;if(!preserveWorkspace&&boundKey){const saved=inspectionConversationState(boundKey);if(saved)saved.browser=null;}clearTimeout(state.browserTypingTimer);clearTimeout(state.browserResizeTimer);cancelAnimationFrame(state.browserWheelFrame);state.browserWheelFrame=0;state.browserWheelX=0;state.browserWheelY=0;state.browserWheelPoint=null;clearInterval(state.browserDownloadTimer);state.browserDownloadTimer=null;state.browserDownloadPending=false;state.browserDownloadSnapshot.clear();state.browserDownloadCurrent=null;$("browserDownloadShelf").hidden=true;state.browserTyping="";state.browserTypingPending=null;state.browserAddressEditing=false;state.browserAddressPending="";state.browserControlEnabled=false;state.browserControlWaiters.splice(0).forEach((resolve)=>resolve(false));rejectBrowserControl(new Error("Browser closed."));state.browserSocket?.close();state.browserSocket=null;const released=releaseBrowserSurface(surfaceInstance);state.browserActivationPromise=null;state.inspectionBrowserResumePromise=null;$("browserOverlay").hidden=true;$("browserFrame").removeAttribute("src");$("browserCanvas").hidden=true;if(state.browserFrameObjectUrl)URL.revokeObjectURL(state.browserFrameObjectUrl);state.browserFrameObjectUrl="";state.browserMode=null;state.browserFrame=null;state.browserFramePending=null;state.browserFramePainting=false;state.browserFramePaintQueued=false;state.browserFrameUrl="";state.browserResizeKey="";state.browserOwnerAgentId=null;state.browserOwnerId=null;state.browserBoundKey="";state.teaching=null;$("teachingTopbar").hidden=true;renderInspectionBrowserTabs({tabs:[]});if(!preserveWorkspace&&state.inspectionTab==="browser")setInspectionTab("desktop");syncActivitySummary();return released;}
  function updateTeachingSteps(){if(state.browserMode!=="teach")return;const steps=state.teaching?.teaching?.steps||[],count=steps.length;$("teachingStepCount").textContent=`${count} action${count===1?"":"s"} learned`;$("teachingLatestStep").textContent=teachingStepLabel(steps.at(-1));}
  async function focusTeachingOwner(ownerId){if(!ownerId||state.item?.kind==="group"||state.item?.id===ownerId)return;await ui.selectItem?.({kind:"agent",id:ownerId});}
  async function startTeaching(ownerId,startUrl=null,scope=null,groupId=null){
    try{
      await focusTeachingOwner(ownerId);
      // Begin owns the optional start navigation so it is recorded as the
      // first semantic step. Navigating before a teaching id exists would
      // send an invalid Interact request and race the recorder.
      await openBrowser(ownerId,"teach");
      $("teachingLatestStep").textContent="Opening the private browser…";
      const value=await rpc({TeachWorkflow:{action:"begin",owner_agent_id:ownerId,scope,group_id:groupId,start_url:startUrl}},20000);state.teaching=value.TeachWorkflow;updateTeachingSteps();
    }
    catch(error){closeBrowser();if(state.teachingAsk?.card){state.teachingAsk.card.querySelectorAll("button,input").forEach((item)=>item.disabled=false);state.teachingAsk=null;}ui.toast(`Could not start teaching: ${error.message}`,true);}
  }
  async function startTeachingRevision(routineId,ownerId=null){
    try{
      state.revisionOwnerHint=ownerId;
      await focusTeachingOwner(ownerId);
      await openBrowser(ownerId||"phoenix","teach");
      $("teachingLatestStep").textContent="Opening the private browser…";
      const value=await rpc({TeachWorkflow:{action:"revise",routine_id:routineId}},20000),teaching=value.TeachWorkflow?.teaching;state.teaching=value.TeachWorkflow;if(teaching?.owner_agent_id&&teaching.owner_agent_id!==state.browserOwnerAgentId){await focusTeachingOwner(teaching.owner_agent_id);await openBrowser(teaching.owner_agent_id,"teach");}updateTeachingSteps();
    }
    catch(error){closeBrowser();ui.toast(`Could not reteach workflow: ${error.message}`,true);}
  }
  function openTeach(){
    if(state.item?.kind!=="group"){startTeaching(state.item?.id||"phoenix");return;}
    const members=(ui.state.view?.directory.members||[]).filter((m)=>m.group_id===state.item.id).map((m)=>agentProfile(m.agent_id)).filter(Boolean);
    ui.showModal(`<section class="modal" role="dialog" aria-modal="true"><header class="modal-header"><span><strong>Who are you teaching?</strong><small>The saved workflow belongs to the coworker you choose. They can later share it with the group.</small></span><button class="modal-close" aria-label="Close"><svg viewBox="0 0 20 20"><path d="m5 5 10 10M15 5 5 15"/></svg></button></header><div class="modal-body member-teach-list">${members.map((p)=>`<button data-owner="${escape(p.agent_id)}"><span class="mini-avatar">${ui.avatarSvg(p)}</span><span><strong>${escape(p.display_name)}</strong><small>${escape(p.role_title)}</small></span></button>`).join("")}</div></section>`);
    document.querySelector(".member-teach-list").onclick=(event)=>{const owner=event.target.closest("button")?.dataset.owner;if(owner){ui.closeModal();startTeaching(owner);}};
  }
  async function cancelBrowserFlow(){
    if(state.browserMode==="teach"&&state.teaching?.teaching?.teaching_id){
      const teachingId=state.teaching.teaching.teaching_id,cancel=$("cancelTeaching");cancel.disabled=true;
      try{const value=await rpc({TeachWorkflow:{action:"cancel",teaching_id:teachingId}},10000),result=value.TeachWorkflow;if(result?.result!=="teaching_cancelled")throw new Error("Phoenix did not confirm that the teaching draft was deleted");state.teaching=null;}
      catch(error){cancel.disabled=false;ui.toast(`Teaching is still open — cancellation was not confirmed: ${error.message||error}`,true);return;}
      cancel.disabled=false;
    }
    if(state.browserMode==="login"&&state.loginAsk?.card)state.loginAsk.card.querySelectorAll("button").forEach((b)=>b.disabled=false);
    if(state.browserMode==="teach"&&state.teachingAsk?.card){state.teachingAsk.card.querySelectorAll("button,input").forEach((item)=>item.disabled=false);state.teachingAsk=null;}
    closeBrowser();
  }
  function setNativeTeachingSurfaceVisible(visible){
    if(!state.browserNative||!state.browserOwnerId)return Promise.resolve();
    const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration;
    return queueBrowserSurfaceOperation(async()=>{if(!state.browserNative||state.browserOwnerId!==instance||state.browserSurfaceGeneration!==generation)return;await ui.invoke(visible?"browser_surface_show":"browser_surface_hide",{instance});if(visible)scheduleNativeBrowserBounds();});
  }
  async function finishTeaching(){
    try{await inspectNativeTeaching(true);}catch(error){ui.toast(`Could not verify the actions you taught: ${error.message||error}`,true);return;}
    try{await setNativeTeachingSurfaceVisible(false);}catch(error){ui.toast(`Could not pause the teaching browser: ${error.message||error}`,true);return;}
    const teaching=state.teaching?.teaching||{},original=state.teaching?.routine||{},revising=Boolean(teaching.revises_routine_id),teachingId=teaching.teaching_id,count=teaching.steps?.length||0,ownerId=teaching.owner_agent_id||state.browserOwnerAgentId,groups=(ui.state.view?.directory.groups||[]).filter((group)=>(ui.state.view?.directory.members||[]).some((member)=>member.group_id===group.group_id&&member.agent_id===ownerId)),defaultScope=teaching.scope==="company"?"company":teaching.scope==="group"&&teaching.group_id?`group:${teaching.group_id}`:"agent";
    const scopeItems=[["agent",`Only ${agentLabel(ownerId)}`],...groups.map((group)=>[`group:${group.group_id}`,group.name]),["company","Whole company"]];
    const scopeLabel=scopeItems.find((item)=>item[0]===defaultScope)?.[1]||scopeItems[0][1];
    ui.showModal(`<section class="modal" role="dialog" aria-modal="true"><header class="modal-header"><span><strong>${revising?"Update this workflow":"Save this workflow"}</strong><small>${count} semantic step${count===1?"":"s"} captured. ${revising?"The previous definition is retained as a private revision. ":""}Passwords are stored as vault placeholders, never inside the workflow.</small></span><button class="modal-close" aria-label="Close"><svg viewBox="0 0 20 20"><path d="m5 5 10 10M15 5 5 15"/></svg></button></header><form id="workflowForm" class="modal-body"><label class="field"><span>Workflow name</span><input name="name" required autofocus value="${escape(original.name||"")}" placeholder="Publish the weekly product update"></label><label class="field"><span>Available to</span><input type="hidden" name="scope" value="${escape(defaultScope)}"><button type="button" class="menu-select" id="workflowScope"><span>${escape(scopeLabel)}</span><svg viewBox="0 0 20 20"><path d="m6 8 4 4 4-4"/></svg></button></label><label class="field"><span>What should trigger it?</span><input name="triggers" value="${escape((original.trigger_phrases||[]).join(", "))}" placeholder="post the weekly update, publish release notes"></label><label class="field"><span>Notes <small>optional</small></span><textarea name="description" rows="3" placeholder="What a successful run should accomplish">${escape(original.description||"")}</textarea></label></form><footer class="modal-footer"><button class="button secondary" data-cancel type="button">Keep teaching</button><button class="button primary" form="workflowForm" type="submit">${revising?"Update workflow":"Save workflow"}</button></footer></section>`);
    $("workflowScope").onclick=()=>ui.openMenuSelect($("workflowScope"),scopeItems,document.querySelector("#workflowForm [name=scope]").value,(value)=>{document.querySelector("#workflowForm [name=scope]").value=value;$("workflowScope").querySelector("span").textContent=scopeItems.find((item)=>item[0]===value)?.[1]||value;});
    const modalLayer=$("modalLayer");modalLayer.addEventListener("phoenix:modal-closing",(event)=>{if(event.detail?.reason!=="commit")setNativeTeachingSurfaceVisible(true).catch((error)=>ui.toast(`Could not resume the teaching browser: ${error.message||error}`,true));},{once:true});
    modalLayer.querySelector("[data-cancel]").onclick=ui.closeModal;
    $("workflowForm").onsubmit=async(event)=>{event.preventDefault();const data=new FormData(event.currentTarget),submit=$("modalLayer").querySelector("[type=submit]"),scopeValue=String(data.get("scope")||"agent"),[scope,group_id]=scopeValue.startsWith("group:")?["group",scopeValue.slice(6)]:[scopeValue,null],name=String(data.get("name")||"").trim();submit.disabled=true;try{await rpc({TeachWorkflow:{action:"finalize",teaching_id:teachingId,name,description:data.get("description")||"",trigger_phrases:String(data.get("triggers")||"").split(",").map((x)=>x.trim()).filter(Boolean),scope,group_id}},20000);const waiting=state.teachingAsk;state.teachingAsk=null;ui.closeModal("commit");closeBrowser();if(waiting?.card?.isConnected)recordAskAnswer(waiting.card,`Workflow taught and saved: ${name}`);ui.toast(`Workflow saved for ${scope==="company"?"the whole company":scope==="group"?"the group":agentLabel(ownerId)}.`);}catch(error){submit.disabled=false;ui.toast(error.message,true);}};
  }
  async function finishBrowserFlow(){
    await flushBrowserTyping();
    if(state.browserMode==="teach"){await finishTeaching();return;}
    if(state.loginAsk){const {card}=state.loginAsk;state.loginAsk=null;closeBrowser();recordAskAnswer(card,"Embedded login complete");}
    else closeBrowser();
  }
  async function navigateNativeBrowser(action,url="",newTab=false) {
    // Capture the selected surface before pending keystrokes yield. A tab or
    // conversation switch during that wait must not redirect this request.
    const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration;
    const boundKey=state.browserBoundKey,targetId=state.browserTabs.find((tab)=>tab.active)?.id;
    const isCurrent=()=>instance===state.browserOwnerId&&generation===state.browserSurfaceGeneration&&boundKey===state.browserBoundKey&&boundKey===conversationKeyOf(state.item);
    await flushBrowserTyping();
    if(!isCurrent())throw new Error("Navigation was superseded by a conversation switch.");
    if(!newTab&&!targetId)throw new Error("Select an open browser tab before navigating.");
    const surface=await ui.invoke(newTab?"browser_surface_new_tab":"browser_surface_nav",{instance,targetId,action,url});
    if(surface?.supported===false||surface?.reason)throw new Error(surface.reason||"Browser navigation is unavailable.");
    if(!isCurrent())return;
    renderInspectionBrowserTabs(surface);
    applyBrowserLocation(surface?.tabs?.find((tab)=>tab.active)?.url||surface?.url||state.browserFrameUrl||url);
  }
  async function navigateBrowser(url,newTab=false) {
    let value=String(url||"").trim();if(!value)return;
    if(!/^[a-z][a-z0-9+.-]*:(?!\d)/i.test(value))value=/\s/.test(value)||!/^(?:localhost|[^\s/]+\.[^\s/]+|\[[0-9a-f:]+\])(?::\d+)?(?:[/?#]|$)/i.test(value)?`https://www.google.com/search?q=${encodeURIComponent(value)}`:`${/^(?:localhost|127\.|\[)/i.test(value)?"http":"https"}://${value}`;
    if((state.browserNative||window.__PHOENIX_CHROMIUM_SHELL__?.enabled===true)&&state.browserOwnerId){await navigateNativeBrowser("navigate",value,newTab);return;}
    const token=activeSelectionToken(),instance=state.browserOwnerId,generation=state.browserSurfaceGeneration;
    await flushBrowserTyping();
    if(!selectionIsCurrent(token)||instance!==state.browserOwnerId||generation!==state.browserSurfaceGeneration)return;
    const receipt=await browserCommand({action:"navigate",url:value,new_tab:newTab});
    if(selectionIsCurrent(token)&&instance===state.browserOwnerId&&generation===state.browserSurfaceGeneration)applyBrowserLocation(receipt?.after_url||value);
  }
  async function runBrowserToolbarAction(action) {
    try {
      if((state.browserNative||window.__PHOENIX_CHROMIUM_SHELL__?.enabled===true)&&state.browserOwnerId){await navigateNativeBrowser(action);return;}
      await flushBrowserTyping();
      if(action==="back")await browserCommand({action:"go_back"});
      else await browserCommand({action:"send_keys",keys:action==="forward"?"Alt+ArrowRight":"Ctrl+R"});
    }catch(error){ui.toast(error.message||String(error),true);}
  }
  async function submitBrowserAddress(input) {
    const typed=String(input?.value||"").trim();if(!typed)return;
    state.browserAddressEditing=false;state.browserAddressPending=typed;input.blur();
    try{await navigateBrowser(typed);}catch(error){ui.toast(error.message||String(error),true);}
    finally{state.browserAddressPending="";syncBrowserAddress(state.browserFrameUrl||typed,true);}
  }
  function browserSurfacePoint(event){
    const surface=$("browserCanvas").hidden?$("browserFrame"):$("browserCanvas"),rect=surface.getBoundingClientRect(),sourceW=state.browserFrame?.w||surface.naturalWidth||surface.width,sourceH=state.browserFrame?.h||surface.naturalHeight||surface.height;if(!sourceW||!sourceH||!rect.width)return null;
    const scale=Math.min(rect.width/sourceW,rect.height/sourceH),shownW=sourceW*scale,shownH=sourceH*scale,offX=(rect.width-shownW)/2,offY=(rect.height-shownH)/2;
    const x=Math.max(0,Math.min(sourceW,(event.clientX-rect.left-offX)/scale)),y=Math.max(0,Math.min(sourceH,(event.clientY-rect.top-offY)/scale));
    return{x,y,rect};
  }
  async function clickBrowserFrame(event){
    if(state.browserNative)return;
    $("browserViewport").focus({preventScroll:true});const point=browserSurfacePoint(event);if(!point)return;await flushBrowserTyping();const{x,y}=point;
    try{await browserCommand({action:"click",x,y});}catch(error){ui.toast(error.message,true);}
  }
  function wheelBrowserFrame(event){
    if(state.browserNative)return;
    const point=browserSurfacePoint(event);if(!point)return;event.preventDefault();$("browserViewport").focus({preventScroll:true});
    const unit=event.deltaMode===1?28:event.deltaMode===2?point.rect.height:1;
    state.browserWheelX=Math.max(-10000,Math.min(10000,state.browserWheelX+event.deltaX*unit));state.browserWheelY=Math.max(-10000,Math.min(10000,state.browserWheelY+event.deltaY*unit));state.browserWheelPoint=point;
    if(state.browserWheelFrame)return;
    state.browserWheelFrame=requestAnimationFrame(()=>{
      state.browserWheelFrame=0;const current=state.browserWheelPoint,delta_x=state.browserWheelX,delta_y=state.browserWheelY;state.browserWheelPoint=null;state.browserWheelX=0;state.browserWheelY=0;if(!current||(!delta_x&&!delta_y))return;
      queueBrowserTransport({action:"scroll",x:current.x,y:current.y,delta_x,delta_y}).catch((error)=>ui.toast(error.message,true));
    });
  }
  function scheduleBrowserTyping(){clearTimeout(state.browserTypingTimer);state.browserTypingTimer=setTimeout(flushBrowserTyping,24);}
  async function flushBrowserTyping(){
    clearTimeout(state.browserTypingTimer);
    if(state.browserTypingPending){await state.browserTypingPending;if(state.browserTyping)return flushBrowserTyping();return;}
    const text=state.browserTyping;if(!text)return;
    state.browserTyping="";const target=state.browserTarget||{},sensitive=target.input_type==="password"||/password|one-time-code/i.test(target.autocomplete||"");
    const pending=browserCommand({action:"type",text,clear:false,sensitive,parameter_name:sensitive?(target.name||target.label||"password"):null});
    state.browserTypingPending=pending;
    try{await pending;}catch(error){ui.toast(error.message,true);}finally{if(state.browserTypingPending===pending)state.browserTypingPending=null;if(state.browserTyping)scheduleBrowserTyping();}
  }
  async function browserKey(event){
    if(state.browserNative||$("browserOverlay").hidden||event.target===$("browserAddress"))return;
    if(event.key.length===1&&!event.ctrlKey&&!event.metaKey&&!event.altKey){event.preventDefault();state.browserTyping+=event.key;scheduleBrowserTyping();return;}
    const map={Enter:"Enter",Tab:"Tab",Escape:"Escape",ArrowUp:"ArrowUp",ArrowDown:"ArrowDown",ArrowLeft:"ArrowLeft",ArrowRight:"ArrowRight",Backspace:"Backspace",Delete:"Delete"};if(!map[event.key])return;event.preventDefault();await flushBrowserTyping();try{await browserCommand({action:"send_keys",keys:map[event.key]});}catch(error){ui.toast(error.message,true);}
  }

  function scheduleBrowserResize(){
    clearTimeout(state.browserResizeTimer);
    if($("browserOverlay").hidden||!state.browserOwnerId)return;
    if(state.browserNative||window.__PHOENIX_CHROMIUM_SHELL__?.enabled===true){scheduleNativeBrowserBounds();return;}
    const instance=state.browserOwnerId,generation=state.browserSurfaceGeneration;
    state.browserResizeTimer=setTimeout(async()=>{
      if(state.browserOwnerId!==instance||state.browserSurfaceGeneration!==generation||state.browserNative||window.__PHOENIX_CHROMIUM_SHELL__?.enabled===true)return;
      const rect=$("browserViewport").getBoundingClientRect();
      const currentNative=window.PhoenixIsolatedBackend?.current;
      const width=Math.max(currentNative?320:640,Math.min(2560,Math.round(rect.width)));
      const height=Math.max(currentNative?240:480,Math.min(1600,Math.round(rect.height)));
      const key=`${width}x${height}`;if(key===state.browserResizeKey)return;
      const pending=queueBrowserTransport({action:"resize",width,height},5000);
      try{await pending;if(state.browserOwnerId===instance&&state.browserSurfaceGeneration===generation)state.browserResizeKey=key;}
      catch(error){console.warn("Phoenix browser resize:",error);}
    },80);
  }

  function bindNativeBrowserTabEvents(){
    const listen=window.__TAURI__?.event?.listen;if(!listen||state.nativeBrowserTabEventsBound)return;state.nativeBrowserTabEventsBound=true;
    const matches=(payload)=>Boolean(state.browserOwnerId&&state.browserBoundKey===conversationKeyOf(state.item)&&String(payload?.instance||"")===String(state.browserOwnerId));
    ["browser-tab-created","browser-tab-activated","browser-tab-closed"].forEach((name)=>listen(name,(event)=>{const payload=event.payload||{};if(!matches(payload))return;if(Array.isArray(payload.tabs))renderInspectionBrowserTabs({tabs:payload.tabs});else refreshInspectionBrowserTabs().catch(()=>{});}).catch(()=>{}));
    // Chromium resolves a page's icon after its title, so the favicon event is
    // usually the last one to land — without it the tab keeps the placeholder
    // until some other refresh happens to fire.
    ["browser-location","browser-title","browser-favicon"].forEach((name)=>listen(name,(event)=>{if(!matches(event.payload))return;clearTimeout(state.nativeBrowserTabRefreshTimer);state.nativeBrowserTabRefreshTimer=setTimeout(()=>refreshInspectionBrowserTabs().catch(()=>{}),80);}).catch(()=>{}));
  }

  function syncComposerDensity(){const composer=$("composer"),width=composer.getBoundingClientRect().width;composer.classList.toggle("compact-toolbar",width<620);composer.classList.toggle("tight-toolbar",width<500);}
  function beginInspectionResize(event){
    if(event.button!==0)return;event.preventDefault();
    const handle=$("inspectionResize"),bounds=inspectionWidthBounds();
    cancelAnimationFrame(state.inspectionResizeFrame);state.inspectionResizeFrame=0;
    state.inspectionResizeStart={x:event.clientX,latestX:event.clientX,width:state.inspectionWidth,pointerId:event.pointerId,bounds};
    document.body.classList.add("inspection-resizing");handle.classList.add("dragging");handle.setAttribute("aria-valuemin",String(bounds.min));handle.setAttribute("aria-valuemax",String(bounds.max));handle.setPointerCapture?.(event.pointerId);
  }
  function flushInspectionResize(){
    state.inspectionResizeFrame=0;const start=state.inspectionResizeStart;if(!start)return;
    const width=applyInspectionWidth(start.width+(start.x-start.latestX),false,start.bounds);$("inspectionResize")?.setAttribute("aria-valuenow",String(width));
  }
  function moveInspectionResize(event){
    const start=state.inspectionResizeStart;if(!start||event.pointerId!==start.pointerId)return;event.preventDefault();start.latestX=event.clientX;
    if(!state.inspectionResizeFrame)state.inspectionResizeFrame=requestAnimationFrame(flushInspectionResize);
  }
  function endInspectionResize(event){
    const start=state.inspectionResizeStart;if(!start||event.pointerId!==start.pointerId)return;
    if(state.inspectionResizeFrame){cancelAnimationFrame(state.inspectionResizeFrame);flushInspectionResize();}
    state.inspectionResizeStart=null;document.body.classList.remove("inspection-resizing");const handle=$("inspectionResize");handle.classList.remove("dragging");try{handle.releasePointerCapture?.(start.pointerId);}catch{}
    applyInspectionWidth(state.inspectionWidth,true,start.bounds);
  }

  // A coworker working for someone else (Tibo or Rory handed it a task)
  // streams its live events into the asker's conversation, not this one.
  // While it works and nothing arrives live, re-read its saved history so the
  // request, its work and its answer show up here as they happen.
  function startQuietWorkCatchUp(){
    if(state.quietCatchUpTimer)return;
    state.quietCatchUpTimer=setInterval(()=>{
      if(preview||document.hidden||!state.item||state.quietCatchUpBusy)return;
      const activity=ui.activityFor?.(state.item);
      if(!LIVE_CONVERSATION_STATUSES.has(activity?.status))return;
      if(Date.now()-(state.lastJournalEventAt||0)<4000)return;
      state.quietCatchUpBusy=true;
      catchUpConversation(activeSelectionToken(),{quiet:true}).finally(()=>{state.quietCatchUpBusy=false;});
    },4000);
  }
  function bind(){
    startQuietWorkCatchUp();
    addEventListener("phoenix:provider-accounts-changed",()=>{if(state.item)refreshModels(activeSelectionToken()).catch(()=>{});});
    initializeComposerInput();
    if ($("historyButton")) $("historyButton").onclick = () => togglePromptHistory($("historyButton").getAttribute("aria-expanded") !== "true");
    document.addEventListener("pointerdown", (event) => {
      if (!event.target.closest("#historyButton, #conversationPromptRail")) togglePromptHistory(false);
    });
    document.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      if (!$("menuLayer").hidden) {
        event.preventDefault(); event.stopImmediatePropagation(); ui.closeLayers(); return;
      }
      if ($("historyButton")?.getAttribute("aria-expanded") === "true") {
        event.preventDefault(); event.stopImmediatePropagation(); togglePromptHistory(false, true);
      }
    }, true);
    const toastBoundsObserver=new MutationObserver(scheduleNativeBrowserBounds);toastBoundsObserver.observe($("toastRegion"),{childList:true,subtree:true,attributes:true});
    const toastSizeObserver=new ResizeObserver(scheduleNativeBrowserBounds);toastSizeObserver.observe($("toastRegion"));
    addEventListener("pagehide",()=>{toastBoundsObserver.disconnect();toastSizeObserver.disconnect();},{once:true});
    bindNativeBrowserTabEvents();
    $("composer").onsubmit=(event)=>{event.preventDefault();const request=composerRequest();if(state.working&&!request){if(state.sendInFlight||Date.now()-state.lastSendAt<1200)return;stopTurn();}else submitTurn();};
    $("sendButton").onfocus=()=>syncSendOrb();
    $("sendButton").onblur=()=>syncSendOrb();
    $("sendButton").onpointerenter=()=>{state.sendOrbHovered=true;syncSendOrb();};
    $("sendButton").onpointerleave=()=>{state.sendOrbHovered=false;syncSendOrb();};
    $("composerInput").oninput=(event)=>{if(!event.isComposing)normalizeComposerTokens(false);autosize();persistComposerDraft();renderImageCommentState();};$("composerInput").onpaste=pasteComposerImages;$("composerInput").onclick=(event)=>{const token=event.target.closest(".composer-inline-agent");if(!token)return;event.preventDefault();token.remove();state.groupEveryone=Boolean($("composerInput").querySelector('[data-everyone="true"]'));state.mentions=[...$("composerInput").querySelectorAll("[data-composer-agent]")].map((node)=>node.dataset.composerAgent);autosize();persistComposerDraft();};$("composerInput").onkeydown=(event)=>{if(handlePickerKeys(event,$("mentionPicker"),chooseMention))return;const slash=$("slashPicker");if(handlePickerKeys(event,slash,(active)=>{const exact=composerText("request").trim()===`/${active.dataset.slash}`;if(exact){closeSlash();submitTurn();}else chooseSlash(active);} ))return;if(event.key==="Enter"&&event.shiftKey&&!event.isComposing){event.preventDefault();insertComposerLineBreak();return;}const shortcut=document.documentElement.dataset.sendShortcut||"enter",send=event.key==="Enter"&&!event.isComposing&&(shortcut==="cmd_enter"?(event.ctrlKey||event.metaKey):true);if(send){event.preventDefault();submitTurn();}};
    $("composer").addEventListener("dragenter",(event)=>{if(draggedComposerImages(event.dataTransfer).length){event.preventDefault();setComposerDropTarget(true);}});$("composer").addEventListener("dragover",(event)=>{if(draggedComposerImages(event.dataTransfer).length){event.preventDefault();event.dataTransfer.dropEffect="copy";setComposerDropTarget(true);}});$("composer").addEventListener("dragleave",(event)=>{if(!$("composer").contains(event.relatedTarget))setComposerDropTarget(false);});$("composer").addEventListener("drop",dropComposerImages);addEventListener("drop",(event)=>{if(event.target.closest?.("#composer"))return;setComposerDropTarget(false);if(draggedComposerImages(event.dataTransfer).length)event.preventDefault();});addEventListener("dragend",()=>setComposerDropTarget(false));
    $("attachButton").onclick=async()=>{
      const button=$("attachButton");if(button.disabled)return;
      button.disabled=true;
      try{closeAttachmentMenu();await pickPathAttachment("file");}
      finally{button.disabled=false;if(button.isConnected)button.focus({preventScroll:true});}
    };
    $("attachmentInput").onchange=async(event)=>{await addAttachments([...event.target.files]);event.target.value="";};
    document.addEventListener("pointerdown",(event)=>{if(!$("attachmentMenu").contains(event.target))closeAttachmentMenu();});
    $("attachmentTray").onclick=(event)=>{const inspect=event.target.closest("[data-inspect-attachment]");if(inspect){const file=state.attachments[Number(inspect.dataset.inspectAttachment)];if(file)openImageInspector({path:file.path,source:file.preview,name:file.name,comments:file.comments});return;}const i=event.target.closest("button")?.dataset.remove;if(i!=null){state.attachments.splice(Number(i),1);renderAttachments();}};
    $("composerCommentPopover").onpointerenter=showComposerCommentPopover;$("composerCommentPopover").onpointerleave=()=>hideComposerCommentPopover();
    $("mentionPicker").onclick=(event)=>{const b=event.target.closest("button");if(b)chooseMention(b);};
    $("slashPicker").onclick=(event)=>{const b=event.target.closest("button");if(b)chooseSlash(b);};
    $("mentionTray").onclick=(event)=>{const b=event.target.closest("[data-remove-mention]");if(b)removeMention(b);};
    $("permissionButton").onclick=openPermission;
    const options=$("composerOptions"),optionsSummary=options.querySelector("summary");
    options.addEventListener("toggle",()=>optionsSummary.setAttribute("aria-expanded",String(options.open)));
    document.addEventListener("pointerdown",(event)=>{if(options.open&&!options.contains(event.target)&&!event.target.closest?.(".goo-layer,#menuLayer,#modalLayer"))options.open=false;});
    document.addEventListener("keydown",(event)=>{if(event.key==="Escape"&&!event.defaultPrevented&&options.open){event.preventDefault();options.open=false;optionsSummary.focus();}});
    addEventListener("phoenix:select-conversation",()=>{options.open=false;});
    // Menus close on any outside press before the click lands; remember
    // whether this button's menu was open so the click toggles it shut.
    window.addEventListener("pointerdown",(event)=>{if(event.target.closest?.("#permissionButton"))state.permissionWasOpen=Boolean(state.permissionPopover?.isConnected);},true);$("modelButton").onclick=openModels;$("contextButton").onclick=(event)=>{event.stopPropagation();openContext();};$("voiceButton").onclick=toggleVoice;
    $("contextControl").onclick=(event)=>{if(event.target.closest("#contextButton"))return;openGooContext();};
    $("reasoningControl").onclick=()=>openGooReasoning();$("stashButton").onclick=()=>{if(!stashCurrentPrompt())openPromptStash();};$("stashButton").oncontextmenu=(event)=>{event.preventDefault();openPromptStash();};syncStashButton();for(const id of ["contextControl","reasoningControl"])$(id).addEventListener("keydown",(event)=>{if((event.key==="Enter"||event.key===" ")&&event.target===event.currentTarget){event.preventDefault();event.currentTarget.click();}});
    $("contextRange").oninput=(event)=>{if(state.item?.kind==="group")return;const maximum=selectedModelContext().maximum||Number(event.currentTarget.max)||1,value=Number(event.currentTarget.value)||maximum;$("composerContext").textContent=formatContextWindow(value);liquidFill($("contextRangeFill"),value/maximum*100);};
    $("contextRange").onchange=(event)=>{if(state.item?.kind!=="group")saveModelContextWindow(Number(event.currentTarget.value));};
    $("reasoningRange").oninput=(event)=>{if(state.item?.kind==="group")return;const levels=selectedEffortLevels(),choices=[null,...levels],index=Math.max(0,Math.min(choices.length-1,Number(event.currentTarget.value)||0)),value=choices[index];$("composerReasoning").textContent=value?value[0].toUpperCase()+value.slice(1):levels.length?"Default":"Built in";liquidFill($("reasoningRangeFill"),choices.length>1?index/(choices.length-1)*100:0);};
    $("reasoningRange").onchange=async(event)=>{if(state.item?.kind==="group")return;const levels=selectedEffortLevels(),choices=[null,...levels],index=Math.max(0,Math.min(choices.length-1,Number(event.currentTarget.value)||0)),value=choices[index],previous=state.reasoning;state.reasoning=value;if(value)localStorage.setItem(`phoenix-reasoning:${modelLane()}`,value);else localStorage.removeItem(`phoenix-reasoning:${modelLane()}`);updateComposerLabels();if(preview||!state.selectedModel)return;try{await persistModelLane(state.selectedModel,value);}catch(error){state.reasoning=previous;updateComposerLabels();ui.toast(error.message||String(error),true);}};
    // The environment summary was retired; its old toggle is gone.
    state.summaryOpen=false;$("inspectionExpandButton").onclick=toggleInspectionExpanded;
    $("stageSidebarButton").onclick=()=>toggleInspectionSidebar();
    addEventListener("keydown",(event)=>{if((event.ctrlKey||event.metaKey)&&!event.shiftKey&&!event.altKey&&event.key.toLowerCase()==="b"){event.preventDefault();toggleInspectionSidebar();}});
    document.querySelector(".inspection-tabbar").addEventListener("click",(event)=>{const button=event.target.closest("[data-inspection-tab]"),tabId=button?.dataset.browserTabId,imageId=button?.dataset.imageTabId;if(event.target.closest("[data-close-browser-tab]")){event.stopPropagation();closeInspectionBrowserTab(tabId);return;}if(event.target.closest("[data-close-image-tab]")){event.stopPropagation();closeInspectionImageTab(imageId);return;}if(button){showInspectionSidebar(button.dataset.inspectionTab);if(tabId)selectInspectionBrowserTab(tabId);if(imageId)selectInspectionImageTab(imageId);}});
    document.querySelector(".inspection-tabbar").addEventListener("keydown",(event)=>{if(!["ArrowLeft","ArrowRight","Home","End"].includes(event.key))return;const tabs=[...document.querySelectorAll('.inspection-tabbar [role="tab"]:not([hidden])')],current=Math.max(0,tabs.indexOf(event.target.closest('[role="tab"]'))),next=event.key==="Home"?0:event.key==="End"?tabs.length-1:(current+(event.key==="ArrowLeft"?-1:1)+tabs.length)%tabs.length;event.preventDefault();tabs[next]?.click();setTimeout(()=>{const selected=document.querySelector('.inspection-tabbar [role="tab"][aria-selected="true"]');selected?.focus();},0);});
    $("inspectionResize").onpointerdown=beginInspectionResize;$("inspectionResize").onpointermove=moveInspectionResize;$("inspectionResize").onpointerup=endInspectionResize;$("inspectionResize").onpointercancel=endInspectionResize;$("inspectionResize").onlostpointercapture=endInspectionResize;$("inspectionResize").onkeydown=(event)=>{if(!["ArrowLeft","ArrowRight"].includes(event.key))return;event.preventDefault();applyInspectionWidth(state.inspectionWidth+(event.key==="ArrowLeft"?16:-16),true);};
    $("inspectionSourcesGrid").onclick=(event)=>{const index=event.target.closest("[data-source-card]")?.dataset.sourceCard,source=conversationSources()[Number(index)];if(source)openImageInspector(source);};
    $("imageCommentToggle").onclick=()=>setImageCommentMode(!state.imageCommentMode);$("imageCommentCancel").onclick=clearImageComments;$("imageCommentSend").onclick=()=>sendCommentedComposer().catch((error)=>ui.toast(error.message||String(error),true));$("imageCommentSave").onclick=()=>saveImageComment().catch((error)=>ui.toast(error.message||String(error),true));$("imageRemoveTab").onclick=()=>closeInspectionImageTab();$("imageResizeToggle").onclick=toggleImageActualSize;$("inspectionImageStage").onclick=placeImageComment;$("imageCommentText").oninput=(event)=>{if(!state.imageCommentDraft)return;const input=event.currentTarget;state.imageCommentDraft.text=input.value;$("imageCommentSave").disabled=!state.imageCommentDraft.text.trim();requestAnimationFrame(()=>{input.scrollLeft=input.scrollWidth;});};$("imageCommentText").onkeydown=(event)=>{if(event.key==="Enter"&&!event.isComposing){event.preventDefault();saveImageComment().catch((error)=>ui.toast(error.message||String(error),true));}};
    $("approvalStack").onclick=(event)=>{const button=event.target.closest("button");if(button)approvalAction(button);};$("approvalStack").onsubmit=(event)=>{if(event.target.matches(".approval-vault")){event.preventDefault();unlockApprovalVault(event.target.closest(".approval-card"));}};$("approvalStack").oninput=(event)=>{if(!event.target.matches(".approval-inline-custom [data-ask-custom-input]"))return;const card=event.target.closest(".approval-card"),model=card._askState;model.customAnswers[model.index]=event.target.value;model.answers[model.index]=null;model.multiSelections[model.index].clear();card.querySelectorAll(".qa-option.selected").forEach((option)=>{option.classList.remove("selected");option.setAttribute("aria-checked","false");});card.querySelector("[data-ask-continue]").disabled=!event.target.value.trim();};$("approvalStack").onkeydown=(event)=>{if(event.key!=="Enter"||!event.target.matches("[data-ask-custom-input]"))return;event.preventDefault();const card=event.target.closest(".approval-card");if(event.target.closest(".approval-inline-custom")){const value=event.target.value.trim();if(value)recordAskAnswer(card,value);return;}approvalAction(card.querySelector("[data-ask-custom-save]"));};$("queueList").onclick=(event)=>{const b=event.target.closest("[data-queue]");if(b)queueAction(b);};
    $("queueBlock").addEventListener("toggle",()=>{if($("queueBlock").hidden)return;state.queueExpandedBySession.set(conversationIdentity(),$("queueBlock").open);});
    $("taskBlock").addEventListener("toggle",()=>{if($("taskBlock").hidden)return;state.taskPlanExpanded=$("taskBlock").open;localStorage.setItem("phoenix-task-plan-expanded",state.taskPlanExpanded?"1":"0");});
    $("teachAgentButton").onclick=openTeach;$("cancelTeaching").onclick=cancelBrowserFlow;$("finishTeaching").onclick=finishBrowserFlow;
    // Links in a chat open in that agent's own browser in the right sidebar
    // (a new tab), not the system browser. Ctrl/Cmd-click still opens outside.
    $("conversationFeed").addEventListener("click",(event)=>{
      const link=event.target.closest?.("a[href]");if(!link||event.defaultPrevented||event.button!==0||event.ctrlKey||event.metaKey||event.shiftKey)return;
      let url;try{url=new URL(link.getAttribute("href"),location.href);}catch{return;}
      if(!/^https?:$/.test(url.protocol)||preview)return;
      event.preventDefault();event.stopPropagation();openLinkInAgentBrowser(url.href);
    },true);
    $("browserNewTab").onclick=async()=>{try{const key=conversationKeyOf(state.item),hadConversationBrowser=Boolean(inspectionConversationState(key)?.browser||(state.browserOwnerId&&state.browserBoundKey===key));showInspectionSidebar("browser");if(state.browserBoundKey!==key)await resumeInspectionBrowser();if(!state.browserOwnerId||state.browserBoundKey!==key)await openBrowser(state.item?.kind==="agent"?state.item.id:"phoenix","browse");const active=state.browserTabs.find((tab)=>tab.active)||state.browserTabs[0],pristineFirstTab=!hadConversationBrowser&&state.browserTabs.length===1&&isBlankTabUrl(active?.url);if(!pristineFirstTab)await navigateBrowser("about:blank",true);hideBrowserCursor();focusBrowserAddress();}catch(error){ui.toast(error.message||String(error),true);}};
    $("browserAddress").onfocus=()=>{state.browserAddressEditing=true;};$("browserAddress").oninput=()=>{state.browserAddressEditing=true;};$("browserAddress").onblur=()=>{state.browserAddressEditing=false;if(!state.browserAddressPending)syncBrowserAddress(state.browserFrameUrl||"about:blank",true);};$("browserAddress").onkeydown=(event)=>{if(event.key==="Enter"){event.preventDefault();submitBrowserAddress(event.currentTarget);}};
    document.querySelector('[data-browser-action="back"]').onclick=()=>runBrowserToolbarAction("back");
    document.querySelector('[data-browser-action="forward"]').onclick=()=>runBrowserToolbarAction("forward");
    document.querySelector('[data-browser-action="reload"]').onclick=()=>runBrowserToolbarAction("reload");
    renderBrowserSettingsMenu();$("browserSettings").onclick=(event)=>{event.stopPropagation();setBrowserMenuOpen("browserSettingsMenu",$("browserSettingsMenu").hidden);};$("browserExtensions").onclick=(event)=>{event.stopPropagation();const open=$("browserExtensionsMenu").hidden;setBrowserMenuOpen("browserExtensionsMenu",open);if(open)renderBrowserExtensionsMenu();};$("browserExtensionsMenu").onclick=browserExtensionsMenuClick;$("browserExtensionsMenu").onchange=browserExtensionsMenuChange;$("browserSettingsMenu").onclick=(event)=>{const action=event.target.closest("[data-browser-setting]")?.dataset.browserSetting;if(action)runBrowserSetting(action);};document.addEventListener("pointerdown",(event)=>{if(!event.target.closest("#browserSettings,#browserSettingsMenu,#browserExtensions,#browserExtensionsMenu")){setBrowserMenuOpen("browserSettingsMenu",false);setBrowserMenuOpen("browserExtensionsMenu",false);}});
    $("browserCanvas").onclick=clickBrowserFrame;$("browserFrame").onclick=clickBrowserFrame;$("browserViewport").onwheel=wheelBrowserFrame;$("browserViewport").onkeydown=browserKey;
    $("browserDownloadOpen").onclick=()=>openBrowserDownload(false);$("browserDownloadReveal").onclick=()=>openBrowserDownload(true);$("browserDownloadDismiss").onclick=()=>{const file=state.browserDownloadCurrent;if(file)state.browserDownloadSnapshot.set(file.path,file.modified_ms);state.browserDownloadCurrent=null;$("browserDownloadShelf").hidden=true;scheduleNativeBrowserBounds();};
    $("jumpLatest").onclick=()=>scrollLatest(true);
    $("conversationFeed").addEventListener("scroll", onFeedScroll, {passive:true});
    $("conversationFeed").addEventListener("wheel",(event)=>{if(event.deltaY<0)state.historyScrollIntentUntil=performance.now()+1000;},{passive:true});
    $("conversationFeed").addEventListener("keydown",(event)=>{if(["ArrowUp","PageUp","Home"].includes(event.key))state.historyScrollIntentUntil=performance.now()+1000;});
    $("conversationFeed").addEventListener("click",(event)=>{
      const relayed=event.target.closest("[data-relayed-image-path]");if(relayed){openImageInspector({path:relayed.dataset.relayedImagePath,source:relayed._inspectionSource||"",name:relayed.querySelector("span")?.textContent||"Image"});return;}
      const generated=event.target.closest("[data-inspect-image-path]");if(generated){openImageInspector({path:generated.dataset.inspectImagePath,source:generated._inspectionSource||"",name:generated.querySelector("small")?.textContent||"Generated image"});return;}
      const messageImage=event.target.closest("[data-message-image]");if(messageImage){const row=messageImage.closest(".user-message"),file=row?._messageAttachments?.[Number(messageImage.dataset.messageImage)];if(file)openImageInspector({path:file.path,source:file.preview,name:file.name});return;}
      const deletePrompt=event.target.closest("[data-delete-prompt]");
      if(deletePrompt){permanentlyDeleteTurn(deletePrompt,"Prompt");return;}
      const deleteAgent=event.target.closest("[data-delete-agent-turn]");
      if(deleteAgent){permanentlyDeleteTurn(deleteAgent,"Agent");return;}
      const groupWork=event.target.closest(".group-work-agent");
      if(groupWork){toggleGroupTurnWork(groupWork.closest(".group-work-cluster"));return;}
      const reasoningSummary=event.target.closest(".reasoning-subgroup>summary,.reasoning-cluster>summary");
      if(reasoningSummary){const disclosure=reasoningSummary.parentElement;if(disclosure.classList.contains("summary-only"))return;event.preventDefault();disclosure.open=!disclosure.open;reasoningSummary.setAttribute("aria-expanded",String(disclosure.open));return;}
      const agentChip=event.target.closest("[data-open-agent]");
      if(agentChip){const id=agentChip.dataset.openAgent;if(id)ui.selectItem?.({kind:"agent",id});return;}
      const localLink=event.target.closest("[data-open-local-path]");
      if(localLink){const path=localMarkdownPath(localLink.dataset.openLocalPath||"");if(!path)return;
        // A local web page opens in this conversation's browser on the right,
        // like any page the agent shows; other files use the system opener.
        if(/\.html?$/i.test(path)&&path.startsWith("/")){(async()=>{try{if(state.browserBoundKey!==conversationKeyOf(state.item)||!state.browserOwnerId)await openBrowser(state.item?.kind==="agent"?state.item.id:"phoenix","browse");await navigateBrowser(`file://${encodeURI(path)}`,true);}catch(error){ui.toast(`Could not open page: ${error.message||error}`,true);}})();return;}
        ui.invoke("open_workspace_file",{path,workspace:state.workspace||null}).catch((error)=>ui.toast(`Could not open file: ${error.message||error}`,true));return;}
      const copy=event.target.closest("[data-copy-answer]");
      if(copy){copyAnswer(copy);return;}
      const moreEdits=event.target.closest("[data-show-more-edits]");if(moreEdits){const card=moreEdits.closest(".turn-edit-summary");card?.querySelectorAll(".turn-edit-file.extra").forEach((row)=>row.hidden=false);moreEdits.remove();return;}
      const toolCopy=event.target.closest(".tool-detail-copy");
      if(toolCopy){const row=toolCopy.closest(".work-tool"),text=row?.dataset.copyText||"",label=toolCopy.getAttribute("aria-label");if(text)navigator.clipboard?.writeText(text).catch(()=>{});toolCopy.classList.add("copied");toolCopy.innerHTML=window.PhoenixAgentKit.icon("check");toolCopy.setAttribute("aria-label","Copied");clearTimeout(toolCopy._copyTimer);toolCopy._copyTimer=setTimeout(()=>{if(toolCopy.isConnected){toolCopy.classList.remove("copied");toolCopy.innerHTML=window.PhoenixAgentKit.icon("copy");toolCopy.setAttribute("aria-label",label==="Copied"?"Copy result":label);}},1600);return;}
      const tool=event.target.closest(".work-tool-summary");
      if(tool){if(tool.dataset.empty==="true")return;const row=tool.closest(".work-tool"),open=row.classList.toggle("open"),detail=row.querySelector(".work-tool-detail");tool.setAttribute("aria-expanded",String(open));if(detail)detail.hidden=!open;return;}
    });
    $("conversationPromptRail").onkeydown = (event) => {
      const buttons = [...$("conversationPromptRail").querySelectorAll("button")];
      const index = buttons.indexOf(document.activeElement);
      if (index < 0 || !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1
        : (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
      buttons[next]?.focus();
    };
    $("conversationPromptRail").onclick=(event)=>{const button=event.target.closest("button");if(button){jumpToPrompt(button.dataset.messageIndex);togglePromptHistory(false,true);}};

    addEventListener("keydown",(event)=>{if(event.key!=="Escape"||event.defaultPrevented)return;if(state.voiceCapture){event.preventDefault();void cancelVoiceInput();return;}if(state.imageCommentMode){event.preventDefault();setImageCommentMode(false);return;}if(closeAttachmentMenu()){event.preventDefault();$("attachButton").focus();return;}const ask=$("approvalStack").lastElementChild;if(ask){event.preventDefault();dismissAsk(ask);return;}if(state.working){event.preventDefault();stopTurn();}});
    // Code fences: unfold one in compact view, or copy its source in either.
    $("conversationFeed").addEventListener("click",(event)=>{
      const peek=event.target.closest("[data-code-peek]");
      if(peek){peek.closest(".code-block")?.classList.toggle("peeked");return;}
      const copy=event.target.closest("[data-copy-code]");
      if(copy)copyCodeBlock(copy);
    });
    const rail=$("conversationPromptRail");
    rail.addEventListener("pointermove",(event)=>scheduleRailProximity(event.clientY));
    rail.addEventListener("pointerleave",()=>scheduleRailProximity(null));
    rail.addEventListener("focusin",(event)=>{const button=event.target.closest("button");if(button?.matches(":focus-visible"))syncRailProximity(null,button);});rail.addEventListener("focusout",(event)=>{if(!rail.contains(event.relatedTarget))syncRailProximity(null);});
    // A hidden window must not keep a GL loop alive.
    document.addEventListener("visibilitychange",()=>{if(document.hidden)state.sendOrb?.stop();else syncSendOrb();});
    addEventListener("phoenix:theme-changed",()=>state.sendOrb?.syncColor());
    addEventListener("phoenix:visual-prefs-changed",syncConversationDetail);
    addEventListener("phoenix:select-conversation",(event)=>selectConversationWithInspection(event.detail));
    addEventListener("phoenix:directory-ready",(event)=>selectConversationWithInspection(event.detail));
    addEventListener("phoenix:directory-status",()=>{syncSelectedLiveActivity();if(state.inspectionOpen&&state.inspectionTab==="desktop")updateDesktopViewer(true);});
    addEventListener("phoenix:directory-updated",()=>{syncGroupPals();if(state.inspectionOpen&&state.inspectionTab==="desktop")updateDesktopViewer(true);});
    addEventListener("phoenix:terminal-tabs",(event)=>{state.terminalProcesses=Array.isArray(event.detail?.tabs)?event.detail.tabs.map((tab)=>({...tab})):[];syncActivitySummary();});
    addEventListener("phoenix:settings-visibility",event=>{syncBrowserChrome();if(event.detail?.open===false)void refreshModels();});
    addEventListener("phoenix:modal-visibility",syncBrowserChrome);
    let readingSavedForReentry=false;
    addEventListener('phoenix:review-reentry',()=>{persistReadingPosition();readingSavedForReentry=true;});
    addEventListener("beforeunload",()=>{persistReadingPosition();readingSavedForReentry=true;});
    // Every renderer reload captures before teardown. The native surface
    // changes layout during pagehide; that later geometry must not replace the
    // user's already-captured paragraph offset.
    addEventListener("pagehide",()=>{if(!readingSavedForReentry)persistReadingPosition();persistComposerDraft();captureInspectionConversation();invalidateComposerIngress();flushDisplayJournal();});
    document.addEventListener("visibilitychange",()=>{if(document.visibilityState==="hidden")flushDisplayJournal();});
    if ("ResizeObserver" in window) {
      let composerResizeFrame=0,lastComposerWidth=-1;const composerObserver=new ResizeObserver(entries=>{const width=entries[0]?.contentRect.width;if(Math.abs(width-lastComposerWidth)<.5)return;lastComposerWidth=width;if(composerResizeFrame)return;composerResizeFrame=requestAnimationFrame(()=>{composerResizeFrame=0;syncComposerDensity();autosize();});});composerObserver.observe($("composer"));addEventListener("pagehide",()=>{composerObserver.disconnect();cancelAnimationFrame(composerResizeFrame);},{once:true});
      let endResizeFrame=0;const endObserver=new ResizeObserver(()=>{if(endResizeFrame)return;endResizeFrame=requestAnimationFrame(()=>{endResizeFrame=0;syncComposerEnd();});});endObserver.observe($("composerZone"));addEventListener("pagehide",()=>{endObserver.disconnect();cancelAnimationFrame(endResizeFrame);},{once:true});
      new ResizeObserver(scheduleBrowserResize).observe($("browserViewport"));
      new ResizeObserver(()=>{if(state.activeInspectionImageId)renderImageCommentState();}).observe($("inspectionImageCanvas"));
      new ResizeObserver(()=>{syncWorkspaceLeft();if(state.inspectionOpen&&state.inspectionExpanded)scheduleNativeBrowserBounds();}).observe($("companySidebar"));
    } else addEventListener("resize", () => { syncComposerDensity(); syncComposerEnd(); applyInspectionWidth(); });
    new MutationObserver(()=>{syncWorkspaceLeft();if(document.documentElement.classList.contains("sidebar-transitioning"))return;if(state.inspectionOpen)scheduleNativeBrowserBounds();}).observe(document.body,{attributes:true,attributeFilter:["class"]});
    addEventListener("phoenix:sidebar-transition-end",()=>{syncWorkspaceLeft();syncComposerDensity();if(state.inspectionOpen)scheduleNativeBrowserBounds();});
    addEventListener("resize",()=>{if(state.inspectionOpen&&innerWidth<=760&&!document.body.classList.contains("sidebar-collapsed")){state.inspectionRestoreSidebar=true;$("sidebarToggle")?.click();}applyInspectionWidth();syncWorkspaceLeft();positionComposerCommentPopover();});
    syncPanelControlLocation();applyInspectionExpanded(state.inspectionExpanded,false);applyInspectionWidth();toggleActivitySummary(state.summaryOpen,false);syncActivitySummary();
    syncComposerDensity();
    syncComposerEnd();
  }

  // Test-only wire receiver calls the same consumer and native renderer as
  // SubscribeJournal. It is absent from production unless this fixture exists.
  if (window.PhoenixFluffyFixture) window.PhoenixFluffyPreviewWire = Object.freeze({
    receive(value) { const context=fluffyContext(); consumeFluffyWire(value,context); if(value.Story)renderStory(value.Story);if(value.StoryReplay)renderStory(value.StoryReplay,true); },
    context:()=>fluffyContext(),
    begin(event) { clearFeed();replaceDisplayRows([],false);state.activeTurnId=event.execution.turn_id;appendDisplay("story",{...event,turn_id:event.execution.turn_id},true,false);setWorking(true);const prompt=renderUser(event.text);beginTurnActivity(prompt,event.agent);consumeFluffyWire({Story:event},fluffyContext()); },
  });
  window.PhoenixConversation=Object.freeze({openBrowser,closeBrowser,openImageInspector,showInspectionSidebar,closeInspectionSidebar:hideInspectionSidebar,openTeach,startTeaching,startTeachingRevision,renderApproval,refreshPromptRail:renderPromptRail,refreshPanelBounds:scheduleNativeBrowserBounds,applySettings,setInitialVisibleTurns,workspace:()=>state.workspace,flushPresentationState(){window.PhoenixQuestionDrafts?.persistAll();persistReadingPosition();persistComposerDraft();captureInspectionConversation();}});
  new MutationObserver(syncMessageGroups).observe($("conversationFeed"),{childList:true});
  bind(); renderVoiceState(); autosize();void recoverVoiceCapture();
  if (ui.state.view) selectConversationWithInspection({item:ui.state.selected,sessionId:ui.activityFor(ui.state.selected)?.canonical_session_id});
  // Keep synthetic histories and test machinery out of normal startup.
  if(preview&&previewShot){
    import("./conversation-previews.js?v=20260930-repair").then(({installConversationPreviews})=>installConversationPreviews({
      ui,
      $,
      preview,
      previewShot,
      state,
      DISPLAY_FEED_KEY,
      icons,
      CHROME_ICON,
      escape,
      conversationKey,
      conversationIdentity,
      selectionIsCurrent,
      activeSelectionToken,
      composerText,
      renderComposerText,
      composerCaretOffset,
      setComposerCaretOffset,
      normalizeComposerTokens,
      insertComposerLineBreak,
      markdown,
      relayedImageReferences,
      knownAgentProfile,
      visibleAnswerText,
      avatar,
      paintFeed,
      clearFeed,
      queueOlderConversationTurns,
      repaintConversation,
      humanFailureDetail,
      turnFailureCard,
      renderUser,
      isAuthoredBoundaryEntry,
      isHandoffReturnEntry,
      cloneDisplayValue,
      displayRole,
      displaySemantic,
      pendingOwnedStories,
      ownedStoryTurn,
      flushOwnedStories,
      displayTurnId,
      repairRecoveredRowOrder,
      replaceDisplayRows,
      appendDisplay,
      parseDisplayRows,
      repairRepeatedCatchUpTurns,
      reconcileAsks,
      renderDisplayEntry,
      foldRestoredWork,
      renderAnswer,
      markQuestionContinuations,
      createReasoningSubgroup,
      armReasoningCursor,
      renderCommentary,
      workClusterMarkup,
      beginTurnActivity,
      syncConversationDetail,
      appendCompletedToolRow,
      traceCategory,
      updateTraceSubgroup,
      ensureTraceSubgroup,
      handoffState,
      renderReturn,
      renderHandoff,
      renderGroupMemberStatus,
      renderStory,
      renderHistory,
      conversationKeyOf,
      selectConversationWithInspection,
      syncBrowserChrome,
      applyInspectionExpanded,
      summaryHandoffs,
      summaryBackgroundProcesses,
      toggleActivitySummary,
      syncActivitySummary,
      renderInspectionBrowserTabs,
      setInspectionTab,
      showInspectionSidebar,
      hideInspectionSidebar,
      renderInspectionSources,
      setImageCommentMode,
      renderImageCommentState,
      openImageInspector,
      journalStoryDisposition,
      submitTurn,
      syncSendMode,
      syncSendOrb,
      syncTeamPresence,
      setWorking,
      renderTasks,
      renderQueue,
      renderApproval,
      renderApprovalQuestion,
      resolveAskDisplay,
      closeApproval,
      contextUsageStorageKey,
      storedContextUsage,
      persistContextUsage,
      updateContext,
      selectedModelContext,
      applyLocalModelContext,
      openModelContext,
      renderModelLabel,
      openModels,
      handleSlashCommand,
      autosize,
      renderMentionTray,
      pasteComposerImages,
      dropComposerImages,
      browserStoryOwner,
      closeAttachmentMenu,
      toggleAttachmentMenu,
      renderAttachments,
      targetName,
      syncBrowserAddress
    })).catch((error)=>{document.documentElement.dataset.previewLoadError=String(error);document.title="FAIL conversation preview initialization";});
  }
})();
