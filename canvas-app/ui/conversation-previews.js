// Preview-only acceptance fixtures. Never imported by ordinary conversations.
export function installConversationPreviews({
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
  receiptReviewChanges,
  renderInspectionChanges,
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
}) {
  async function runConversationAcceptance() {
    // This fixture exercises the actual renderer and computed layout. It is
    // deliberately opt-in so production startup and the user's journal remain
    // untouched; release verification can open ?shot=conversation-acceptance
    // and read one deterministic PASS/FAIL result from the document title.
    state.inspectionExpanded=false;state.initialVisibleTurns=50;applyInspectionExpanded(false,false);let complete=false;
    const finish=(checks,error=null)=>{
      if(complete)return;complete=true;
      const failures=error?[`fixture: ${error.message||error}`]:Object.entries(checks).filter(([,passed])=>!passed).map(([name])=>name);
      document.documentElement.dataset.conversationAcceptance=failures.length?"fail":"pass";
      document.documentElement.dataset.conversationAcceptanceChecks=JSON.stringify(checks||{});
      document.title=failures.length?`FAIL conversation acceptance: ${failures.join(", ")}`:"PASS conversation acceptance";
      window.PhoenixConversationAcceptance=Object.freeze({checks:checks||{},failures});
    };
    const deadline=setTimeout(()=>finish({},new Error("fixture timed out")),30000);
    const nextTask=(delay=45)=>new Promise((resolve)=>setTimeout(resolve,delay));
    const mark=(value)=>{document.documentElement.dataset.conversationAcceptanceStep=value;};
    try{
      mark("directory");
      for(let attempt=0;attempt<40&&!state.item;attempt+=1)await nextTask(25);
      clearFeed();closeApproval();replaceDisplayRows([],false);state.activeTools.clear();state.toolRows=[];
      $("composerInput").value="/comp";$("composerInput").focus();setComposerCaretOffset($("composerInput").value.length);autosize();
      const slashPickerDiscoversCompact=Boolean(!$("slashPicker").hidden&&$("slashPicker").querySelector('[data-slash="compact"]'));
      mark("slash");
      const slashCommandHandled=await handleSlashCommand("/compact");
      mark("slash-done");
      const slashCommandStayedLocal=Boolean(slashCommandHandled&&$("conversationFeed").querySelector(".slash-notice")?.textContent.includes("Automatic compaction is active"));
      clearFeed();
      state.mentions=[];state.attachments=[];state.groupEveryone=false;state.sendOrbHovered=false;
      $("composerInput").value="";renderMentionTray();renderAttachments();autosize();syncSendMode();
      const acceptancePrompt="Email maya@example.com if needed; @scribe can open the account page and sign me in.";
      appendDisplay("history",{role:"user",text:acceptancePrompt},true);
      const prompt=renderUser(acceptancePrompt);
      state.turnStartedAt=Date.now()-31000;setWorking(true);beginTurnActivity(prompt);
      const visible=(node)=>Boolean(node&&node.getClientRects().length&&getComputedStyle(node).visibility!=="hidden"&&getComputedStyle(node).display!=="none");
      const activityCursors=()=>[...$("conversationFeed").querySelectorAll(".reasoning-cluster-cursor,.reasoning-subgroup-cursor,.trace-subgroup-cursor,.context-compaction-cursor,.work-tool-cursor")].filter(visible);
      const initialHeader=$("conversationFeed").querySelector(".group-work-cluster.live .group-work-agent");
      const liveWorkStartsCollapsed=initialHeader?.getAttribute("aria-expanded")==="false";
      initialHeader?.click();
      const immediateThinkingCursor=Boolean($("conversationFeed").querySelector(".reasoning-subgroup.pending-activity.activity-current .reasoning-subgroup-label")?.textContent==="Thinking"&&activityCursors().length===1);
      const activeWork = $("conversationFeed").querySelector(".group-work-cluster.live");
      const activeHeader = activeWork?.querySelector(".group-work-agent");
      const expandedWorkKeepsOneSquare = Boolean(activeHeader && !visible(activeHeader.querySelector(".group-work-live")) && activityCursors().length === 1);
      activeHeader?.click();
      const foldedWorkKeepsSquare = Boolean(activeHeader?.getAttribute("aria-expanded") === "false" && visible(activeHeader.querySelector(".group-work-live")) && !visible(activeHeader.querySelector(".group-work-avatar")) && activityCursors().length === 0);
      activeHeader?.click();
      const reopenedWorkReturnsSquareToStep = Boolean(activeHeader?.getAttribute("aria-expanded") === "true" && !visible(activeHeader.querySelector(".group-work-live")) && visible(activeHeader.querySelector(".group-work-avatar")) && activityCursors().length === 1);
      const workingButtonShowsStop = $("sendButton").getAttribute("aria-label") === "Stop agent" && $("sendLabel").textContent === "Stop" && !$("sendButton").disabled;
      const workingActionIsVisible = $("sendButton").classList.contains("orb-live") ? visible($("sendOrb")) : visible($("sendButton").querySelector(".stop-glyph"));
      state.sendOrbHovered=true;syncSendOrb();
      const workingHoverKeepsStop = $("sendLabel").textContent === "Stop";
      state.sendOrbHovered=false;$("composerInput").value="Queue this while Phoenix works";syncSendMode();
      const workingDraftShowsSendNow = $("sendLabel").textContent === "Send" && String($("sendButton").getAttribute("aria-label")||"").startsWith("Send now");
      $("composerInput").value="";syncSendMode();
      // Thinking is live status: it relabels the one working cube and never
      // becomes a transcript row. Messages the agent writes between tool
      // calls are real rows, above the cube, in order.
      const rowsBeforeThinking=state.displayRows.length;
      renderStory({kind:"thinking",agent:"phoenix",text:"**Checking the visible page**\n\nPrivate planning detail."});
      const liveCube=()=>$("conversationFeed").querySelector(".reasoning-subgroup.pending-activity");
      const headingOnlyReasoningHonest=liveCube()?.querySelector(".reasoning-subgroup-label")?.textContent==="Checking the visible page"&&!$("conversationFeed").textContent.includes("Private planning detail");
      renderStory({kind:"commentary",agent:"phoenix",text:"The account form is ready. I’m finding its sign-in control before touching anything."});
      const update=$("conversationFeed").querySelector(".agent-update");
      const actualReasoningExpandsDisclosure=Boolean(update?.textContent.includes("The account form is ready")&&update.nextElementSibling===liveCube());
      renderStory({kind:"commentary",agent:"phoenix",text:"**Verifying the account session**\n\nLegacy provider summary stored as commentary."});
      const reasoningHeadingDoesNotRepeatBody=!$("conversationFeed").textContent.includes("Legacy provider summary");
      renderStory({kind:"reasoning",agent:"phoenix",text:"Verifying the account session"});
      const liveSignals=liveCube()?.querySelector(".reasoning-subgroup-label")?.textContent==="Verifying the account session"?1:0;
      const literalThinkingLabel=!$("conversationFeed").querySelector(".reasoning-cluster");
      const individualThoughtRows=$("conversationFeed").querySelectorAll(".agent-update").length===1;
      const summaryLabeledThoughts=!state.displayRows.slice(rowsBeforeThinking).some((row)=>["thinking","reasoning"].includes(row.value?.kind)||String(row.value?.text||"").includes("Legacy provider summary"));
      const consecutiveThoughtsGrouped=$("conversationFeed").querySelectorAll(".reasoning-subgroup").length===1;
      const reasoningCursorCount=activityCursors().length,oneTravelingReasoningCursor=reasoningCursorCount===1;
      const liveReasoningLeaf=liveCube();
      const persistentCursorCount=activityCursors().length,currentReasoningCursorPersists=Boolean(liveReasoningLeaf?.classList.contains("running")&&persistentCursorCount===1);
      const topThoughtHeaderAbsent=!$("conversationFeed").querySelector(".work-shimmer,.work-trace-toggle,.shimmer-copy");
      const traceFixture=document.createElement("div");traceFixture.className="work-cluster";traceFixture.innerHTML=workClusterMarkup();
      const firstBrowserSegment=ensureTraceSubgroup(traceFixture,"browser_state");firstBrowserSegment.open=false;firstBrowserSegment.dataset.running="1";updateTraceSubgroup(firstBrowserSegment);
      const runningTraceDoesNotAutoExpand=!firstBrowserSegment.open;
      firstBrowserSegment.dataset.running="0";updateTraceSubgroup(firstBrowserSegment);
      const traceThought=createReasoningSubgroup();traceFixture.querySelector(".work-tools").append(traceThought);
      const secondBrowserSegment=ensureTraceSubgroup(traceFixture,"browser_click"),fixtureChildren=[...traceFixture.querySelector(".work-tools").children];
      const interleavedBrowserTimeline=Boolean(firstBrowserSegment!==secondBrowserSegment&&traceFixture.querySelectorAll(".trace-browser").length===2&&fixtureChildren.indexOf(firstBrowserSegment)<fixtureChildren.indexOf(traceThought)&&fixtureChildren.indexOf(traceThought)<fixtureChildren.indexOf(secondBrowserSegment));
      const target='{"app":"Zen","query":"Continue with email","session_id":"session-secret-12345678"}';
      renderStory({kind:"tool_start",agent:"phoenix",tool:"computer_app_locate",target});
      await nextTask();
      const cursorMovedToTool=Boolean($("conversationFeed").querySelector(".trace-subgroup.running.activity-current")&&activityCursors().length===1&&!$("conversationFeed").querySelector(".tool-status.running .pixel-loader"));
      const activeToolGroup=$("conversationFeed").querySelector(".trace-subgroup.running.activity-current");
      const wasToolGroupOpen=activeToolGroup.open;activeToolGroup.open=true;
      const squareBesideLatestCall=visible(activeToolGroup.querySelector(".tool-current .work-tool-cursor"))&&activityCursors().length===1;
      activeToolGroup.open=false;
      const foldedToolGroupKeepsSquare=visible(activeToolGroup.querySelector(".trace-subgroup-cursor"))&&activityCursors().length===1;
      activeToolGroup.open=wasToolGroupOpen;
      const promptTop=prompt.getBoundingClientRect().top-$("conversationFeed").getBoundingClientRect().top;
      renderStory({kind:"tool",agent:"phoenix",tool:"computer_app_locate",target,ok:false,detail:'computer_app_locate: {"ok":false,"query":"Continue with email","error":"no visible element matching Continue with email","closest":[{"text":"Sign in with Google","role":"internal frame","cx":1333,"cy":122}]}' });
      await nextTask();
      const cursorReturnedToThinking=Boolean($("conversationFeed").querySelector(".reasoning-subgroup.pending-activity.running.activity-current .reasoning-subgroup-label")?.textContent==="Thinking"&&activityCursors().length===1);
      const rowsBeforeProviderRetry=state.displayRows.length,providerRetryFixture={kind:"notice",text:"provider temporarily unavailable: codex stream reported an error: Our servers are currently overloaded. Please try again later.; retrying in 8s (attempt 3)"};renderStory(providerRetryFixture);const providerRetryLine=$("conversationFeed").querySelector(".provider-retry-line"),providerRetryLivesInThinking=Boolean(providerRetryLine?.closest(".reasoning-subgroup.running.activity-current")&&providerRetryLine.textContent.includes("Our servers are currently overloaded")&&providerRetryLine.textContent.includes("Retrying in 8s")&&providerRetryLine.textContent.includes("attempt 3")&&activityCursors().length===1&&state.displayRows.length===rowsBeforeProviderRetry);renderStory({kind:"reasoning",agent:"phoenix",text:"Continuing the browser check now that provider capacity returned."});const rowsAfterProviderResume=state.displayRows.length;renderStory(providerRetryFixture,true);const providerRetryClearsAndNeverReplays=!$("conversationFeed").querySelector(".provider-retry-line")&&state.displayRows.length===rowsAfterProviderResume;
      const tasteTarget='{"path":"taste/SKILL.md","trace_id":"trace-design-secret"}';
      renderStory({kind:"tool_start",agent:"phoenix",tool:"design_reference",target:tasteTarget});
      const tasteLiveLabel=$("conversationFeed").querySelector(".work-tool.running .work-tool-name")?.textContent;
      renderStory({kind:"tool",agent:"phoenix",tool:"design_reference",target:tasteTarget,ok:true});
      const imageTarget='{"prompt":"private raw art direction must stay hidden","size":"1536x1024"}';
      renderStory({kind:"tool_start",agent:"phoenix",tool:"image_gen",target:imageTarget});
      const imageLiveLabel=$("conversationFeed").querySelector(".work-tool.running .work-tool-name")?.textContent;
      renderStory({kind:"tool",agent:"phoenix",tool:"image_gen",target:imageTarget,ok:true,detail:"Image generated: artifacts/images/acceptance.png (128 KB, model test-image)."});
      await nextTask(80);
      const completedImageRow=[...$("conversationFeed").querySelectorAll('.work-tool[data-tool="image_gen"]')].at(-1);
      completedImageRow?.querySelector(".work-tool-summary")?.click();
      const imagePreviewVisible=Boolean(completedImageRow?.classList.contains("open")&&completedImageRow.querySelector(".tool-image-preview img"));
      completedImageRow?.querySelector("[data-inspect-image-path]")?.click();await nextTask();
      const imageOpensReviewPane=Boolean(state.inspectionOpen&&state.inspectionTab==="image"&&$("inspectionSidebar").getAttribute("aria-hidden")==="false"&&!$("inspectionImageFigure").hidden);
      renderStory({kind:"tool",agent:"phoenix",tool:"web_search",target:'{"queries":[{"q":"Phoenix durable conversations"}]}',ok:true,detail:'{"results":[{"title":"Example source","url":"https://example.com/phoenix"}]}' });
      renderStory({kind:"tool",agent:"phoenix",tool:"browser_navigate",target:"https://example.com",ok:true});
      renderStory({kind:"ask_pending",id:"acceptance-answered",agent:"phoenix",status:"answered",display_answers:["Keep it compact"],questions:[{header:"Trace density",question:"How should completed edit details appear?",options:["Keep it compact","Leave it open"],multi_select:false}]});
      renderStory({kind:"tool",agent:"phoenix",tool:"read",target:"conversation.css",ok:true});
      renderStory({kind:"tool",agent:"phoenix",tool:"str_replace",target:"conversation.js",ok:true,detail:"+46 -15"});
      renderStory({kind:"tool",agent:"phoenix",tool:"bash",target:"node --check conversation.js",ok:true});
      renderStory({kind:"tool",agent:"phoenix",tool:"composio_search",target:"Find the connected mailbox",ok:true,detail:"Connected app result"});
      renderStory({kind:"context_compaction",agent:"phoenix",status:"started",before_tokens:254000,after_tokens:0,folded_messages:0,limit:256000});
      const compactionRow=$("conversationFeed").querySelector(".context-compaction.running.activity-current"),compactionRunsInWave=Boolean(compactionRow&&compactionRow.closest(".work-tools")&&compactionRow.textContent.includes("Context compacting")&&activityCursors().length===1);
      renderStory({kind:"context_compaction",agent:"phoenix",status:"completed",before_tokens:254000,after_tokens:92000,folded_messages:48,limit:256000});
      const compactionReceiptPersists=Boolean(compactionRow&&!compactionRow.classList.contains("running")&&compactionRow.textContent.includes("Context compacted")&&compactionRow.textContent.includes("254K → 92K"));
      showInspectionSidebar("changes");await nextTask(380);const reviewRect=$("inspectionSidebar").getBoundingClientRect(),stageRect=$("conversationStage").getBoundingClientRect(),reviewPaneIsRightSidebar=Boolean(reviewRect.width>=360&&reviewRect.width<=980&&Math.abs(reviewRect.right-innerWidth)<2&&stageRect.right<=reviewRect.left+1);hideInspectionSidebar();
      const realWebMarks=Boolean($("conversationFeed").querySelector('.work-tool[data-tool="web_search"] .source-avatar-stack img')&&$("conversationFeed").querySelector('.work-tool[data-tool="browser_navigate"] .tool-brand-image'));
      const previousConversationView=document.documentElement.dataset.conversationView;
      document.documentElement.dataset.conversationView="detailed";syncConversationDetail();
      const detailedToolDisclosure=Boolean($("conversationFeed").querySelector(".trace-connected:not([open])")&&$("conversationFeed").querySelector(".trace-coding:not([open])")&&[...$("conversationFeed").querySelectorAll(".work-tool:has(.work-tool-detail)")].every((row)=>row.classList.contains("open")&&!row.querySelector(".work-tool-detail")?.hidden));
      if(previousConversationView)document.documentElement.dataset.conversationView=previousConversationView;else delete document.documentElement.dataset.conversationView;
      syncConversationDetail();
      const volumeHandoffCountBefore=$("conversationFeed").querySelectorAll(".handoff-chain").length,volumeDisplayCountBefore=state.displayRows.length;
      renderStory({kind:"handoff",from:"orchestrator",to:"Worker (volume_worker) #1",receiver:"Worker (volume_worker) #1",subject:"anonymous batch item",status:"queued"});
      const ephemeralVolumeHandoffHidden=$("conversationFeed").querySelectorAll(".handoff-chain").length===volumeHandoffCountBefore&&state.displayRows.length===volumeDisplayCountBefore;
      const foregroundExcludedFromBackgroundSummary=summaryBackgroundProcesses().length===0;
      renderStory({kind:"handoff",handoff_id:"message-draft-a",requester:"phoenix",receiver:"scribe",from:"phoenix",to:"scribe",subject:"Draft the concise copy",status:"queued"});
      renderStory({kind:"handoff",handoff_id:"message-check-ui",requester:"phoenix",receiver:"frontend",from:"phoenix",to:"frontend",subject:"Check the interaction",status:"queued"});
      const handoffChains=[...$("conversationFeed").querySelectorAll(".handoff-chain")],scribeHandoff=handoffChains.find((row)=>row.dataset.handoffTo==="scribe"),frontendHandoff=handoffChains.find((row)=>row.dataset.handoffTo==="frontend"),handoffCount=handoffChains.length;
      renderStory({kind:"handoff",handoff_id:"message-draft-a",requester:"phoenix",receiver:"scribe",from:"phoenix",to:"scribe",subject:"Draft the concise copy",status:"working"});
      const handoffReplayUpserted=$("conversationFeed").querySelectorAll(".handoff-chain").length===handoffCount;
      const directPeerCountBeforeReturns=$("conversationFeed").querySelectorAll(".peer-message").length;
      renderStory({kind:"handoff",handoff_id:"message-draft-b",requester:"phoenix",receiver:"scribe",from:"phoenix",to:"scribe",subject:"Draft the concise copy",status:"queued"});
      const secondDraftHandoff=[...$("conversationFeed").querySelectorAll('.handoff-chain[data-handoff-to="scribe"]')].at(-1);
      renderStory({kind:"return",handoff_id:"return-draft-b",reply_to:"message-draft-b",receiver:"scribe",agent:"scribe",ok:true,subject:"Draft the concise copy",body:"Second draft is ready."});
      const outOfOrderReturnMatchedExact=secondDraftHandoff?.dataset.handoffState==="done"&&scribeHandoff?.dataset.handoffState==="working";
      renderStory({kind:"return",handoff_id:"return-draft-a",reply_to:"message-draft-a",receiver:"scribe",agent:"scribe",ok:true,subject:"Draft the concise copy",body:"Draft is ready to use."});
      renderStory({kind:"handoff",handoff_id:"message-review",requester:"phoenix",receiver:"scribe",from:"phoenix",to:"scribe",subject:"Review the final copy",status:"queued"});
      const reviewHandoff=[...$("conversationFeed").querySelectorAll('.handoff-chain[data-handoff-to="scribe"]')].at(-1);
      const lateReturnMatchedExact=scribeHandoff?.dataset.handoffState==="done"&&reviewHandoff?.dataset.handoffState==="queued";
      renderStory({kind:"return",handoff_id:"return-review",reply_to:"message-review",receiver:"scribe",agent:"scribe",ok:false,subject:"Review the final copy",body:"The source copy is missing."});
      const directReturnStaysInTrace=$("conversationFeed").querySelectorAll(".peer-message").length===directPeerCountBeforeReturns;
      const compactDelegation=Boolean(handoffReplayUpserted&&outOfOrderReturnMatchedExact&&lateReturnMatchedExact&&scribeHandoff?.querySelector(".handoff-requester")?.textContent.includes("Phoenix")&&scribeHandoff?.querySelector(".handoff-receiver")?.textContent.includes("Nico")&&scribeHandoff?.querySelector(".handoff-task")?.textContent==="Draft the concise copy"&&scribeHandoff?.querySelector(".handoff-result")?.textContent==="Draft is ready to use."&&frontendHandoff?.dataset.handoffState==="queued"&&reviewHandoff?.dataset.handoffState==="blocked"&&reviewHandoff?.querySelector(".handoff-result")?.textContent.includes("source copy is missing"));
      renderStory({kind:"handoff",handoff_id:"message-live-journal",from:"phoenix",to:"scribe",receiver:"scribe",subject:"Return while the owner is working",status:"working"});const savedTurnSocket=state.turnSocket;state.turnSocket={close(){}};const liveReturn={kind:"return",handoff_id:"return-live-journal",reply_to:"message-live-journal",receiver:"scribe",agent:"scribe",ok:true,subject:"Return while the owner is working",body:"Finished in the background."};if(journalStoryDisposition(liveReturn)==="render")renderStory(liveReturn);const liveJournalHandoff=[...$("conversationFeed").querySelectorAll(".handoff-chain")].find((row)=>row.textContent.includes("Return while the owner is working")),liveSubagentSettlesWithoutReentry=Boolean(liveJournalHandoff?.dataset.handoffState==="done"&&liveJournalHandoff.textContent.includes("Finished in the background.")),groupBrowserReceiptTargetsCoworker=journalStoryDisposition({kind:"tool",agent:"Theo (researcher)",tool:"browser_navigate",ok:true})==="browser"&&browserStoryOwner({agent:"Theo (researcher)"})==="researcher";state.turnSocket=savedTurnSocket;
      const handoffsBeforeContextMessage=$('conversationFeed').querySelectorAll('.handoff-chain').length;
      renderHistory({role:"talk",from:"phoenix",to:"critic",subject:"Use the corrected source",text:"<!-- phoenix-message-priority:high -->\nThe second source supersedes the first.",status:"priority_message_queued",handoff_id:"context-source-correction"});
      const contextMessageRow=[...$('conversationFeed').querySelectorAll('.agent-context-message')].at(-1);
      const contextMessageDistinctFromHandoff=Boolean(contextMessageRow&&$('conversationFeed').querySelectorAll('.handoff-chain').length===handoffsBeforeContextMessage&&contextMessageRow.textContent.includes('Use the corrected source')&&contextMessageRow.textContent.includes('Message · high')&&!contextMessageRow.textContent.includes('phoenix-message-priority')&&(reviewHandoff.compareDocumentPosition(contextMessageRow)&Node.DOCUMENT_POSITION_FOLLOWING));
      mark("group-fixture");const priorItem=state.item;state.item={kind:"group",id:"acceptance-build-group"};state.activeGroupAgentIds=["researcher"];const phoenixGroupWorkBefore=$("conversationFeed").querySelectorAll('.group-work-cluster[data-agent="phoenix"]').length;
      renderStory({kind:"commentary",agent:"phoenix",text:"**Inspecting the shared build evidence**\nTheo is checking the current result."});
      const liveTheo=[...$("conversationFeed").querySelectorAll('.group-work-cluster[data-agent="researcher"]')].at(-1),theoHeader=liveTheo?.querySelector(".group-work-agent"),theoAccent=ui.profileColor(knownAgentProfile("researcher"));
      const groupThinkingCubeColored=Boolean(liveTheo?.classList.contains("live")&&!theoHeader?.hidden&&theoHeader?.textContent.includes("Theo")&&theoHeader?.textContent.includes("Thinking")&&liveTheo.querySelector(".reasoning-subgroup.running.activity-current .pixel-loader")&&liveTheo.style.getPropertyValue("--agent-accent")===theoAccent);
      const groupCoordinatorTraceUsesSelectedCoworker=Boolean(liveTheo&&$("conversationFeed").querySelectorAll('.group-work-cluster[data-agent="phoenix"]').length===phoenixGroupWorkBefore);
      theoHeader?.click();const groupThinkingFolds=Boolean(liveTheo&&!liveTheo.classList.contains("group-work-expanded")&&theoHeader.getAttribute("aria-expanded")==="false");theoHeader?.click();
      renderStory({kind:"handoff",handoff_id:"message-figma",requester:"researcher",receiver:"scribe",from:"Theo",to:"Nico",subject:"Draft the Figma report",status:"queued"});
      const groupCheckpoint=[...$("conversationFeed").querySelectorAll("[data-handoff-checkpoint]")].at(-1);
      renderStory({kind:"return",handoff_id:"message-figma",reply_to:"message-figma",requester:"researcher",receiver:"scribe",agent:"scribe",ok:true,subject:"Figma report",body:"The verified report is ready."});
      const groupPeer=[...$("conversationFeed").querySelectorAll(".peer-message")].at(-1);
      renderStory({kind:"commentary",agent:"researcher",text:"Integrating Nico’s verified report"});
      const resumedTheo=[...$("conversationFeed").querySelectorAll('.work-cluster[data-agent="researcher"]')].at(-1);
      const groupHandoffSequence=Boolean(groupCheckpoint?.textContent.includes("I’ve asked Nico")&&groupPeer?.textContent.includes("From Nico")&&groupPeer.textContent.includes("verified report is ready")&&groupCheckpoint.compareDocumentPosition(groupPeer)&Node.DOCUMENT_POSITION_FOLLOWING&&groupPeer.compareDocumentPosition(resumedTheo)&Node.DOCUMENT_POSITION_FOLLOWING);
      const priorGroupFixtureRows=state.displayRows;state.displayRows=[{source:"history",turn_id:state.renderingTurnId||state.activeTurnId,value:{role:"user",text:"Theo, inspect the proof.",initiating_agent_id:"researcher"}}];
      renderStory({kind:"group_message",message_id:"group-markdown-proof",group_id:"acceptance-build-group",round:1,agent_id:"researcher",agent_name:"Theo",markdown:"## Verified Markdown\n\n> Work runs in parallel.\n\n- **Bold item**\n- `code item`\n\n| Check | Result | Evidence |\n|:---|:---:|---:|\n| Browser auth | **PASS** | `2 workers` |\n| Literal pipe | PASS | `alpha \\| beta` |\n\n[Research report](artifacts/research/report.md)"});
      const markdownGroup=[...$("conversationFeed").querySelectorAll('.group-message[data-message-id="group-markdown-proof"]')].at(-1),markdownTable=markdownGroup?.querySelector(".markdown table"),liveGroupMarkdownParsed=Boolean(markdownGroup?.querySelector(".markdown h2")&&markdownGroup.querySelector(".markdown blockquote")&&markdownGroup.querySelectorAll(".markdown li").length===2&&markdownGroup.querySelector(".markdown code")&&markdownGroup.querySelector('[data-open-local-path="artifacts/research/report.md"]')&&markdownTable?.querySelectorAll("thead th").length===3&&markdownTable.querySelectorAll("tbody tr").length===2&&markdownTable.querySelector("tbody strong")?.textContent==="PASS"&&markdownTable.textContent.includes("alpha | beta")&&!markdownGroup.textContent.includes("|:---"));
      state.displayRows=priorGroupFixtureRows;
      const migratedGroupRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[
        {source:"history",turn_id:"turn-group-proof",value:{role:"user",text:"Theo inspect the proof",turn_id:"turn-group-proof"}},
        {source:"story",turn_id:"turn-group-proof",value:{kind:"commentary",agent:"Theo (researcher)",text:"Inspecting the proof"}},
        {source:"story",turn_id:"turn-group-proof",value:{kind:"tool",agent:"Theo (researcher)",tool:"read",target:"proof.md",ok:true,detail:"read"}},
        {source:"story",turn_id:"turn-group-proof",value:{kind:"group_message",message_id:"group-message-proof",group_id:"acceptance-build-group",agent_id:"researcher",agent_name:"Theo",markdown:"## Result  - flat"}},
        {source:"history",turn_id:"legacy-99-proof",value:{role:"user",text:"@researcher inspect the proof"}},
        {source:"story",turn_id:"legacy-99-proof",value:{kind:"commentary",agent:"Theo (researcher)",text:"Inspecting the proof"}},
        {source:"story",turn_id:"legacy-99-proof",value:{kind:"tool",agent:"Theo (researcher)",tool:"read",target:"proof.md",ok:true,detail:"read"}},
        {source:"story",turn_id:"legacy-99-proof",value:{kind:"group_message",message_id:"group-message-proof",group_id:"acceptance-build-group",agent_id:"researcher",agent_name:"Theo",markdown:"## Result  - flat"}},
        {source:"history",turn_id:"legacy-99-proof",value:{role:"group_message",turn_id:"turn-group-proof",message_id:"group-message-proof",group_id:"acceptance-build-group",agent_id:"researcher",agent_name:"Theo",markdown:"## Result\n\n- **Preserved**"}},
      ]})}),groupReplayKeepsPromptToolsAndMarkdown=Boolean(migratedGroupRows.length===4&&migratedGroupRows.map(displayRole).join("|")==="user|commentary|tool|group_message"&&migratedGroupRows.at(-1).turn_id==="turn-group-proof"&&migratedGroupRows.at(-1).value.markdown.includes("\n\n- **Preserved**"));
      const lateGroupWork=repairRecoveredRowOrder([
        {source:"history",turn_id:"turn-group-late",value:{role:"user",text:"Theo verify it"}},
        {source:"story",turn_id:"turn-group-late",value:{kind:"group_message",agent_id:"researcher",agent_name:"Theo",markdown:"Verified."}},
        {source:"story",turn_id:"turn-group-late",value:{kind:"commentary",agent:"Theo (researcher)",text:"Checking the proof"}},
        {source:"story",turn_id:"turn-group-late",value:{kind:"tool",agent:"Theo (researcher)",tool:"read",target:"proof.md",ok:true}},
      ]),groupRecoveredWorkPrecedesItsAgentAnswer=lateGroupWork.map(displayRole).join("|")==="user|commentary|tool|group_message";
      state.item=priorItem;state.activeGroupAgentIds=[];mark("group-done");
      const imageProof={name:"phoenix_logo.png",path:"/proof/phoenix_logo.png",type:"image/png"},cleanImagePrompt="Yay that worked! What comment did I give to you?",migratedImageRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[
        {source:"history",turn_id:"turn-image-proof",value:{role:"user",turn_id:"turn-image-proof",text:cleanImagePrompt,attachments:[imageProof]}},
        {source:"story",turn_id:"turn-image-proof",value:{kind:"commentary",agent:"phoenix",text:"Inspecting the image"}},
        {source:"story",turn_id:"turn-image-proof",value:{kind:"answer",agent:"phoenix",text:"I like phoenix."}},
        {source:"history",turn_id:"legacy-image-proof",value:{role:"user",text:`${cleanImagePrompt}\n\nImage comments:\n**phoenix_logo.png**\n1. 35% from the left, 19% from the top — I like phoenix.`,attachments:[imageProof]}},
        {source:"history",turn_id:"legacy-image-proof",value:{role:"answer",text:"I like phoenix.",agent:"phoenix"}},
      ]})}),imageCommentTurnDeduped=Boolean(migratedImageRows.map(displayRole).join("|")==="user|commentary|answer"&&migratedImageRows.every((entry)=>entry.turn_id==="turn-image-proof")&&migratedImageRows[0].value.text===cleanImagePrompt&&migratedImageRows[0].value.attachments?.length===1);
      const previousPlanExpanded=state.taskPlanExpanded;state.taskPlanExpanded=false;
      state.tasks=[{task:"Inspect the current state",completed:true},{task:"Apply the verified change",completed:false},{task:"Confirm the outcome",completed:false}];
      renderTasks();
      const taskStayedGloballyCollapsed=!$("taskBlock").hidden&&!$("taskBlock").open&&$("taskList").querySelectorAll(".task-item.in-progress").length===1;
      state.taskPlanExpanded=true;renderTasks();const taskExpandedGlobally=$("taskBlock").open;
      state.tasks=state.tasks.map((item)=>({...item,completed:true}));renderTasks();
      const taskPreferenceSurvivedCompletion=$("taskBlock").open&&$("taskBlock").classList.contains("complete")&&$("taskProgress").textContent==="3/3";state.taskPlanExpanded=previousPlanExpanded;
      setWorking(false);
      const staleQueuedHandoffsExcluded=!summaryHandoffs().some((row)=>row.dataset.handoffState==="queued");
      const faithfulAnswer='I left `session_id=release-session-12345678` and `123e4567-e89b-12d3-a456-426614174000` unchanged.';
      appendDisplay("history",{role:"answer",text:faithfulAnswer,agent:"phoenix"},true);renderAnswer(faithfulAnswer,"phoenix",{created_at:new Date().toISOString(),elapsed_ms:31000});
      const runningBeforeReplay=$("conversationFeed").querySelectorAll(".work-tool.running").length;
      renderStory({kind:"tool_start",agent:"phoenix",tool:"work",target:'{"action":"inspect"}'},true);
      const replayStartIgnored=$("conversationFeed").querySelectorAll(".work-tool.running").length===runningBeforeReplay;
      const receiptFixture=document.createElement("div");
      const receipt={kind:"tool",agent:"phoenix",tool:"work",target:'{"action":"inspect"}',ok:true,detail:"company work status accepted"};
      appendCompletedToolRow(receiptFixture,receipt,"Checked company work");
      appendCompletedToolRow(receiptFixture,receipt,"Checked company work");
      const repeatedReceiptFolded=receiptFixture.querySelectorAll(".work-tool").length===1&&receiptFixture.querySelector(".work-tool-count")?.textContent==="×2";
      const recurringRoutineText="[cron abc12345 | scheduled daily 09:00] Check the account status. Do not mutate anything.",routineOrigin={kind:"routine",routine_id:"abc12345",scheduled_for:"2026-08-26T15:00:00.000Z",schedule:"daily 09:00"};
      renderStory({kind:"user",text:recurringRoutineText,turn_id:"routine:abc12345:one",origin:routineOrigin});
      // Same occurrence delivered through replay is one row; tomorrow's
      // byte-identical prompt is a distinct authored Routine boundary.
      renderStory({kind:"user",text:recurringRoutineText,turn_id:"routine:abc12345:one",origin:routineOrigin},true);
      renderStory({kind:"user",text:recurringRoutineText,turn_id:"routine:abc12345:two",origin:{...routineOrigin,scheduled_for:"2026-08-27T15:00:00.000Z"}},true);
      renderStory({kind:"steer",from:"phoenix",to:"scribe",subject:"internal route",body:"This plumbing must stay hidden."});
      renderHistory({role:"tool",agent:"orchestrator",tool:"__phoenix_group_user_boundary",target:"turn_internal_secret",ok:true,detail:""});
      renderApproval({kind:"ask_pending",id:"acceptance-question",agent:"phoenix",questions:[{header:"Launch size",question:"How many options should we launch with?",options:["Three focused options","Five complete options","One hero option"],multi_select:false},{header:"Channels",question:"Which channels should we include?",options:["Email","Website","Social"],multi_select:true}]});
      renderApproval({kind:"ask_pending",id:"acceptance-login",agent:"scribe",questions:[{header:"Sign in",question:"scribe needs this website login to continue. How should Phoenix proceed?",options:["Import from my browser","I'll log in","Create an account"],multi_select:false}],approval:{action:"login_request",details:{site:"example.com",agent_id:"scribe",credential_id:"credential-secret-12345678",scope:"agent"}}});
      renderApproval({kind:"ask_pending",id:"acceptance-vault",agent:"phoenix",questions:[{header:"Unlock vault",question:"Unlock the local vault here so Phoenix can use the saved account.",options:["Unlock here","Not now"],multi_select:false}],approval:{action:"vault_unlock",details:{reason:"Continue the approved sign-in",ask_id:"ask-secret-12345678"}}});
      renderApproval({kind:"ask_pending",id:"acceptance-permission",agent:"school_coach",questions:[{header:"Permission",question:"Avery (school_coach) wants to use `browser_state` for {}. Allow Full Access for this call?",options:["Allow once","Keep current access"],multi_select:false}],approval:{action:"tool_permission",subject:"browser_state",approved_option:"Allow once",details:{tool_name:"browser_state",current_mode:"workspace",required_mode:"full_access",scope:"single_call"}}});
      state.mentions=["scribe"];renderMentionTray();
      await nextTask(80);
      const stoppedPrompt=renderUser("Stop this test turn after showing its reasoning.");
      setWorking(true);beginTurnActivity(stoppedPrompt);
      renderCommentary("phoenix","This reasoning must remain visible after the turn is stopped.");
      setWorking(false,true);
      const stoppedCluster=[...$("conversationFeed").querySelectorAll(".work-cluster")].at(-1);
      const feed=$("conversationFeed"),text=feed.textContent,cluster=feed.querySelector(".work-cluster"),visibleApproval=$("approvalStack").lastElementChild,fade=$("composerFade"),answerText=[...feed.querySelectorAll(".agent-message .markdown")].at(-1)?.textContent||"";
      let decisionAlternativeFlow=false;
      if(visibleApproval?.classList.contains("approval-decision-card")){
        visibleApproval.querySelector("[data-ask-alternatives]")?.click();
        const opened=!visibleApproval.querySelector(".approval-alternatives")?.hidden;
        visibleApproval.querySelector('[data-ask-decision-select="1"]')?.click();
        decisionAlternativeFlow=Boolean(opened&&visibleApproval.querySelector("[data-ask-decision-confirm]")?.textContent==="Keep current access"&&visibleApproval.querySelector(".approval-alternatives")?.hidden);
        visibleApproval._askState.decisionSelection=0;renderApprovalQuestion(visibleApproval);
      }
      // Settled turn: the agent's own messages stay as conversation, the live
      // thinking cube is gone, and no reasoning text was ever rendered.
      const settledUpdate=cluster.querySelector(".agent-update");
      const settledHeaderVisible=Boolean(settledUpdate&&visible(settledUpdate)&&settledUpdate.textContent.includes("The account form is ready"));
      const settledAnimationStopped=!cluster.querySelector(".reasoning-subgroup.running,.reasoning-subgroup.pending-activity");
      const workedInitiallyCollapsed=!cluster.querySelector(".work-tools")?.hidden;
      const workedExpandsInPlace=!text.includes("Private planning detail")&&!text.includes("Legacy provider summary");
      const traceCategories=[...cluster.querySelectorAll(".trace-subgroup")].map((group)=>group.dataset.traceCategory);
      const structuredTrace=traceCategories.includes("search")&&traceCategories.includes("browser")&&traceCategories.includes("coding")&&cluster.querySelector('.trace-browser .tool-brand-image')&&cluster.querySelector('.trace-coding:not([open])');
      const oneTurnTrace=Boolean(cluster.querySelector(".ask-history-row")&&cluster.querySelector('.handoff-chain[data-handoff-from="phoenix"][data-handoff-to="scribe"]')&&cluster.querySelectorAll(".work-tool").length>=8&&[...cluster.querySelectorAll(".handoff-receiver .agent-metal-chip strong")].some((node)=>node.textContent==="Nico"));
      const answerFooterBeforeRestore=Boolean(feed.querySelector(".answer-footer [data-copy-answer]")&&feed.querySelector(".answer-footer .completion-meta"));
      const stoppedReasoningBeforeRestore=Boolean(stoppedCluster&&getComputedStyle(stoppedCluster).display!=="none"&&!stoppedCluster.classList.contains("live")&&!stoppedCluster.querySelector(".work-tools")?.hidden&&stoppedCluster.querySelector(".reasoning-subgroup:not([open])")&&stoppedCluster.textContent.includes("must remain visible"));
      // Repaint the durable journal exactly as a real conversation switch does.
      // This catches the former self-deduplication bug that silently erased all
      // reasoning and completed tool rows whenever the user came back.
      const persisted=state.displayRows.map((entry)=>cloneDisplayValue(entry)).filter(Boolean);
      clearFeed();
      paintFeed(()=>persisted.forEach(renderDisplayEntry));
      foldRestoredWork();
      const restoredFeed=$("conversationFeed"),restoredText=restoredFeed.textContent;
      const restoredTrace=restoredFeed.querySelector(".work-cluster.settled");
      const conversationSwitchPreservesTrace=Boolean(restoredTrace&&restoredText.includes("The account form is ready")&&restoredText.includes("no visible element matching Continue with email")&&restoredFeed.querySelectorAll(".work-tool").length>=3&&!restoredFeed.querySelector(".work-tool.running"));
      const permanentTurnControls=Boolean(restoredFeed.querySelector(".user-message [data-delete-prompt]")&&restoredFeed.querySelector(".work-cluster [data-delete-agent-turn],.agent-message [data-delete-agent-turn]")&&[...restoredFeed.querySelectorAll("[data-delete-prompt],[data-delete-agent-turn]")].every((button)=>button.closest("[data-turn-id]")));
      const visibleLogin=document.querySelector('[data-ask-id="acceptance-login"]'),cleanLoginOwner=Boolean(visibleLogin?.textContent.includes("How should Nico sign in")&&!visibleLogin.textContent.includes("scribe"));
      const displayCountBeforeProbe=state.displayRows.length,dedupeProbe={kind:"answer",agent:"school_coach",markdown:"One transport-owned answer"};
      const firstTransportAnswer=appendDisplay("story",dedupeProbe),secondTransportAnswer=appendDisplay("story",dedupeProbe);
      const transportDuplicateSuppressed=firstTransportAnswer&&!secondTransportAnswer&&state.displayRows.length===displayCountBeforeProbe+1;
      state.displayRows.pop();
      const interleavedStart=state.displayRows.length,interleavedAnswer={kind:"answer",agent:"school_coach",markdown:"One interleaved final"};appendDisplay("story",interleavedAnswer);appendDisplay("story",{kind:"settled",agent:"school_coach",ok:true});const interleavedDuplicateSuppressed=!appendDisplay("story",interleavedAnswer)&&state.displayRows.length===interleavedStart+2;state.displayRows.splice(interleavedStart);
      const legacyCompletionRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[{source:"story",value:{kind:"cross_answer",session_id:"agent-school_coach",project_name:"Avery",summary:"done"}},{source:"story",value:dedupeProbe},{source:"story",value:dedupeProbe}]})});
      const legacyCompletionNoisePruned=legacyCompletionRows.length===1&&legacyCompletionRows[0].value?.kind==="answer";
      state.displayMigrationDirty=false;
      const legacyVolumeRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[
        {source:"history",turn_id:"turn-volume",value:{role:"user",text:"Read these files"}},
        {source:"story",turn_id:"turn-volume",value:{kind:"handoff",from:"orchestrator",to:"Worker (volume_worker) #1",receiver:"Worker (volume_worker) #1",subject:"item-1",status:"queued"}},
        {source:"story",turn_id:"turn-volume",value:{kind:"notice",text:"Volume batch finished."}},
      ]})});
      const legacyVolumeHandoffsMigrated=state.displayMigrationDirty&&legacyVolumeRows.length===2&&!legacyVolumeRows.some((entry)=>displayRole(entry)==="handoff");
      const routineTurnId="routine:54c78125:fixture",routineMirrorRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[
        {source:"history",turn_id:routineTurnId,value:{role:"user",text:"Scheduled work",turn_id:routineTurnId,origin:{kind:"routine"}}},
        {source:"history",turn_id:routineTurnId,value:{role:"tool",tool:"composio_search",target:'{"queries":[{"use_case":"scan both connected Gmail accounts for school messages received after today"}]}',ok:true,detail:""}},
        {source:"history",turn_id:routineTurnId,value:{role:"answer",text:"The scheduled review is complete."}},
        {source:"story",turn_id:routineTurnId,value:{kind:"tool",agent:"school_coach",tool:"composio_search",target:'{"queries":[{"use_case":"scan both connected Gmail accounts for school messages...',ok:true,detail:"composio search ok"}},
        {source:"story",turn_id:routineTurnId,value:{kind:"answer",agent:"school_coach",markdown:"**[school_coach]** The scheduled review is complete."}},
      ]})});
      const routineMirrorDeduped=routineMirrorRows.filter((entry)=>displayRole(entry)==="user").length===1&&routineMirrorRows.filter((entry)=>displayRole(entry)==="tool").length===1&&routineMirrorRows.filter((entry)=>displayRole(entry)==="answer").length===1;
      const handoffTurnId="turn:handoff-order",handoffRecoveryRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[
        {source:"history",turn_id:handoffTurnId,value:{role:"user",text:"Review the repository"}},
        {source:"story",turn_id:handoffTurnId,value:{kind:"handoff",from:"orchestrator",to:"critic",subject:"Check the conclusion"}},
        {source:"story",turn_id:handoffTurnId,value:{kind:"answer",markdown:"The review is complete."}},
        {source:"history",turn_id:"talk-99",value:{role:"talk",from:"orchestrator",to:"critic",subject:"Check the conclusion",text:"Check the conclusion",turn_id:"talk-99"}},
      ]})});
      const recoveredHandoffBeforeAnswer=handoffRecoveryRows.filter((entry)=>["talk","handoff"].includes(displayRole(entry))).length===1&&handoffRecoveryRows.findIndex((entry)=>["talk","handoff"].includes(displayRole(entry)))<handoffRecoveryRows.findIndex((entry)=>displayRole(entry)==="answer");
      const returnTurnId="turn:return-order",correlatedReturnRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[
        {source:"history",turn_id:returnTurnId,value:{role:"user",text:"Prepare the corrected packs"}},
        {source:"story",turn_id:returnTurnId,value:{kind:"handoff",handoff_id:"message-pack",from:"phoenix",to:"scribe",receiver:"scribe",subject:"Draft corrected packs",status:"working"}},
        {source:"story",turn_id:returnTurnId,value:{kind:"answer",agent:"phoenix",markdown:"The corrected packs are ready."}},
        {source:"history",turn_id:"talk-197",value:{role:"talk",from:"scribe",to:"phoenix",handoff_id:"message-pack",reply_to:"message-pack",status:"done",subject:"Corrected packs",text:"Both corrected packs are complete."}},
      ]})}),correlatedReturnIndex=correlatedReturnRows.findIndex(isHandoffReturnEntry),correlatedAnswerIndex=correlatedReturnRows.findIndex((entry)=>displayRole(entry)==="answer"),coworkerReturnPrecedesOwnerFinal=Boolean(correlatedReturnIndex>0&&correlatedReturnIndex<correlatedAnswerIndex&&correlatedReturnRows[correlatedReturnIndex].turn_id===returnTurnId&&!isAuthoredBoundaryEntry(correlatedReturnRows[correlatedReturnIndex]));
      state.displayMigrationDirty=false;
      const staleAskRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[
        {source:"history",turn_id:"turn:current",value:{role:"user",text:"Finish the current review"}},
        {source:"story",turn_id:"turn:current",value:{kind:"answer",agent:"school_coach",markdown:"The current review is finished."}},
        {source:"story",turn_id:"turn:current",value:{kind:"ask_pending",id:"ask-days-old",agent:"school_coach",status:"answered_late",answer:"Continue",created_at:"2026-09-01T00:06:00Z",resolved_at:"2026-09-01T00:07:00Z",questions:[{header:"Old question",question:"Already answered?",options:["Continue"]}]}}
      ]})});
      const staleResolvedAskMigrated=state.displayMigrationDirty&&!staleAskRows.some((entry)=>entry.value?.id==="ask-days-old");
      const savedAskState={rows:state.displayRows,bytes:state.displayBytes,dirty:state.displayDirty,activeTurnId:state.activeTurnId};
      replaceDisplayRows([{source:"history",turn_id:"turn:api",value:{role:"user",text:"Current request"}},{source:"story",turn_id:"turn:api",value:{kind:"answer",markdown:"Current answer"}}],false);state.activeTurnId="turn:api";
      reconcileAsks([{ask_id:"ask-resolved-absent",agent:"school_coach",status:"dismissed",answer:"Not now",created_at:"2026-08-31T23:37:00Z",resolved_at:"2026-08-31T23:53:00Z",questions:[{header:"Login",question:"Old login?",options:["Not now"]}]},{ask_id:"ask-pending-current",agent:"school_coach",status:"pending",created_at:new Date().toISOString(),questions:[{header:"Current",question:"Still needs input?",options:["Continue"]}]}]);
      const resolvedAskHistoryDoesNotResurrect=!state.displayRows.some((entry)=>entry.value?.id==="ask-resolved-absent")&&state.displayRows.some((entry)=>entry.value?.id==="ask-pending-current");
      replaceDisplayRows(savedAskState.rows,savedAskState.dirty);state.displayBytes=savedAskState.bytes;state.activeTurnId=savedAskState.activeTurnId;
      const recoveredOrder=repairRecoveredRowOrder([
        {source:"history",turn_id:routineTurnId,value:{role:"user",text:"Scheduled work"}},
        {source:"story",turn_id:routineTurnId,value:{kind:"answer",markdown:"Finished."}},
        {source:"story",turn_id:routineTurnId,value:{kind:"tool",tool:"todo_write",target:"8/10",ok:true}},
        {source:"story",turn_id:routineTurnId,value:{kind:"receipt",text:"1 other action"}},
        {source:"story",turn_id:routineTurnId,value:{kind:"settled",agent:"school_coach",ok:true}},
      ]);
      const recoveredWorkPrecedesAnswer=recoveredOrder.findIndex((entry)=>displayRole(entry)==="tool")<recoveredOrder.findIndex((entry)=>displayRole(entry)==="answer")&&recoveredOrder.findIndex((entry)=>displayRole(entry)==="receipt")<recoveredOrder.findIndex((entry)=>displayRole(entry)==="answer")&&recoveredOrder.findIndex((entry)=>displayRole(entry)==="settled")>recoveredOrder.findIndex((entry)=>displayRole(entry)==="answer");
      const catchUpUserIdentityStable=displaySemantic({source:"history",value:{role:"user",text:"Same authored prompt"},turn_id:"turn-live"})===displaySemantic({source:"history",value:{role:"user",text:"Same authored prompt"},turn_id:""});
      const legacyStormRows=[
        {source:"history",turn_id:"legacy-0-answer:a1",value:{role:"user",text:"Alpha prompt"}},
        {source:"history",turn_id:"legacy-0-answer:a1",value:{role:"answer",text:"Alpha answer"}},
        {source:"history",turn_id:"legacy-2-answer:b1",value:{role:"user",text:"Beta prompt"}},
        {source:"history",turn_id:"legacy-2-answer:b1",value:{role:"answer",text:"Beta answer"}},
        {source:"history",turn_id:"legacy-4-answer:a1",value:{role:"user",text:"Alpha prompt"}},
        {source:"history",turn_id:"legacy-4-answer:a1",value:{role:"narration",text:"Alpha resumed work"}},
        {source:"history",turn_id:"legacy-6-answer:b1",value:{role:"user",text:"Beta prompt"}},
        {source:"history",turn_id:"legacy-7-answer:a1",value:{role:"user",text:"Alpha prompt"}},
        {source:"history",turn_id:"legacy-8-answer:b1",value:{role:"user",text:"Beta prompt"}},
      ];
      const repairedStorm=repairRepeatedCatchUpTurns(legacyStormRows,[{role:"user",text:"Alpha prompt"},{role:"user",text:"Beta prompt"}]);
      const legacyCatchUpStormRepaired=Boolean(repairedStorm.changed&&repairedStorm.rows.filter((entry)=>displayRole(entry)==="user").length===2&&repairedStorm.rows.filter((entry)=>displayRole(entry)==="answer").length===2&&repairedStorm.rows.some((entry)=>entry.value?.text==="Alpha resumed work")&&new Set(repairedStorm.rows.map((entry)=>entry.turn_id)).size===2);
      state.usageBySession.set("acceptance-agent-a",{used:111,limit:1000});state.usageBySession.set("acceptance-agent-b",{used:777,limit:2000});persistContextUsage("acceptance-agent-a","phoenix",{used:111,limit:1000});persistContextUsage("acceptance-agent-b","frontend",{used:777,limit:2000});
      const persistedContextA=storedContextUsage("acceptance-agent-a","phoenix"),persistedContextB=storedContextUsage("acceptance-agent-b","frontend"),contextCountersArePerSession=state.usageBySession.get("acceptance-agent-a").used===111&&state.usageBySession.get("acceptance-agent-b").used===777&&persistedContextA?.used===111&&persistedContextB?.used===777;localStorage.removeItem(contextUsageStorageKey("acceptance-agent-a","phoenix"));localStorage.removeItem(contextUsageStorageKey("acceptance-agent-b","frontend"));
      const expiredAuthCard=turnFailureCard({message:"Stored OAuth token is expired"},"frontend"),expiredAuthFailureIsActionable=expiredAuthCard.kind==="card"&&expiredAuthCard.ok===false&&expiredAuthCard.subject==="Provider sign-in expired"&&expiredAuthCard.body.includes("Sign in again")&&expiredAuthCard.body.includes("routes will stay unchanged");
      const savedModels=state.models,savedSelectedModel=state.selectedModel;state.models=[{id:"sol",name:"Sol",provider:"openai-codex",provider_id:"openai-codex",provider_name:"OpenAI Codex"},{id:"opus",name:"Opus",provider:"anthropic",provider_id:"anthropic",provider_name:"Anthropic"}];state.selectedModel=state.models[0];openModels();const modelPicker=document.querySelector(".model-picker-popover"),anthropicFilter=modelPicker?.querySelector('[data-model-provider-filter="anthropic"]');anthropicFilter?.click();const visibleModelProviders=[...(modelPicker?.querySelectorAll("[data-model-provider]")||[])].filter((row)=>!row.hidden).map((row)=>row.dataset.modelProvider),providerModelFiltering=Boolean(anthropicFilter?.getAttribute("aria-selected")==="true"&&visibleModelProviders.length===1&&visibleModelProviders[0]==="anthropic"&&modelPicker.querySelector("[data-model-count]")?.textContent==="1 model");ui.closeLayers();state.models=savedModels;state.selectedModel=savedSelectedModel;renderModelLabel();
      mark("attachments");toggleAttachmentMenu();await nextTask(720);const liquidSurface=$("attachmentMenu").querySelector("svg[data-gooey-svg]");const liquidAttachmentMenu=Boolean($("attachmentMenu").classList.contains("open")&&$("attachButton").getAttribute("aria-expanded")==="true"&&$("attachPathButton").getAttribute("aria-hidden")==="false"&&liquidSurface&&$("attachmentMenu").querySelectorAll(".liquid-plus-file-item,.liquid-plus-image-item").length===2&&!$("attachmentMenu").querySelector(".liquid-plus-blob")&&getComputedStyle($("composer")).overflow==="visible");closeAttachmentMenu();
      let imagePastePrevented=false,textPastePrevented=false;const pastedFile=new File([new Uint8Array([137,80,78,71])],"",{type:"image/png"});
      mark("paste-image");await pasteComposerImages({clipboardData:{items:[{kind:"file",type:"image/png",getAsFile:()=>pastedFile}],files:[]},preventDefault:()=>{imagePastePrevented=true;}});
      const clipboardImagePaste=Boolean(imagePastePrevented&&state.attachments.length===1&&state.attachments[0].name==="Pasted image 1.png"&&$("attachmentTray").classList.contains("populated")&&$("attachmentTray").querySelector(".attachment-image img"));
      await pasteComposerImages({clipboardData:{items:[],files:[]},preventDefault:()=>{textPastePrevented=true;}});const textPasteUnaffected=!textPastePrevented;
      let imageDropPrevented=false,imageDropStopped=false;const droppedFiles=[new File([new Uint8Array([137,80,78,71])],"first.png",{type:"image/png"}),new File([new Uint8Array([255,216,255])],"second.jpg",{type:"image/jpeg"})];
      mark("drop-images");await dropComposerImages({dataTransfer:{files:droppedFiles},preventDefault:()=>{imageDropPrevented=true;},stopPropagation:()=>{imageDropStopped=true;}});const multipleExplorerImageDrop=Boolean(imageDropPrevented&&imageDropStopped&&state.attachments.length===3&&state.attachments.slice(-2).map((file)=>file.name).join(",")==="first.png,second.jpg");
      state.attachments=[];renderAttachments();
      mark("steer-submit");state.mentions=[];state.attachments=[];$("composerInput").value="This follow-up should reach the running turn";setWorking(true);syncSendMode();const draftsBeforeSteer=state.queuedDrafts.size;await submitTurn();mark("steer-submitted");
      // A one-to-one send while working is delivered into the running turn:
      // shown at once as sent (labelled), never a queue row or queued draft.
      const steeredBubble=[...$("conversationFeed").querySelectorAll(".user-message.steered-message")].at(-1);
      const steerDeliveredImmediately=Boolean(steeredBubble&&steeredBubble.textContent.includes("This follow-up should reach the running turn")&&steeredBubble.querySelector(".steered-label")&&$("queueBlock").hidden&&state.working&&state.queuedDrafts.size===draftsBeforeSteer&&state.displayRows.some((entry)=>entry.value?.steered&&entry.value?.text==="This follow-up should reach the running turn")&&!$("composerInput").value);
      // The durable queue drawer remains for recovered/internal queued work
      // (group rooms never queue a user message); exercise that retained path.
      mark("queue-submit");{const text="This follow-up should look like a regular message",turnId="turn_preview_group_followup",queueId="q-preview-group-followup";state.queue.push({queue_id:queueId,preview:text,turn_id:turnId,state:"waiting"});state.queuedDrafts.set(turnId,{queueId,turnId,requestText:text,displayText:text,initiatingAgentId:null,files:[]});renderQueue();}mark("queue-submitted");
      const queuedDraft=[...state.queuedDrafts.values()].at(-1),queuedBeforeCanonical=$("conversationFeed").querySelector(`.user-message[data-queued-id="${CSS.escape(String(queuedDraft?.queueId||""))}"]`),queueAboveTodo=$("queueBlock").compareDocumentPosition($("taskBlock"))&Node.DOCUMENT_POSITION_FOLLOWING;
      const queuedPromptLooksRegular=Boolean(queuedDraft&&!queuedBeforeCanonical&&!$("queueBlock").hidden&&$("queueList").textContent.includes("This follow-up should look like a regular message")&&queueAboveTodo);
      const queuedDrawerDoesNotDuplicate=Boolean(!queuedBeforeCanonical&&$("queueList").querySelectorAll(".queue-row").length===1);
      renderStory({kind:"user",text:queuedDraft.requestText,turn_id:queuedDraft.turnId});
      const queuedEntry=[...state.displayRows].reverse().find((entry)=>entry.value?.queued_id===queuedDraft.queueId),queuedBubble=$("conversationFeed").querySelector(`.user-message[data-queued-id="${CSS.escape(String(queuedDraft.queueId))}"]`);
      const queuedWakeShowsCube=Boolean(queuedEntry&&queuedBubble&&$("queueBlock").hidden&&state.queuedWakeTurnId===queuedEntry.turn_id&&activityCursors().length===1);
      renderStory({kind:"answer",agent:"phoenix",markdown:"This is the queued turn's answer."});
      const queuedAnswer=[...$("conversationFeed").querySelectorAll(".agent-message")].at(-1),queuedPromptBoundaryBeforeAnswer=Boolean(queuedBubble.compareDocumentPosition(queuedAnswer)&Node.DOCUMENT_POSITION_FOLLOWING)&&state.displayRows.findIndex((entry)=>entry.turn_id===queuedEntry.turn_id&&displayRole(entry)==="user")<state.displayRows.findIndex((entry)=>entry.turn_id===queuedEntry.turn_id&&displayRole(entry)==="answer");
      renderStory({kind:"settled",agent:"phoenix",ok:true});
      const detachedPromptText="This turn started from another Phoenix surface";
      appendDisplay("history",{role:"user",text:detachedPromptText},true);
      const detachedPrompt=renderUser(detachedPromptText);
      renderStory({kind:"commentary",agent:"phoenix",text:"**Researching the requested direction**"});
      const detachedTurnWakesVisibleTrace=Boolean(state.working&&state.turnStatus?.classList.contains("live")&&activityCursors().length===1&&state.turnStatus.textContent.includes("Researching the requested direction")&&detachedPrompt.compareDocumentPosition(state.turnStatus)&Node.DOCUMENT_POSITION_FOLLOWING);
      renderStory({kind:"tool_start",agent:"phoenix",tool:"web_search",target:"current interface research"});
      const detachedTurnMovesCubeToTool=Boolean(state.turnStatus?.querySelector(".trace-subgroup.running.activity-current")&&activityCursors().length===1);
      renderStory({kind:"settled",agent:"phoenix",ok:true});
      const lateAnswerOrigin={kind:"ask_answer",ask_id:"ask-acceptance",agent_id:"researcher",display:"Research Grok agent groups and compare their features."};
      renderStory({kind:"user",text:lateAnswerOrigin.display,turn_id:"queued_late_acceptance",origin:lateAnswerOrigin});
      renderStory({kind:"reasoning",agent:"researcher",text:"Researching Grok agent groups and features"});
      const lateAnswerBubble=[...$("conversationFeed").querySelectorAll(".answer-resume-message")].at(-1),lateAnswerLabelVisible=Boolean(lateAnswerBubble?.textContent.includes("Answer to Theo")&&lateAnswerBubble.textContent.includes(lateAnswerOrigin.display)&&!lateAnswerBubble.textContent.includes("late ask answer")),lateAnswerIsWorking=state.working,lateAnswerCursorCount=activityCursors().length,lateAnswerHasOneCursor=lateAnswerCursorCount===1,lateAnswerBoundaryVisible=Boolean(lateAnswerLabelVisible&&lateAnswerIsWorking&&lateAnswerHasOneCursor);
      state.queue=[{queue_id:"queued_late_acceptance",preview:lateAnswerOrigin.display,state:"running",interaction_mode:"execute",origin:lateAnswerOrigin}];renderQueue();
      const claimedQueueBecomesLiveTurn=$("queueBlock").hidden;
      const waitingLateOrigin={...lateAnswerOrigin,ask_id:"ask-waiting",display:"Compare the strongest Grok group workflow."};state.queue=[{queue_id:"queued_late_waiting",preview:waitingLateOrigin.display,state:"queued",interaction_mode:"execute",origin:waitingLateOrigin}];renderQueue();
      const waitingLateAnswerIsCleanAndRemovable=Boolean(!$("queueBlock").hidden&&$("queueList").textContent.includes(waitingLateOrigin.display)&&$("queueList").textContent.includes("Answer · queued")&&!$("queueList").textContent.includes("late ask answer")&&$("queueList").querySelector('[data-queue="cancel"]'));
      const legacyLate='[late ask answer] The user answered the earlier popup AFTER the asking turn had already ended: "Collected 1 answer(s) from user:\n\n  [Research] Q: Continue?\n  A: Research Grok groups now.". This answer supersedes any timeout assumption that turn proceeded with — act on it now: continue the work it unblocks, or report what changes because of it.';
      const migratedLateRows=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:[{source:"history",turn_id:"legacy-late",value:{role:"user",text:legacyLate}}]})});
      const legacyLateAnswerMigrates=Boolean(migratedLateRows[0]?.value?.origin?.kind==="ask_answer"&&migratedLateRows[0].value.text==="Research Grok groups now."&&!migratedLateRows[0].value.text.includes("late ask answer"));
      renderStory({kind:"settled",agent:"researcher",ok:true});
      state.queue=[];state.previewQueues.set(state.sessionId,[]);state.queueBySession.set(state.sessionId,[]);renderQueue();setWorking(false);
      const tokenPriorItem=state.item;state.item={kind:"group",id:"launch-room"};state.mentions=[];state.groupEveryone=false;$("composerInput").value="Ask Iris to review ";normalizeComposerTokens(false);const inlineAgentToken=Boolean($("mentionTray").hidden&&$("composerInput").querySelector('[data-composer-agent="frontend"]')?.textContent.includes("Iris")&&composerText("request").includes("@frontend")&&!composerText("display").includes("@"));$("composerInput").value="Ask iris to review ";normalizeComposerTokens(false);const wrongCaseDoesNotPing=!$("composerInput").querySelector("[data-composer-agent]")&&state.mentions.length===0;state.item=tokenPriorItem;$("composerInput").value="";state.mentions=[];state.groupEveryone=false;renderMentionTray();
      $("composerInput").value="First line";$("composerInput").focus();setComposerCaretOffset(composerText().length);insertComposerLineBreak();const shiftedText=composerText(),shiftedCaret=composerCaretOffset(),selection=getSelection(),caretAnchoredOnNewLine=Boolean(selection?.rangeCount&&selection.getRangeAt(0).startContainer?.nodeType===Node.TEXT_NODE&&selection.getRangeAt(0).startContainer.data==="\u200B"&&selection.getRangeAt(0).startOffset===1);renderComposerText(`${shiftedText}Second line`,{allowEnd:true});const shiftEnterCreatesLineBreak=shiftedText==="First line\n"&&shiftedCaret===shiftedText.length&&caretAnchoredOnNewLine&&composerText()==="First line\nSecond line";$("composerInput").value="";
      const reviewPrior={item:state.item,rows:state.displayRows,snapshot:state.reviewSnapshot,scope:state.reviewScope};state.item={kind:"agent",id:"coder"};state.reviewScope="agent";state.reviewSnapshot={files:["coder-owned.rs","other-agent.rs"],changes:[]};state.displayRows=[{source:"story",value:{kind:"tool",agent:"coder",tool:"write",target:"coder-owned.rs",ok:true,diff:"+owned"}},{source:"story",value:{kind:"tool",agent:"phoenix",tool:"write",target:"other-agent.rs",ok:true,diff:"+foreign"}}];const agentReviewRows=receiptReviewChanges(),reviewIsAgentScoped=agentReviewRows.length===1&&agentReviewRows[0].path==="coder-owned.rs";state.item=reviewPrior.item;state.displayRows=reviewPrior.rows;state.reviewSnapshot=reviewPrior.snapshot;state.reviewScope=reviewPrior.scope;
      const checks={
        workingButtonShowsStop,
        workingActionIsVisible,
        squareBesideLatestCall,
        foldedToolGroupKeepsSquare,
        workingHoverKeepsStop,
        workingDraftShowsSendNow,
        slashPickerDiscoversCompact,
        slashCommandStayedLocal,
        immediateThinkingCursor,
        headingOnlyReasoningHonest,
        actualReasoningExpandsDisclosure,
        reasoningHeadingDoesNotRepeatBody,
        oneLiveSignal:liveSignals===1,
        literalThinkingLabel,
        individualThoughtRows,
        summaryLabeledThoughts,
        oneTravelingReasoningCursor,
        currentReasoningCursorPersists,
        cursorMovedToTool,
        cursorReturnedToThinking,
        providerRetryLivesInThinking,
        providerRetryClearsAndNeverReplays,
        consecutiveThoughtsGrouped,
        topThoughtHeaderAbsent,
        runningTraceDoesNotAutoExpand,
        interleavedBrowserTimeline,
        replayStartIgnored,
        repeatedReceiptFolded,
        scheduledWorkVisible:feed.querySelectorAll(".routine-message").length===2&&new Set([...feed.querySelectorAll(".routine-message")].map((node)=>node.dataset.turnId)).size===2&&text.includes("Routine")&&text.includes("Check the account status")&&!text.includes("abc12345"),
        steeringPlumbingHidden:!text.includes("Phoenix steered")&&!text.includes("This plumbing must stay hidden")&&!text.includes("phoenix group user boundary")&&!text.includes("turn_internal_secret"),
        promptFocused:promptTop>=0&&promptTop<=96,
        noRawPayload:!["session-secret","credential-secret","ask-secret","trace-design-secret","private raw art direction","\"closest\"","\"cx\"","1333","internal frame"].some((token)=>`${text} ${$("approvalStack").textContent}`.includes(token)),
        friendlyFailure:text.includes("no visible element matching Continue with email")&&text.includes("Nearby: Sign in with Google"),
        faithfulAnswer:answerText.includes("session_id=release-session-12345678")&&answerText.includes("123e4567-e89b-12d3-a456-426614174000"),
        privateRoleTagHidden:visibleAnswerText("**[school_coach]** Hello from Avery.","school_coach")==="Hello from Avery."&&visibleAnswerText("[customer_id] remains authored data.","phoenix")==="[customer_id] remains authored data.",
        safeMentions:text.includes("maya@example.com")&&Boolean(feed.querySelector(".sent-agent-mention")?.textContent.includes("Nico"))&&!text.includes("maya@Maya"),
        neutralHttpFailure:humanFailureDetail("provider request failed HTTP 500","provider_call")==="Request failed (HTTP 500)."&&humanFailureDetail("fetch failed HTTP 403","web_fetch")==="Website request failed (HTTP 403).",
        settledHeaderVisible,
        settledAnimationStopped,
        workedInitiallyCollapsed,
        workedExpandsInPlace,
        structuredTrace:Boolean(structuredTrace),
        oneTurnTrace,
        noMicroThoughtLabels:[...feed.querySelectorAll(".reasoning-subgroup-label")].every((node)=>! /^(?:Thought|Thinking)$/.test(node.textContent)),
        stoppedReasoningVisible:stoppedReasoningBeforeRestore,
        conversationSwitchPreservesTrace,
        permanentTurnControls,
        transportDuplicateSuppressed,
        interleavedDuplicateSuppressed,
        legacyCompletionNoisePruned,
        ephemeralVolumeHandoffHidden,
        legacyVolumeHandoffsMigrated,
        foregroundExcludedFromBackgroundSummary,
        staleQueuedHandoffsExcluded,
        routineMirrorDeduped,
        recoveredHandoffBeforeAnswer,
        coworkerReturnPrecedesOwnerFinal,
        staleResolvedAskMigrated,
        resolvedAskHistoryDoesNotResurrect,
        recoveredWorkPrecedesAnswer,
        catchUpUserIdentityStable,
        legacyCatchUpStormRepaired,
        steerDeliveredImmediately,
        queuedPromptLooksRegular,
        queuedDrawerDoesNotDuplicate,
        queuedWakeShowsCube,
        queuedPromptBoundaryBeforeAnswer,
        detachedTurnWakesVisibleTrace,
        detachedTurnMovesCubeToTool,
        lateAnswerLabelVisible,
        lateAnswerIsWorking,
        lateAnswerHasOneCursor,
        lateAnswerBoundaryVisible,
        claimedQueueBecomesLiveTurn,
        waitingLateAnswerIsCleanAndRemovable,
        legacyLateAnswerMigrates,
        clipboardImagePaste,
        textPasteUnaffected,
        multipleExplorerImageDrop,
        contextCountersArePerSession,
        expiredAuthFailureIsActionable,
        providerModelFiltering,
        liquidAttachmentMenu,
        answerFooter:answerFooterBeforeRestore,
        compactQuestion:Boolean(visibleApproval&&visibleApproval.getBoundingClientRect().width<=380&&parseFloat(getComputedStyle(visibleApproval.querySelector(".approval-question")).fontSize)>=14),
        distinctApprovalVariants:Boolean($("approvalStack").querySelector(".approval-question-card .approval-choice-list")&&$("approvalStack").querySelector(".approval-decision-card .approval-alternatives")),
        decisionAlternativeFlow,
        cleanPermissionDecision:Boolean(visibleApproval?.classList.contains("approval-decision-card")&&visibleApproval.textContent.includes("check the current Chrome page once?")&&!/school_coach|browser_state|\{\}/.test(visibleApproval.textContent)),cleanLoginOwner,
        inlineVaultUnlock:Boolean($("approvalStack").querySelector('[data-ask-id="acceptance-vault"] [data-vault-open]')),
        loginChoices:[...$("approvalStack").querySelectorAll("button")].some((button)=>button.textContent==="Import from my browser")&&[...$("approvalStack").querySelectorAll("button")].some((button)=>button.textContent==="Create an account"),
        humanMention:inlineAgentToken,
        wrongCaseDoesNotPing,
        shiftEnterCreatesLineBreak,
        oneBrowserSurface:document.querySelectorAll("#browserOverlay").length===1&&document.querySelectorAll("#browserCanvas").length===1,
        designActivity:tasteLiveLabel==="Reading Taste direction"&&text.includes("Applied Taste direction")&&imageLiveLabel==="Creating an image"&&text.includes("Created an image"),
        imageResultPreview:imagePreviewVisible,
        imageOpensReviewPane,
        reviewPaneIsRightSidebar,
        reviewIsAgentScoped,
        compactionRunsInWave,
        compactionReceiptPersists,
        realWebMarks,
        detailedToolDisclosure,
        compactDelegation:Boolean(compactDelegation),
        liveSubagentSettlesWithoutReentry,
        groupBrowserReceiptTargetsCoworker,
        directReturnStaysInTrace,
        contextMessageDistinctFromHandoff,
        groupHandoffCheckpointReturnResume:groupHandoffSequence,
        liveWorkStartsCollapsed,
        expandedWorkKeepsOneSquare,
        foldedWorkKeepsSquare,
        reopenedWorkReturnsSquareToStep,
        groupThinkingCubeColored,
        groupCoordinatorTraceUsesSelectedCoworker,
        groupThinkingFolds,
        liveGroupMarkdownParsed,
        groupReplayKeepsPromptToolsAndMarkdown,
        groupRecoveredWorkPrecedesItsAgentAnswer,
        imageCommentTurnDeduped,
        taskPlanLifecycle:taskStayedGloballyCollapsed&&taskExpandedGlobally&&taskPreferenceSurvivedCompletion,
        cleanTraceHeader:Boolean(cluster&&!cluster.querySelector(".work-shimmer,.work-trace-toggle,.work-elapsed,.shimmer-copy")),
        oneVisibleRequest:[...$("approvalStack").children].filter(visible).length===1&&$("toastRegion").children.length===0,
        conversationReadable:getComputedStyle($("conversationStage")).opacity==="1"&&getComputedStyle(feed).opacity==="1"&&fade.getBoundingClientRect().height<=220,
      };
      const durableAskIndexBefore=state.displayRows.findIndex((entry)=>entry.source==="story"&&entry.value?.kind==="ask_pending"&&(entry.value.id||entry.value.ask_id)==="acceptance-answered"),durableAskProbe=document.createElement("div");
      durableAskProbe.dataset.askId="acceptance-answered";durableAskProbe._askState={ask:cloneDisplayValue(state.displayRows[durableAskIndexBefore]?.value),answers:["Keep it compact"]};$("approvalStack").append(durableAskProbe);resolveAskDisplay(durableAskProbe,"Keep it compact","answered");
      const durableAskIndexes=state.displayRows.map((entry,index)=>entry.source==="story"&&entry.value?.kind==="ask_pending"&&(entry.value.id||entry.value.ask_id)==="acceptance-answered"?index:-1).filter((index)=>index>=0);checks.resolvedQuestionStayedInPlace=durableAskIndexes.length===1&&durableAskIndexes[0]===durableAskIndexBefore&&Boolean($("conversationFeed").querySelector(".ask-history-row"));
      const continuationProbe={kind:"user",turn_id:"group-ready-acceptance",text:"INTERNAL_SCHEDULER_ENVELOPE",origin:{kind:"group_continuation",original_turn_id:"original-group-acceptance",display:"Prerequisites ready · Iris continuing"}};
      renderStory(continuationProbe);renderStory(continuationProbe,true);
      const continuationNodes=$("conversationFeed").querySelectorAll(".group-continuation-message");
      checks.groupContinuationIsExplicit=continuationNodes.length===1&&continuationNodes[0].textContent.includes("Group continuation")&&continuationNodes[0].textContent.includes("Iris continuing");
      checks.groupContinuationHidesEnvelope=!$("conversationFeed").textContent.includes("INTERNAL_SCHEDULER_ENVELOPE");
      const sourceTeamTurn="team-status-source-acceptance",resumedTeamTurn="team-status-resumed-acceptance";
      renderStory({kind:"user",turn_id:sourceTeamTurn,text:"Team status source"});
      for(const [agent_id,agent_name,status] of [["researcher","Theo","waiting_user"],["coder","Leo","working"],["critic","Remy","queued"]])renderStory({kind:"group_member_status",turn_id:sourceTeamTurn,agent_id,agent_name,state:status,detail:"Acceptance status"});
      renderStory({kind:"user",turn_id:resumedTeamTurn,text:"Resume acceptance",origin:{kind:"group_continuation",original_turn_id:sourceTeamTurn,display:"Theo and Remy continuing"}});
      renderStory({kind:"group_member_status",turn_id:resumedTeamTurn,agent_id:"researcher",agent_name:"Theo",state:"done",detail:"Contribution saved"});
      const sourceTeam=$("conversationFeed").querySelector(`.group-execution-strip[data-turn-id="${sourceTeamTurn}"]`);
      checks.groupSuccessorSettlesOnlyItsSourceMember=sourceTeam?.querySelector('[data-group-status-agent="researcher"]')?.dataset.state==="done"&&sourceTeam?.querySelector('[data-group-status-agent="coder"]')?.dataset.state==="working"&&sourceTeam?.querySelector('[data-group-status-agent="critic"]')?.dataset.state==="queued";
      renderStory({kind:"ask_pending",turn_id:sourceTeamTurn,id:"team-status-ask",agent:"Remy",questions:[{question:"Acceptance decision?",options:["Proceed"]}],status:"answered",answer:"Proceed"});
      renderStory({kind:"user",turn_id:"team-status-answer",text:"Proceed",origin:{kind:"ask_answer",ask_id:"team-status-ask",agent_id:"critic",display:"Proceed"}});
      renderStory({kind:"group_member_status",turn_id:"team-status-answer",agent_id:"critic",agent_name:"Remy",state:"working",detail:"Execution lane acquired"});
      checks.groupAnswerMovesOnlyMatchedTask=sourceTeam?.querySelector('[data-group-status-agent="critic"]')?.dataset.state==="continued"&&sourceTeam?.querySelector('[data-group-status-agent="coder"]')?.dataset.state==="working";
      renderStory({kind:"group_member_status",turn_id:"team-status-answer",agent_id:"critic",agent_name:"Remy",state:"done",detail:"Contribution saved"});
      checks.groupAnswerSettlesHistoricalWait=sourceTeam?.querySelector('[data-group-status-agent="critic"]')?.dataset.state==="done";
      renderGroupMemberStatus({turn_id:"unloaded-team-history",agent_id:"researcher",agent_name:"Theo",state:"done",detail:"Old receipt"});
      checks.unloadedTeamStatusNeverAppearsAtTail=!$("conversationFeed").querySelector('.group-execution-strip[data-turn-id="unloaded-team-history"]');
      repaintConversation(null,false);
      const replayedTeam=$("conversationFeed").querySelector(`.group-execution-strip[data-turn-id="${sourceTeamTurn}"]`);
      checks.groupSuccessorStateSurvivesReplay=replayedTeam?.querySelector('[data-group-status-agent="researcher"]')?.dataset.state==="done"&&replayedTeam?.querySelector('[data-group-status-agent="critic"]')?.dataset.state==="done"&&replayedTeam?.querySelector('[data-group-status-agent="coder"]')?.dataset.state==="working";
      const settledTeam=$("conversationFeed").querySelector('.group-execution-strip[data-turn-id="team-status-answer"]'),teamToggle=settledTeam?.querySelector('.group-status-toggle');
      checks.finishedTeamDetailsCollapse=Boolean(settledTeam?.classList.contains("is-settled")&&getComputedStyle(settledTeam.querySelector('.group-execution-members')).display==="none"&&!teamToggle.hidden);
      teamToggle?.click();checks.finishedTeamDetailsExpand=teamToggle?.getAttribute('aria-expanded')==="true"&&getComputedStyle(settledTeam.querySelector('.group-execution-members')).display!=="none";teamToggle?.click();
      document.documentElement.dataset.conversationAcceptanceDebug=JSON.stringify({reasoningCursorCount,persistentCursorCount,liveReasoningLeaf:liveReasoningLeaf?.outerHTML||""});
      finish(checks);
    }catch(error){finish({},error);}finally{clearTimeout(deadline);}
  }

  async function runOwnedJournalAcceptance(){
    const checks={};
    try{
      state.item={kind:"agent",id:"phoenix"};state.sessionId="owned-journal-proof";
      clearFeed();replaceDisplayRows([],false);state.turnSocket=null;
      const scope=(turn)=>({turn_id:turn,task_id:`task-${turn}`,attempt_id:`attempt-${turn}`});
      renderStory({kind:"user",turn_id:"owned-old",text:"Earlier task: inspect the research brief."});
      renderStory({kind:"commentary",agent:"phoenix",text:"Research inspection is underway."});
      renderStory({kind:"user",turn_id:"owned-new",text:"Newer task: keep building independently."});
      setWorking(true);beginTurnActivity($("conversationFeed").lastElementChild);
      renderStory({kind:"tool_start",agent:"phoenix",tool:"read",target:"new-task.md",execution:scope("owned-new"),event_sequence:4});
      const currentNodes=[...$("conversationFeed").children].filter((node)=>node.dataset.turnId==="owned-new");
      const liveTool=currentNodes.flatMap((node)=>[...node.querySelectorAll(".work-tool.running")])[0];
      checks.newTaskHasLiveTool=Boolean(liveTool);
      renderStory({kind:"tool",agent:"phoenix",tool:"read",target:"old-task.md",ok:true,execution:scope("owned-old"),event_sequence:5});
      renderStory({kind:"answer",agent:"phoenix",markdown:"Earlier research is verified.",execution:scope("owned-old"),event_sequence:6});
      renderStory({kind:"settled",agent:"phoenix",ok:true,execution:scope("owned-old"),event_sequence:7});
      checks.lateRowsKeepOwner=state.displayRows.filter((entry)=>ownedStoryTurn(entry.value)==="owned-old").every((entry)=>entry.turn_id==="owned-old");
      const nextPromptIndex=state.displayRows.findIndex((entry)=>isAuthoredBoundaryEntry(entry)&&displayTurnId(entry)==="owned-new");
      checks.lateRowsPrecedeNewPrompt=state.displayRows.every((entry,index)=>ownedStoryTurn(entry.value)!=="owned-old"||index<nextPromptIndex);
      checks.newTaskStillWorking=state.working&&state.activeTurnId==="owned-new";
      checks.newTaskDomPreserved=currentNodes.every((node)=>node.isConnected&&node.parentElement===$("conversationFeed"));
      checks.liveToolRemainsRunning=Boolean(liveTool?.isConnected&&liveTool.classList.contains("running"));
      const oldNodes=[...$("conversationFeed").children].filter((node)=>node.dataset.turnId==="owned-old");
      checks.oldAnswerInOldTurn=oldNodes.some((node)=>node.textContent.includes("Earlier research is verified."));
      checks.newTurnNotContaminated=currentNodes.every((node)=>!node.textContent.includes("Earlier research is verified."));
      const count=state.displayRows.length;
      renderStory({kind:"tool",agent:"phoenix",tool:"read",target:"old-task.md",ok:true,execution:scope("owned-old"),event_sequence:5},true);
      checks.replayDeduplicated=state.displayRows.length===count;
      renderStory({kind:"user",turn_id:"owned-old",text:"Earlier task: inspect the research brief.",execution:scope("owned-old"),event_sequence:1},true);
      checks.oldBoundaryDoesNotRewind=state.activeTurnId==="owned-new"&&state.displayRows.length===count;
      updateContext(90000,200000,state.sessionId,"phoenix");
      renderStory({kind:"context_compaction",agent:"phoenix",status:"completed",before_tokens:60000,after_tokens:12000,limit:200000,execution:scope("owned-old"),event_sequence:9},true);
      checks.historicalCompactionKeepsCurrentUsage=state.usage.used===90000&&state.usageBySession.get(state.sessionId)?.used===90000;
      renderStory({kind:"context_compaction",agent:"phoenix",status:"started",execution:scope("owned-old"),event_sequence:10},true);
      const oldCompactions=[...$("conversationFeed").querySelectorAll('[data-turn-id="owned-old"] .context-compaction')];
      checks.historicalStartDoesNotClaimSuccess=oldCompactions.some((row)=>row.dataset.status==="started"&&!row.classList.contains("running")&&row.textContent.includes("completion not recorded"));
      checks.compactionReplayPreservesLiveTool=Boolean(liveTool?.isConnected&&liveTool.classList.contains("running"));
      renderStory({kind:"commentary",agent:"phoenix",text:"Unloaded historical output",execution:scope("owned-unloaded"),event_sequence:8},true);
      checks.unloadedOutputDeferred=!$("conversationFeed").textContent.includes("Unloaded historical output")&&(pendingOwnedStories.get(conversationIdentity())||[]).length===1;
      state.displayRows.unshift({source:"history",turn_id:"owned-unloaded",value:{role:"user",text:"Earlier saved task",turn_id:"owned-unloaded"}});
      flushOwnedStories();
      checks.deferredRowsRecoverToOwner=(pendingOwnedStories.get(conversationIdentity())||[]).length===0&&state.displayRows.some((entry)=>displayTurnId(entry)==="owned-unloaded"&&entry.value.text==="Unloaded historical output")&&state.activeTurnId==="owned-new"&&state.working;
      const restored=parseDisplayRows({[DISPLAY_FEED_KEY]:JSON.stringify({version:2,rows:state.displayRows})});
      checks.persistedOwnership=restored.filter((entry)=>ownedStoryTurn(entry.value)==="owned-old").every((entry)=>entry.turn_id==="owned-old");
    }catch(error){checks.fixtureError=String(error);}
    const failures=Object.entries(checks).filter(([,passed])=>passed!==true).map(([name])=>name);
    document.documentElement.dataset.ownedJournalChecks=JSON.stringify(checks);
    document.title=failures.length?`FAIL owned journal: ${failures.join(", ")}`:"PASS owned journal isolation";
  }

  async function runConversationIsolationAcceptance(){
    const startedAt=performance.now(),mark=(step)=>{document.documentElement.dataset.conversationIsolationStep=step;document.documentElement.dataset.conversationIsolationElapsedMs=String(Math.round(performance.now()-startedAt));};
    mark("initializing");
    let complete=false;const finish=(checks,error=null)=>{if(complete)return;complete=true;const failures=error?[`fixture: ${error.message||error}`]:Object.entries(checks).filter(([,passed])=>!passed).map(([name])=>name);document.documentElement.dataset.conversationIsolation=failures.length?"fail":"pass";document.documentElement.dataset.conversationIsolationChecks=JSON.stringify(checks||{});document.title=failures.length?`FAIL conversation isolation: ${failures.join(", ")}`:"PASS conversation isolation";window.PhoenixConversationIsolationAcceptance=Object.freeze({checks:checks||{},failures,measurements:window.PhoenixConversationIsolationMeasurements||{}});};
    const deadline=setTimeout(()=>finish({},new Error("fixture timed out")),8000),pause=(delay=40)=>new Promise((resolve)=>setTimeout(resolve,delay));
    try{
      for(let attempt=0;attempt<40&&!state.item;attempt+=1)await pause(25);
      const agents=ui.state.view?.directory?.agents||[];let avery=agents.find((agent)=>agent.agent_id==="school_coach"||agent.display_name==="Avery");
      if(!avery){avery={agent_id:"school_coach",internal_role:"school_coach",display_name:"Avery",description:"Vista Virtual School Coach",color:"#6f9577",icon_seed:"school-coach",kind:"responsibility_owner",lifecycle:"active",metadata_json:"{}"};agents.push(avery);}
      const phoenixItem={kind:"agent",id:"phoenix"},averyItem={kind:"agent",id:avery.agent_id},phoenixSession="isolation-phoenix",averySession="isolation-avery";
      state.conversationViews.clear();
      mark("switching");
      let started=performance.now();await selectConversationWithInspection({item:phoenixItem,sessionId:phoenixSession});const coldPhoenixMs=performance.now()-started;
      started=performance.now();await selectConversationWithInspection({item:averyItem,sessionId:averySession});const coldAveryMs=performance.now()-started;
      const averyColdText=$("conversationFeed").textContent,averyColdIsolated=averyColdText.includes(`${avery.display_name} private answer`)&&!averyColdText.includes("Phoenix private answer");
      started=performance.now();await selectConversationWithInspection({item:phoenixItem,sessionId:phoenixSession});const cachedPhoenixMs=performance.now()-started;
      const feed=$("conversationFeed"),maximum=Math.max(0,feed.scrollHeight-feed.clientHeight);feed.scrollTop=Math.round(maximum*.43);state.pinToLatest=false;const scrollBefore=feed.scrollTop;
      await selectConversationWithInspection({item:averyItem,sessionId:averySession});
      started=performance.now();await selectConversationWithInspection({item:phoenixItem,sessionId:phoenixSession});const cachedRestoreMs=performance.now()-started;await pause(80);const scrollAfter=feed.scrollTop,scrollRestored=maximum===0||Math.abs(scrollAfter-scrollBefore)<=6;
      mark("submitting-stale-turn");
      const staleToken=activeSelectionToken();$("composerInput").value="stale preview response must stay in Phoenix";autosize();await submitTurn();
      mark("switching-after-submit");
      started=performance.now();await selectConversationWithInspection({item:averyItem,sessionId:averySession});const cachedAveryMs=performance.now()-started;const staleTokenRejected=!selectionIsCurrent(staleToken);await pause(1500);
      mark("checking-late-output");
      const averyFinalText=feed.textContent,headers=[...feed.querySelectorAll(".agent-message header strong")].map((node)=>node.textContent.trim()),targetName=ui.displayName(averyItem,ui.profileFor(averyItem));
      const noLatePhoenixRender=!averyFinalText.includes("I have the context")&&!averyFinalText.includes("Now I’ll check the company context")&&!averyFinalText.includes("stale preview response")&&!averyFinalText.includes("Phoenix private answer"),foreignTerminalAnswerRejected=!averyFinalText.includes("FOREIGN TERMINAL ANSWER MUST NOT RENDER");
      const measurements={coldPhoenixMs:Number(coldPhoenixMs.toFixed(2)),coldAveryMs:Number(coldAveryMs.toFixed(2)),cachedPhoenixMs:Number(cachedPhoenixMs.toFixed(2)),cachedRestoreMs:Number(cachedRestoreMs.toFixed(2)),cachedAveryMs:Number(cachedAveryMs.toFixed(2)),scrollBefore,scrollAfter};window.PhoenixConversationIsolationMeasurements=Object.freeze(measurements);document.documentElement.dataset.conversationIsolationMeasurements=JSON.stringify(measurements);
      const checks={coldSwitchSubSecond:Math.max(coldPhoenixMs,coldAveryMs)<1000,cachedSwitchSubSecond:Math.max(cachedPhoenixMs,cachedRestoreMs,cachedAveryMs)<1000,cachedSwitchFast:Math.max(cachedPhoenixMs,cachedRestoreMs,cachedAveryMs)<250,averyColdIsolated,oneConversationDom:feed.dataset.conversationKey===conversationIdentity()&&conversationIdentity()===`agent:${avery.agent_id}\u0000${averySession}`,targetHeadersOnly:headers.length>0&&headers.every((name)=>name===targetName),foreignTerminalAnswerRejected,fullOpacity:getComputedStyle(feed).opacity==="1"&&!feed.classList.contains("thread-switching"),scrollRestored,staleTokenRejected,noLatePhoenixRender};
      finish(checks);
    }catch(error){finish({},error);}finally{clearTimeout(deadline);}
  }

  async function runChromeControlsAcceptance(){
    let complete=false;
    const finish=(checks,error=null)=>{if(complete)return;complete=true;const failures=error?[`fixture: ${error.message||error}`]:Object.entries(checks).filter(([,passed])=>!passed).map(([name])=>name);document.documentElement.dataset.chromeControlsAcceptance=failures.length?"fail":"pass";document.documentElement.dataset.chromeControlsChecks=JSON.stringify(checks||{});document.title=failures.length?`FAIL chrome controls: ${failures.join(", ")}`:"PASS chrome controls";window.PhoenixChromeControlsAcceptance=Object.freeze({checks:checks||{},failures});};
    const deadline=setTimeout(()=>finish({},new Error("fixture timed out")),9000),pause=(ms=40)=>new Promise((resolve)=>setTimeout(resolve,ms));
    try{
      for(let attempt=0;attempt<40&&!state.item;attempt+=1)await pause(25);
      state.browserOwnerAgentId="phoenix";state.browserOwnerId="agent-phoenix";state.browserMode="browse";state.browserBoundKey=conversationKeyOf(state.item);state.browserFrameUrl="about:blank";state.browserNative=false;
      renderInspectionBrowserTabs({tabs:[{id:"address-proof",title:"New tab",url:"about:blank",active:true}]});showInspectionSidebar("changes");setInspectionTab("browser");syncBrowserChrome();
      const address=$("browserAddress");address.focus();address.value="x.com/home";address.dispatchEvent(new Event("input",{bubbles:true}));
      for(let index=0;index<5;index+=1)renderInspectionBrowserTabs({tabs:[{id:"address-proof",title:"New tab",url:"about:blank",active:true}]});
      const focusedAddressSurvivesPolling=document.activeElement===address&&address.value==="x.com/home";
      state.browserAddressPending="https://x.com/home";state.browserAddressEditing=false;renderInspectionBrowserTabs({tabs:[{id:"address-proof",title:"New tab",url:"about:blank",active:true}]});const submittedAddressSurvivesPolling=address.value==="x.com/home";state.browserAddressPending="";syncBrowserAddress("about:blank",true);address.blur();
      openModelContext();const picker=document.querySelector(".model-context-popover"),maximum=selectedModelContext().maximum,modelContextChoicesComplete=Boolean(maximum&&picker?.querySelector('[data-model-context=""]')&&picker?.querySelector("[data-model-context-custom]"));picker?.querySelector("[data-model-context-custom]")?.click();const customForm=picker?.querySelector(".model-context-custom"),customInput=customForm?.querySelector("input"),customValue=Math.max(8192,Math.floor(maximum*.25/1024)*1024);if(customInput){customInput.value=String(customValue);customForm.dispatchEvent(new Event("submit",{bubbles:true,cancelable:true}));await pause(50);}const customContextBelowMaximum=selectedModelContext().override===customValue&&selectedModelContext().effective<maximum;applyLocalModelContext(null);ui.closeLayers();
      const priorTheme=document.documentElement.dataset.themeChoice||"light";ui.setTheme("light");await pause(60);await window.PhoenixView.toggleTerminal(true);await pause(360);
      const panel=$("termPanel").getBoundingClientRect(),head=document.querySelector(".term-head").getBoundingClientRect(),tab=document.querySelector(".term-tab").getBoundingClientRect(),screenStyle=getComputedStyle($("termScreen")),lightPalette=screenStyle.backgroundColor==="rgb(255, 255, 255)"&&screenStyle.color==="rgb(36, 36, 36)",terminalProportions=Math.round(head.height)===34&&Math.round(tab.height)===28&&Math.round(tab.top-panel.top)===4;
      const noHorizontalBar=getComputedStyle(document.querySelector(".xterm-scrollable-element>.scrollbar.horizontal")).display==="none";
      const terminalWinsWorkspace=Math.round(panel.right)===innerWidth&&Math.abs($("inspectionSidebar").getBoundingClientRect().bottom-panel.top)<2;
      const globImagePathsRejected=relayedImageReferences("Search ../Doorquoter/**.png and ../Doorquoter/[draft].jpg").length===0;
      ui.setTheme("dark");await pause(60);const darkStyle=getComputedStyle($("termScreen")),darkPalette=darkStyle.backgroundColor==="rgb(24, 24, 24)"&&getComputedStyle(document.querySelector(".term-tab")).backgroundColor==="rgb(36, 36, 36)";ui.setTheme(priorTheme);await pause(30);
      const smoothMotionContract=!getComputedStyle(document.body).transition.includes("--term-current-h")&&getComputedStyle($("termPanel")).transitionDuration.includes("0.3s");
      finish({focusedAddressSurvivesPolling,submittedAddressSurvivesPolling,modelContextChoicesComplete,customContextBelowMaximum,terminalProportions,noHorizontalBar,lightPalette,darkPalette,terminalWinsWorkspace,smoothMotionContract,globImagePathsRejected});
    }catch(error){finish({},error);}finally{clearTimeout(deadline);}
  }

  function runApprovalPreview(kind){
    closeApproval();
    $("taskBlock").hidden=true;$("queueBlock").hidden=true;
    if(kind==="question"){
      renderApproval({kind:"ask_pending",id:"preview-question",agent:"phoenix",questions:[{header:"Launch size",question:"How many flavors should we launch?",options:["Three (core line)","Five (full case)","Just one hero"],multi_select:false},{header:"Mix-ins",question:"Which mix-ins should we stock?",options:["Chocolate chips","Waffle bits","Sprinkles"],multi_select:true},{header:"Market",question:"Which market do we enter first?",options:["Food trucks","Grocery freezers","Scoop shops"],multi_select:false}]});
      document.title="Phoenix question approval";
    }else{
      renderApproval({kind:"ask_pending",id:"preview-decision",agent:"school_coach",questions:[{header:"Permission",question:"Avery (school_coach) wants to use `browser_state` for {}. Allow Full Access for this call?",options:["Allow once","Keep current access"],multi_select:false}],approval:{action:"tool_permission",subject:"browser_state",approved_option:"Allow once",details:{tool_name:"browser_state",current_mode:"workspace",required_mode:"full_access",scope:"single_call"}}});
      document.title="Phoenix decision approval";
    }
  }

  function runAttachmentPreview(){
    const attachment={name:"phoenix-logo-reference.png",path:"/tmp/phoenix-logo-reference.png",size:184320,type:"image/png",preview:ui.phoenixLogoSource()};
    const references=[attachment,...["recommended-settings-desktop.png","canvas-cover-mobile.png","agent-handoff-proof.png"].map((name,index)=>({...attachment,name,path:`/tmp/phoenix-reference-${index+2}.png`}))];
    clearFeed();renderUser("Use these screenshots as the visual reference.",references);
    state.attachments=[attachment];
    renderAttachments();$("composerInput").value="Make the attached interface feel calmer and easier to scan.";autosize();$("composerInput").focus();
    if(new URLSearchParams(location.search).get("open")==="1")setTimeout(toggleAttachmentMenu,120);
    document.title="Phoenix image attachment preview";
  }

  function runInspectionPreview(){
    state.inspectionExpanded=false;clearFeed();replaceDisplayRows([],false);state.activeTools.clear();state.toolRows=[];
    const prompt=renderUser("Polish the activity sidebar and keep the conversation readable.");
    setWorking(true);beginTurnActivity(prompt);
    renderStory({kind:"reasoning",agent:"phoenix",text:"Reviewing the existing sidebar structure"});
    renderStory({kind:"tool",agent:"phoenix",tool:"apply_patch",state:"success",label:"Updated conversation layout",target:"canvas-app/ui/conversation.css",diff:"+ Large review surface\n+ Smooth slide-in motion\n+ Browser, changes, and image views"});
    renderStory({kind:"tool",agent:"phoenix",tool:"write",state:"success",label:"Added image relay",target:"canvas-app/ui/conversation.js",diff:"+ Open local images beside the conversation"});
    renderStory({kind:"context_compaction",agent:"phoenix",status:"completed",before_tokens:251904,after_tokens:93184,folded_messages:42,limit:262144});
    renderAnswer("The review surface is ready. Images, browser work, and code edits now stay visible beside the conversation.","phoenix",{created_at:new Date().toISOString(),elapsed_ms:487000});setWorking(false);
    const tab=new URLSearchParams(location.search).get("tab")||"changes";
    if(tab==="image")openImageInspector({path:"/tmp/phoenix-review-reference.png",name:"phoenix-review-reference.png"});
    else showInspectionSidebar(tab);
    document.title="Phoenix activity sidebar preview";
  }

  function runSummaryProofPreview(){
    if(new URLSearchParams(location.search).get("narrow")==="1")document.documentElement.style.setProperty("--sidebar-width","420px");
    clearFeed();replaceDisplayRows([],false);state.activeTools.clear();state.toolRows=[];
    const prompt=renderUser("Finish the responsive pass, ask the right coworker to inspect it, and show me the proof.");
    setWorking(true);beginTurnActivity(prompt);
    renderStory({kind:"reasoning",agent:"phoenix",text:"Checking the responsive hierarchy and the final proof surface"});
    renderStory({kind:"tool",agent:"phoenix",tool:"apply_patch",state:"success",label:"Refined responsive layout",target:"canvas-app/ui/conversation.css",diff:"+ Keep the annotation rail inside the viewport\n+ Preserve named image tabs"});
    renderHandoff({handoff_id:"summary-proof-handoff",from:"phoenix",to:"frontend",subject:"Inspect the final responsive layering",status:"working"});
    renderReturn({reply_to:"summary-proof-handoff",agent:"frontend",subject:"Inspect the final responsive layering",body:"Iris verified the image rail, tab overflow, and mobile stacking.",ok:true});
    renderAnswer("## Verified result\n\nThe responsive image workspace is ready, with one real screenshot retained as visual proof.\n\n![Phoenix visual proof](/home/mik-is-good/My_Stuff/dev-workspace/Project Phoenix/logos/phoenix_logo.png)","phoenix",{created_at:new Date().toISOString(),elapsed_ms:92000});setWorking(false);
    renderInspectionBrowserTabs({tabs:[{id:"summary-tab-app",title:"Phoenix preview",url:"https://example.com/preview",active:true,favicon:CHROME_ICON},{id:"summary-tab-docs",title:"Reference",url:"https://example.com/reference",active:false,favicon:CHROME_ICON}]});
    toggleActivitySummary(true,false);syncActivitySummary();document.title="Phoenix summary proof";
  }

  function runSourcesProofPreview(){
    clearFeed();replaceDisplayRows([],false);state.activeTools.clear();state.toolRows=[];
    const source=ui.phoenixLogoSource(),files=[
      {name:"phoenix-logo.png",path:"/home/mik-is-good/My_Stuff/dev-workspace/Project Phoenix/logos/phoenix_logo.png",size:184320,type:"image/png",preview:source},
      {name:"responsive-shell-proof.png",path:"/tmp/responsive-shell-proof.png",size:126880,type:"image/png",preview:source},
      {name:"terminal-light-proof.png",path:"/tmp/terminal-light-proof.png",size:98240,type:"image/png",preview:source},
    ];
    renderUser("Keep these three named visual references with the conversation.",files);
    renderAnswer("The Sources panel keeps each raster preview attached to its real filename and opens it in the image workspace.","phoenix",{created_at:new Date().toISOString(),elapsed_ms:17000});
    showInspectionSidebar("sources");renderInspectionSources();document.title="Phoenix source thumbnails proof";
  }

  function runHandoffProofPreview(){
    clearFeed();replaceDisplayRows([],false);state.activeTools.clear();state.toolRows=[];
    const prompt=renderUser("Have Theo validate the evidence and Iris check the interface, then integrate both answers.");
    setWorking(true);beginTurnActivity(prompt);
    renderStory({kind:"reasoning",agent:"phoenix",text:"Splitting the evidence check from the interface review"});
    renderHandoff({handoff_id:"handoff-proof-theo",from:"phoenix",to:"researcher",subject:"Validate the external evidence",status:"working"});
    renderReturn({reply_to:"handoff-proof-theo",agent:"researcher",subject:"Validate the external evidence",body:"The source trail is current and supports the claim.",ok:true});
    renderHandoff({handoff_id:"handoff-proof-iris",from:"phoenix",to:"frontend",subject:"Inspect spacing and layering",status:"working"});
    renderReturn({reply_to:"handoff-proof-iris",agent:"frontend",subject:"Inspect spacing and layering",body:"The responsive hierarchy is clean; no overlap or clipping remains.",ok:true});
    renderAnswer("## Integrated answer\n\n- Theo verified the evidence.\n- Iris verified the rendered interface.\n\nBoth results stayed inside Phoenix’s accountable turn and are reflected in this answer.","phoenix",{created_at:new Date().toISOString(),elapsed_ms:74000});setWorking(false);document.title="Phoenix handoff proof";
  }

  function runBrowserProofPreview(){
    state.inspectionExpanded=false;clearFeed();replaceDisplayRows([],false);renderUser("Keep the live website inside Phoenix while I follow the work.");
    renderAnswer("## Browser surface\n\nThe managed page stays fitted to this conversation. Its named tab, address controls, and content remain visible without opening a detached browser window.","phoenix",{created_at:new Date().toISOString(),elapsed_ms:38000});
    state.browserOwnerAgentId="phoenix";state.browserOwnerId="agent-phoenix";state.browserMode="browse";state.browserBoundKey=conversationKeyOf(state.item);state.browserFrameUrl="https://preview.phoenix.local";renderInspectionBrowserTabs({tabs:[{id:"browser-proof",title:"Phoenix browser proof",url:state.browserFrameUrl,active:true,favicon:CHROME_ICON}]});showInspectionSidebar("browser");const frame=$("browserFrame");frame.src="../artifacts/verification-2026-08-29/expanded-proof/browser-native-content.png";frame.hidden=false;$("browserCanvas").hidden=true;$("browserEmpty").hidden=true;$("browserAddress").value=state.browserFrameUrl;syncBrowserChrome();document.title="Phoenix browser fit proof";
  }

  async function runImageTabsProofPreview(){
    state.inspectionExpanded=false;clearFeed();replaceDisplayRows([],false);renderUser("Keep the visual references open while we compare the responsive shell.");renderAnswer("## Visual comparison\n\nEvery screenshot stays named and independently closable above the image workspace.","phoenix",{created_at:new Date().toISOString(),elapsed_ms:26000});const source=ui.phoenixLogoSource(),count=Math.max(2,Math.min(14,Number(new URLSearchParams(location.search).get("count"))||2));for(let index=0;index<count;index+=1)await openImageInspector({path:`/tmp/responsive-shell-proof-${index+1}.png`,source,name:`responsive-shell-proof-${index+1}.png`});document.title="Phoenix named image tabs proof";
  }

  async function runImageCommentsProofPreview(){
    state.inspectionExpanded=new URLSearchParams(location.search).get("expanded")==="1";
    const raster=document.createElement("canvas");raster.width=760;raster.height=1180;const context=raster.getContext("2d"),gradient=context.createLinearGradient(0,0,760,1180);gradient.addColorStop(0,"#fff7f0");gradient.addColorStop(.52,"#ef6535");gradient.addColorStop(1,"#7f241c");context.fillStyle=gradient;context.fillRect(0,0,760,1180);context.fillStyle="rgba(255,255,255,.93)";context.font="700 54px sans-serif";context.fillText("FULL IMAGE",58,104);context.font="32px sans-serif";context.fillText("Top",58,164);context.fillText("Center",58,590);context.fillText("Bottom",58,1110);const source=raster.toDataURL("image/png"),comments=[{x:49,y:14,text:"Keep the title aligned with the image edge."},{x:64,y:50,text:"This center detail needs more breathing room."},{x:34,y:88,text:"Preserve the complete bottom edge."}];
    clearFeed();replaceDisplayRows([],false);renderUser("Add the image comments to this prompt without sending them yet.");renderAnswer("The image stays fully visible while each pinpoint comment is gathered into the composer.","phoenix",{created_at:new Date().toISOString(),elapsed_ms:19000});state.mentions=[];state.groupEveryone=false;$("composerInput").value="Apply every saved image comment and keep the whole image visible.";
    if(new URLSearchParams(location.search).get("mode")==="composer"){
      const tenComments=Array.from({length:10},(_,index)=>({x:18+(index%5)*15,y:18+Math.floor(index/5)*44,text:`Comment ${index+1}: refine this part without changing the surrounding layout.`}));state.attachments=Array.from({length:12},(_,index)=>({name:`reference-${index+1}.png`,path:`/tmp/reference-${index+1}.png`,size:204800,type:"image/png",preview:source,comments:index<2?tenComments.slice(index*5,index*5+5):[]}));renderAttachments();renderMentionTray();autosize();hideInspectionSidebar();document.title="Phoenix scrollable image comments proof";return;
    }
    state.attachments=[{name:"tall-interface-proof.png",path:"/tmp/tall-interface-proof.png",size:204800,type:"image/png",preview:source,comments}];renderAttachments();renderMentionTray();autosize();await openImageInspector({path:"/tmp/tall-interface-proof.png",source,name:"tall-interface-proof.png",comments});applyInspectionExpanded(state.inspectionExpanded,false);setImageCommentMode(true);state.imageCommentDraft={x:28,y:64,text:"Move this callout without cropping the tall image."};renderImageCommentState();document.title="Phoenix image comments composer proof";
  }

  async function runConversationPagingAcceptance(){
    clearFeed();const rows=[];
    for(let turn=0;turn<60;turn+=1){
      const turnId=`paging-turn-${turn}`;rows.push({source:"history",turn_id:turnId,value:{role:"user",turn_id:turnId,text:`Prompt ${turn+1}`}});
      rows.push({source:"story",turn_id:turnId,value:{kind:"commentary",agent:"phoenix",text:`Working on prompt ${turn+1}`}});
      for(let tool=0;tool<18;tool+=1)rows.push({source:"story",turn_id:turnId,value:{kind:"tool",agent:"phoenix",tool:"read",target:`turn-${turn}/file-${tool}.txt`,ok:true,detail:`Read file ${tool}`}});
      rows.push({source:"story",turn_id:turnId,value:{kind:"answer",agent:"phoenix",markdown:`Finished prompt ${turn+1}.`}});
    }
    replaceDisplayRows(rows,false);state.initialVisibleTurns=5;const started=performance.now();repaintConversation(null,true,true);const initialMs=performance.now()-started,feed=$("conversationFeed"),initialPrompts=feed.querySelectorAll(":scope > .user-message").length,initialDomRows=feed.children.length,fullRows=state.displayRows.length,startBefore=state.renderedRowStart;
    feed.scrollTop=0;queueOlderConversationTurns(true);await new Promise((resolve)=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    const pagedPrompts=feed.querySelectorAll(":scope > .user-message").length,checks={fullContextRetained:fullRows===1260,onlyFiveTurnsPaintInitially:initialPrompts===5&&initialDomRows<140,initialPaintSubsecond:initialMs<1000,olderTurnsPaged:pagedPrompts===10&&state.renderedRowStart<startBefore,scrollAnchorPreserved:feed.scrollTop>0,loaderRemainsUntilComplete:Boolean(feed.querySelector(".conversation-history-loader"))};
    const failures=Object.entries(checks).filter(([,passed])=>!passed).map(([name])=>name);document.documentElement.dataset.conversationPaging=failures.length?"fail":"pass";document.documentElement.dataset.conversationPagingChecks=JSON.stringify({...checks,initialMs,initialDomRows,fullRows,pagedPrompts});document.title=failures.length?`FAIL conversation paging: ${failures.join(", ")}`:"PASS conversation paging";
  }

  function runEmptyReviewPreview(){
    state.inspectionExpanded=false;clearFeed();replaceDisplayRows([],false);state.reviewSnapshot={workspace:state.workspace,additions:0,deletions:0,changes:[],files:["phoenix_agent/canvas-app/src/main.rs","phoenix_agent/canvas-app/ui/conversation.css","phoenix_agent/canvas-app/ui/conversation.js","README.md"],truncated:false};showInspectionSidebar("changes");renderInspectionChanges();document.title="Phoenix empty Review preview";
  }

  function runMarkdownTableProofPreview(){
    state.item={kind:"agent",id:"school_coach"};state.sessionId="markdown-table-proof";clearFeed();replaceDisplayRows([],false);setWorking(false);
    renderAnswer("## VVS worker authentication inheritance probe\n\n| Check | Result | Evidence |\n|---|:---:|---|\n| Inherited vault metadata | **PASS** | Job 1 could see matching credential metadata. |\n| Inherited browser authentication | PASS | Job 2 inherited the signed-in session without entering credentials. |\n| Zero login questions | PASS | Neither job called `ask_for_login` nor `ask_user`. |\n\nNo user-only factor blocked sign-in, and no external course data was changed.","school_coach",{created_at:new Date().toISOString(),elapsed_ms:1200});
    const root=[...$("conversationFeed").querySelectorAll(".agent-message")].at(-1),markdownRoot=root?.querySelector(".markdown"),table=root?.querySelector("table"),wrapper=table?.closest(".markdown-table-wrap"),checks={
      headingRendered:Boolean(root?.querySelector("h2")?.textContent.includes("VVS worker authentication inheritance probe")),
      semanticTable:Boolean(table&&table.querySelectorAll("thead th").length===3&&table.querySelectorAll("tbody tr").length===3&&table.querySelectorAll("tbody td").length===9),
      rawSyntaxGone:Boolean(root&&!root.textContent.includes("|---")&&!root.textContent.includes("| Check |")),
      inlineMarkdownInCells:Boolean(table?.querySelector("strong")?.textContent==="PASS"&&table.querySelector("code")?.textContent==="ask_for_login"),
      responsiveOverflow:getComputedStyle(wrapper).overflowX==="auto",
      tableContained:Boolean(wrapper&&markdownRoot&&wrapper.getBoundingClientRect().right<=markdownRoot.getBoundingClientRect().right+1),
    };
    const failures=Object.entries(checks).filter(([,passed])=>!passed).map(([name])=>name);document.documentElement.dataset.conversationAcceptance=failures.length?"fail":"pass";document.documentElement.dataset.conversationAcceptanceChecks=JSON.stringify(checks);document.title=failures.length?`FAIL Markdown table: ${failures.join(", ")}`:"PASS Markdown table rendering";
  }

  // Every beUI agent component with realistic data (?shot=agent-components).
  function runAgentComponentsProof(){
    state.item={kind:"agent",id:"phoenix"};clearFeed();replaceDisplayRows([],false);state.activeTurnId="agent-components-proof";
    document.documentElement.dataset.conversationView="detailed";
    const part=new URLSearchParams(location.search).get("part")||"all";if(new URLSearchParams(location.search).get("theme"))document.documentElement.dataset.theme=new URLSearchParams(location.search).get("theme");
    renderUser("Tidy the hero and send the unsubscribe email.");
    renderStory({kind:"tool",agent:"phoenix",tool:"bash",target:"npm run build",ok:true,detail:"> kornblume@0.1.0 build\n> next build\n\n✓ Compiled successfully in 4.2s\n✓ Generating static pages (5/5)"});
    renderStory({kind:"tool",agent:"phoenix",tool:"str_replace",target:"src/app/page.tsx",ok:true,diff:"@@ -12,4 +12,5 @@\n   <section className=\"hero\">\n-    <h1>Fresh bread daily</h1>\n+    <h1>Bread worth waking up for</h1>\n+    <p className=\"lede\">Baked at 5am in Kornblume.</p>\n   </section>"});
    renderStory({kind:"tool",agent:"phoenix",tool:"browser_navigate",target:"http://127.0.0.1:5173/",ok:false,detail:"navigation to http://127.0.0.1:5173/ failed: connection refused"});
    renderStory({kind:"tool_start",agent:"phoenix",tool:"bash",target:"npm run dev"});
    if(part==="tools")renderStory({kind:"thinking",agent:"phoenix",text:"**Checking the phone layout**"});
    if(part!=="tools")renderAnswer("Done. The hero now reads:\n\n```tsx\nexport function Hero() {\n  return <h1 className=\"hero\">Bread worth waking up for</h1>;\n}\n```\n\nThe dev server is starting.","phoenix");
    state.tasks=[{task:"Rewrite the hero copy",status:"completed",completed:true},{task:"Check the phone layout",status:"in_progress",progress:40,detail:"2 of 5 widths"},{task:"Send the unsubscribe email"},{task:"Old carousel idea",status:"cancelled"}];setWorking(true);state.taskPlanExpanded=true;renderTasks();
    if(part!=="tools")renderApproval({id:"proof-approval",agent:"school_coach",status:"pending",approval:{action:"governed_effect",subject:"send an email to unsubscribe@news.example.com from divoto-reesty — subject “Unsubscribe”",approved_option:"Allow once",details:{tool_name:"composio_run",effect:"external_send",parameters:JSON.stringify([["Tool","composio_run"],["Action","GMAIL_SEND_EMAIL"],["Account","gmail_divoto-reesty"],["Recipient email","unsubscribe@news.example.com"],["Subject","Unsubscribe"]])}},questions:[{header:"Send",question:"Avery wants to send an email to unsubscribe@news.example.com from divoto-reesty — subject “Unsubscribe”. Your settings ask before anything goes out.",options:["Allow once","Always allow","Deny"],multi_select:false}]});
    if(part==="all")renderApproval({id:"proof-question",agent:"phoenix",status:"pending",questions:[{header:"Hero style",question:"Which hero should the bakery keep?",options:["Photo with overlay","Split layout","Typographic"],multi_select:false},{header:"Extras",question:"What else should the page show?",options:["Opening hours","Map","Instagram feed"],multi_select:true},{header:"New file",question:"What should I name the new file, and what should it do?",options:[],multi_select:false}]});
    const feed=$("conversationFeed"),checks={toolResults:feed.querySelectorAll(".work-tool.tool-result").length>=3,fileDiff:Boolean(feed.querySelector(".work-tool.file-diff .fd-line.added")),codeBlock:Boolean(feed.querySelector(".code-block .cb-row")),todos:document.querySelectorAll("#taskList .td-item").length===4,approval:Boolean(document.querySelector(".approval-decision-card .ta-badge")),question:Boolean(document.querySelector(".approval-question-card .qa-option"))};
    document.documentElement.dataset.agentComponentsChecks=JSON.stringify(checks);document.title=Object.values(checks).every(Boolean)?"PASS agent components":"FAIL agent components";
  }
  function runCompactErrorsProof(){
    state.item={kind:"agent",id:"school_coach"};clearFeed();replaceDisplayRows([],false);state.activeTurnId="compact-error-proof";
    const quota="The provider became unavailable after this agent had already performed work.\nProvider failure: usage_limit_reached\nTool evidence:\n"+"ok browser_extract: Long diagnostic receipt.\n".repeat(80);
    const timeout="Phoenix stopped this agent at a hard runtime boundary: tool browser_act returned without confirmed external termination.\nCompleted tool evidence:\n"+"Private technical details.\n".repeat(40);
    const auth="**[school_coach]** The `school_coach` agent could not complete its turn: OAuth access expired and automatic renewal failed: OAuth token refresh returned HTTP 400: the refresh token was already used; sign in again";
    renderUser("Continue the school work.");renderHistory({role:"answer",agent:"school_coach",text:quota});renderHistory({role:"answer",agent:"school_coach",text:timeout});renderHistory({role:"answer",agent:"school_coach",text:auth});renderAnswer(quota,"school_coach");
    const feed=$("conversationFeed"),errors=[...feed.querySelectorAll('.runtime-error-details')];
    const checks={threeCompactErrors:errors.length===3,closedByDefault:errors.every(n=>!n.open),shortSummaries:errors.every(n=>n.querySelector('summary').textContent.length<130),quotaDistinct:errors[0]?.querySelector('summary').textContent.includes('usage limit'),timeoutDistinct:errors[1]?.querySelector('summary').textContent.includes('browser action timed out'),authDistinct:errors[2]?.querySelector('summary').textContent.includes('sign-in'),fullDiagnosticsPreserved:errors[0]?.querySelector('pre').textContent===quota&&errors[1]?.querySelector('pre').textContent===timeout&&errors[2]?.querySelector('pre').textContent===auth,questionMarkPresent:errors.every(n=>n.querySelector('.runtime-error-help').textContent==='?'),noGiantAnswer:!feed.querySelector('.agent-message .markdown')};
    document.documentElement.dataset.compactErrorsChecks=JSON.stringify(checks);document.title=Object.values(checks).every(Boolean)?"PASS compact errors":"FAIL compact errors";
  }

  function runWorkerPresenceProof(){
    state.item={kind:"group",id:"launch-room"};clearFeed();replaceDisplayRows([],false);state.activeTools.clear();state.ephemeralWorkers.clear();state.activeTurnId="worker-presence-proof";
    renderUser("Find and check the available opportunities.");setWorking(true);
    for(const agent of ["phoenix","researcher","finance"])renderStory({kind:"reasoning",agent,text:`Reviewing the request with ${agent}`});
    renderStory({kind:"handoff",from:"phoenix",to:"researcher",handoff_id:"presence-theo",subject:"Research opportunities",status:"working"});
    renderStory({kind:"handoff",from:"phoenix",to:"finance",handoff_id:"presence-vera",subject:"Check payment terms",status:"working"});
    for(const [batch,count] of [["abc123",12],["def456",4]])renderStory({kind:"notice",text:`Volume batch \`${batch}\` started: ${count} independent jobs, up to ${count} at once.`});
    for(let index=0;index<16;index++)renderStory({kind:"tool",agent:"Worker (volume_worker)",tool:"web_fetch",target:`https://example.com/source-${index}`,ok:true,detail:"Read source"});
    state.displayRows.push({source:"history",turn_id:state.activeTurnId,value:{role:"user",initiating_agent_id:"phoenix",text:"Find opportunities"}});
    const activity={status:"working",active_agent_ids:["phoenix","researcher"]};syncTeamPresence(activity);
    const feed=$("conversationFeed"),pool=feed.querySelector('[data-agent="volume_worker"].team-work-row'),theo=feed.querySelector('[data-agent="researcher"].team-work-row'),vera=feed.querySelector('[data-agent="finance"].team-work-row');
    const checks={onePool:feed.querySelectorAll('[data-agent="volume_worker"].team-work-row').length===1,aggregateExplained:pool.querySelector('strong').textContent==="Worker pool"&&pool.querySelector('small').textContent.includes("16 jobs · 16 calls total"),batchNotesFolded:pool.querySelectorAll('.worker-batch-receipt').length===2&&!feed.querySelector('.notice-row'),namedPresenceSurvivesPeerCompletion:theo.classList.contains('live')&&theo.querySelector('small').textContent==='Thinking',inactiveIsNotWorking:!vera.classList.contains('live')&&vera.querySelector('small').textContent==='Awaiting result',handoffMatchesPresence:feed.querySelector('[data-handoff-to="researcher"] .handoff-status').textContent==='Active · awaiting update'&&feed.querySelector('[data-handoff-to="finance"] .handoff-status').textContent==='Awaiting result',poolDoesNotInventLiveWorkers:!pool.classList.contains('live')};
    renderStory({kind:"tool_start",agent:"researcher",tool:"web_search",target:"Current opportunities"});syncTeamPresence(activity);
    checks.runningToolShown=theo.querySelector('.work-tool.running')&&feed.querySelector('[data-handoff-to="researcher"] .handoff-status').textContent==='Working';
    checks.ownerProgressVisible=feed.querySelectorAll(".handoff-checkpoint").length===1&&feed.querySelector(".handoff-checkpoint").textContent.includes("Theo and Vera")&&feed.querySelector(".handoff-checkpoint").textContent.includes("Waiting for");
    checks.collapsedByDefault=[...feed.querySelectorAll('.team-work-row')].every(n=>!n.classList.contains('group-work-expanded'));
    renderStory({kind:"settled",agent:"Worker (volume_worker)",ok:true});checks.workerCompletionDoesNotEndGroup=state.working;
    syncTeamPresence(activity);
    syncTeamPresence({status:"idle",active_agent_ids:[]});checks.idleClearsPresence=!feed.querySelector('.team-work-row.live');
    syncTeamPresence(activity);document.documentElement.dataset.workerPresenceChecks=JSON.stringify(checks);document.title=Object.values(checks).every(Boolean)?"PASS worker presence":"FAIL worker presence";
  }

  function runOwnerAnswersProof(){
    state.item={kind:"group",id:"launch-room"};clearFeed();state.activeTools.clear();
    state.activeTurnId="owner-answer-proof";state.activeGroupAgentIds=["researcher","phoenix","critic"];
    replaceDisplayRows([{source:"history",turn_id:state.activeTurnId,value:{role:"user",text:"Theo, lead this review.",initiating_agent_id:"researcher"}}],false);
    renderUser("Theo, lead this review.");
    for(const agent of state.activeGroupAgentIds)renderStory({kind:"reasoning",agent,text:"Checking evidence"});
    renderStory({kind:"handoff",from:"researcher",to:"phoenix",subject:"Check implementation",handoff_id:"owner-proof-handoff"});
    renderStory({kind:"group_message",agent_id:"phoenix",markdown:"Private coworker implementation findings",reply_to:"owner-proof-handoff"});
    renderStory({kind:"return",agent:"phoenix",body:"Private coworker implementation findings",reply_to:"owner-proof-handoff"});
    renderStory({kind:"group_message",agent_id:"critic",markdown:"Private coworker risk findings"});
    renderStory({kind:"group_message",agent_id:"researcher",markdown:"The final answer from Theo."});
    const feed=$("conversationFeed"),returns=[...feed.querySelectorAll(".coworker-return")],answers=[...feed.querySelectorAll(".group-message:not(.handoff-checkpoint)")];
    const checks={onlyInitiatorAnswer:answers.length===1&&answers[0].textContent.includes("The final answer from Theo"),delegatedPhoenixFolded:returns.some(n=>n.dataset.agentId==="phoenix"),allReturnsClosed:returns.length===2&&returns.every(n=>!n.open),duplicateReturnCoalesced:returns.filter(n=>n.dataset.agentId==="phoenix").length===1,returnsInsideActivity:returns.every(n=>n.closest(".team-work-row .work-tools")),handoffButtonsPreserved:!!feed.querySelector(".handoff-chain [data-open-agent]")};
    state.item={kind:"agent",id:"researcher"};clearFeed();replaceDisplayRows([],false);state.activeTurnId="direct-return-proof";
    renderUser("Check this with Phoenix.");renderStory({kind:"reasoning",agent:"researcher",text:"Reviewing"});
    renderStory({kind:"return",agent:"phoenix",body:"Direct coworker findings"});
    renderAnswer("Theo’s final answer.","researcher");
    checks.directReturnFolded=!!feed.querySelector(".work-tools .coworker-return:not([open])")&&!feed.querySelector(".peer-message");
    state.item={kind:"group",id:"launch-room"};clearFeed();state.activeTurnId="historic-owner-proof";
    replaceDisplayRows([{source:"history",turn_id:state.activeTurnId,value:{role:"user",text:"Review"}},{source:"story",turn_id:state.activeTurnId,value:{kind:"handoff",from:"critic",to:"researcher"}}],false);
    renderUser("Remy, review the evidence with Theo.");renderStory({kind:"reasoning",agent:"researcher",text:"Checking delegated evidence"});
    renderStory({kind:"group_message",agent_id:"researcher",markdown:"Delegated evidence"});renderStory({kind:"group_message",agent_id:"critic",markdown:"Remy’s final answer."});
    checks.historyUsesHandoffOwner=feed.querySelectorAll(".group-message").length===1&&feed.querySelector(".group-message").textContent.includes("Remy’s final answer");
    document.documentElement.dataset.ownerAnswersChecks=JSON.stringify(checks);document.title=Object.values(checks).every(Boolean)?"PASS owner answers":"FAIL owner answers";
  }

  function runTeamRosterProof(){
    state.item={kind:"group",id:"launch-room"};clearFeed();replaceDisplayRows([],false);state.activeTools.clear();
    state.activeTurnId="roster-proof-first";$("composerInput").value="";state.mentions=[];state.attachments=[];
    renderUser("Theo, Remy and Vera: check the evidence, review the risks, and compare the costs.");setWorking(true);
    const agents=["researcher","critic","finance"];
    for(let round=0;round<3;round++)for(const agent of agents){
      renderStory({kind:"reasoning",agent,text:`**Checking source ${round+1}**\nComparing this source with the earlier evidence.`});
      renderStory({kind:"tool_start",agent,tool:"read",target:`${agent}-source-${round+1}.md`});
      renderStory({kind:"tool",agent,tool:"read",target:`${agent}-source-${round+1}.md`,ok:true,detail:`Verified source ${round+1}.`});
    }
    for(const agent of agents)renderStory({kind:"tool_start",agent,tool:"web_search",target:`Verify ${agent} findings`});
    renderStory({kind:"context_compaction",agent:"researcher",status:"completed",before_tokens:180000,after_tokens:75000,folded_messages:20});
    const rows=[...$("conversationFeed").querySelectorAll(".team-work-row")];
    const checks={compactionInsideDisclosure:!!rows[0].querySelector(".work-trace .context-compaction"),oneRowPerAgent:rows.length===3,collapsedByDefault:rows.every(row=>!row.classList.contains("group-work-expanded")),allCallsPreserved:rows.every(row=>row.querySelectorAll(".work-tool").length===4),correctOwner:rows.every(row=>[...row.querySelectorAll(".work-tool")].every(tool=>tool.dataset.toolTarget.includes(row.dataset.agent)))};
    const theo=rows.find(row=>row.dataset.agent==="researcher"),header=theo.querySelector(".group-work-agent");header.click();
    renderStory({kind:"reasoning",agent:"critic",text:"Checking the last risk"});
    renderStory({kind:"reasoning",agent:"researcher",text:"Cross-checking the final source"});
    checks.openChoiceSurvivesInterleaving=theo.classList.contains("group-work-expanded")&&$("conversationFeed").querySelectorAll(".team-work-row").length===3;
    header.click();
    renderGroupMemberStatus({kind:"group_member_status",turn_id:state.activeTurnId,agent_id:"critic",state:"done"});
    checks.settlesOnlyItsAgent=!rows.find(row=>row.dataset.agent==="critic").classList.contains("live")&&theo.classList.contains("live");
    state.activeTurnId="roster-proof-second";renderUser("Follow up on the evidence.");
    renderStory({kind:"reasoning",agent:"researcher",text:"Investigating the follow-up"});
    checks.newRequestGetsNewRow=$("conversationFeed").querySelectorAll('.team-work-row[data-agent="researcher"]').length===2;
    // Leave the first request on screen for visual review.
    $("conversationFeed").querySelectorAll('[data-turn-id="roster-proof-second"]').forEach(node=>node.remove());state.activeTurnId="roster-proof-first";
    document.documentElement.dataset.teamRosterChecks=JSON.stringify(checks);document.title=Object.values(checks).every(Boolean)?"PASS team roster":"FAIL team roster";
  }

  function runTeamBlockProof(){
    state.item={kind:"group",id:"launch-room"};clearFeed();replaceDisplayRows([],false);state.activeTools.clear();
    state.activeTurnId="team-block-proof";state.activeGroupAgentIds=["phoenix","researcher","finance","critic"];
    const turn=state.activeTurnId,feed=$("conversationFeed"),activity=ui.activityFor(state.item);
    Object.assign(activity,{status:"working",active_agent_ids:["researcher","critic"]});
    renderUser("Phoenix, have Theo check the sources, Vera compare costs, and Remy review risks. Give me one recommendation.");
    state.displayRows.push({source:"history",turn_id:turn,value:{role:"user",text:"Review the options.",initiating_agent_id:"phoenix"}});
    setWorking(true);
    for(const [agent,subject] of [["researcher","Check the current sources"],["finance","Compare costs and eligibility"],["critic","Review risks and missing evidence"]]){
      renderStory({kind:"handoff",from:"phoenix",to:agent,subject,handoff_id:`team-${agent}`,status:"queued"});
      renderStory({kind:"group_member_status",turn_id:turn,agent_id:agent,state:agent==="finance"?"queued":"working"});
    }
    for(const agent of ["researcher","critic"]){renderStory({kind:"reasoning",agent,text:"Checking the evidence"});renderStory({kind:"tool_start",agent,tool:"read",target:`${agent}-notes.md`});}
    syncTeamPresence(activity);
    const block=feed.querySelector('.team-work-block'),button=block.querySelector('.team-work-toggle'),body=block.querySelector('.team-work-body'),title=()=>button.querySelector('strong').textContent;
    const checks={oneBlock:feed.querySelectorAll(':scope > .team-work-block').length===1,handoffsInside:!feed.querySelector(':scope > .handoff-chain')&&body.querySelectorAll('.handoff-chain').length===3,closedByDefault:body.hidden&&button.getAttribute('aria-expanded')==='false',truthfulCounts:button.textContent.includes('2 working')&&button.textContent.includes('1 queued'),ownerExplicit:button.textContent.includes('Phoenix owns the answer'),chipsRetained:block.querySelectorAll('.team-work-participants .agent-metal-chip').length===3,activitySignalRetained:!!button.querySelector('.ld')};
    button.click();renderStory({kind:"reasoning",agent:"researcher",text:"Checking a second source"});
    renderStory({kind:"handoff",from:"phoenix",to:"researcher",subject:"Check the current sources",handoff_id:"team-researcher",status:"working"});
    checks.openChoiceRetained=!body.hidden&&button.getAttribute('aria-expanded')==='true';checks.replayDoesNotDuplicate=body.querySelectorAll('.handoff-chain').length===3;
    const phase=new URLSearchParams(location.search).get('phase')||'working';
    if(phase==='blocked'||phase==='done'){
    renderStory({kind:"return",agent:"critic",reply_to:"team-critic",ok:false,body:"A required source is unavailable.",status:"blocked"});
    renderStory({kind:"group_member_status",turn_id:turn,agent_id:"critic",state:"blocked",detail:"A required source is unavailable."});
    checks.blockerVisible=title()==='Team needs attention';
    renderStory({kind:"group_member_status",turn_id:turn,agent_id:"finance",state:"waiting_user",detail:"Choose the budget."});
    checks.questionVisible=title()==='Waiting for you';
    checks.handoffQuestionMatches=body.querySelector('[data-handoff-to="finance"] .handoff-status').textContent==='Waiting for you';
    checks.checkpointExplainsAttention=feed.querySelector('.handoff-checkpoint').textContent.includes('Vera needs your input')&&feed.querySelector('.handoff-checkpoint').textContent.includes('Remy reported a blocker');
    }
    if(phase==='incomplete'){
      setWorking(false,true);
      checks.interruptedSummary=title()==='Team stopped';
      checks.interruptedHandoffs=[...body.querySelectorAll('.handoff-status')].every(node=>node.textContent==='Stopped');
      checks.interruptedTools=!body.querySelector('.work-tool.running')&&body.querySelectorAll('.tool-status.stopped').length===2;
      checks.interruptedCheckpoint=feed.querySelector('.handoff-checkpoint').textContent.includes('stopped before returning');
      syncTeamPresence(activity);
      checks.stalePresenceCannotRestart=!body.querySelector('.team-work-row.live')&&block.dataset.live==='false';
    }
    if(phase==='done'){
      for(const agent of ['researcher','finance','critic']){
        renderStory({kind:"group_member_status",turn_id:turn,agent_id:agent,state:"done"});
        renderStory({kind:"return",agent,reply_to:`team-${agent}`,ok:true,body:`Internal ${agent} findings.`,status:"done"});
      }
      Object.assign(activity,{status:'idle',active_agent_ids:[]});syncTeamPresence(activity);setWorking(false);
      renderStory({kind:'group_message',agent_id:'phoenix',markdown:'The sources support option A. It has the lowest verified cost; the remaining uncertainty is clearly identified in the comparison.'});
      checks.ownerAnswerOutside=!!feed.querySelector(':scope > .group-message:not(.handoff-checkpoint)')&&!body.querySelector('.group-message');
    }
    button.click();checks.stillOneBlock=feed.querySelectorAll(':scope > .team-work-block').length===1;checks.staysCollapsed=body.hidden;
    if(phase==='history'){
      state.activeTurnId='team-block-next';
      renderUser('Phoenix, check the next option.');
      state.displayRows.push({source:'history',turn_id:state.activeTurnId,value:{role:'user',text:'Check the next option.',initiating_agent_id:'phoenix'}});
      renderStory({kind:'handoff',from:'phoenix',to:'researcher',subject:'Check the next option',handoff_id:'team-next',status:'working'});
      renderStory({kind:'reasoning',agent:'researcher',text:'Checking the next option'});
      syncTeamPresence(activity);
      checks.turnsStaySeparate=feed.querySelectorAll(':scope > .team-work-block').length===2&&body.querySelectorAll('.handoff-chain').length===3;
      checks.oldTurnQuiet=block.dataset.live==='false';
      checks.oldDisclosureRetained=body.hidden;
    }
    if(new URLSearchParams(location.search).get('theme')==='dark')document.documentElement.dataset.theme='dark';
    const beforeEnd=feed.innerHTML,workingBeforeEnd=state.working;
    renderStory({kind:"execution_ended",execution:{turn_id:state.activeTurnId,task_id:state.activeTurnId,attempt_id:"fixture"}});
    checks.internalEndHidden=feed.innerHTML===beforeEnd&&state.working===workingBeforeEnd;
    document.documentElement.dataset.teamBlockChecks=JSON.stringify(checks);document.title=Object.values(checks).every(Boolean)?'PASS team block':'FAIL team block';
  }

  function runLiveControlsProof(){
    state.item={kind:"group",id:"launch-room"};clearFeed();replaceDisplayRows([],false);state.activeTools.clear();
    $("composerInput").value="";state.mentions=[];state.attachments=[];
    const prompt=renderUser("Phoenix, ask Iris to review the conversation. Keep `Phoenix` as code and maya@example.com as an email.");
    setWorking(true);beginTurnActivity(prompt,"phoenix");
    renderStory({kind:"tool_start",agent:"phoenix",tool:"read",target:"conversation.css"});
    renderStory({kind:"tool",agent:"phoenix",tool:"read",target:"conversation.css",ok:true,detail:"Read the conversation styles."});
    renderStory({kind:"tool_start",agent:"phoenix",tool:"bash",target:"Verify conversation controls"});
    $("conversationFeed").querySelectorAll(".trace-subgroup").forEach(group=>group.open=true);
    document.title="Phoenix live controls proof";
  }

  function runProviderRetryProofPreview(){
    state.item={kind:"agent",id:"school_coach"};state.sessionId="provider-retry-proof";clearFeed();replaceDisplayRows([],false);state.activeTools.clear();state.toolRows=[];
    const prompt=renderUser("Continue the lesson review when provider capacity returns.");setWorking(true);beginTurnActivity(prompt,"school_coach");
    renderStory({kind:"reasoning",agent:"school_coach",text:"**Preparing the lesson evidence**\n\nI’m checking the saved browser state and the remaining lesson material before continuing."});
    renderStory({kind:"tool_start",agent:"school_coach",tool:"browser_state",target:"current lesson page"});renderStory({kind:"tool",agent:"school_coach",tool:"browser_state",target:"current lesson page",ok:true,detail:"Lesson page retained."});
    renderStory({kind:"notice",text:"provider temporarily unavailable: codex stream reported an error: Our servers are currently overloaded. Please try again later.; retrying in 12s (attempt 4)"});
    document.title="Phoenix provider retry proof";
  }

  function runResponseTimingAcceptance(){
    clearFeed();replaceDisplayRows([],false);
    const response=renderAnswer("I still need your answer before I can finish.","phoenix",{created_at:"2026-09-05T12:00:00Z",elapsed_ms:65000});
    const footer=response.querySelector(".completion-meta");
    const checks={durationPreserved:footer.textContent.includes("Responded in 1m 5s"),noInventedGoalSuccess:!footer.textContent.includes("Goal achieved")&&!footer.querySelector(".completion-check"),partialAnswerPreserved:response.textContent.includes("I still need your answer before I can finish.")};
    document.documentElement.dataset.responseTimingChecks=JSON.stringify(checks);
    document.title=Object.values(checks).every(Boolean)?"PASS response timing":"FAIL response timing";
  }

  function runPreviewWhenDirectoryReady(callback,delay=0){
    let started=false;const start=()=>{if(started)return;started=true;setTimeout(callback,delay);};
    if(ui.state.view)start();else{addEventListener("phoenix:directory-ready",start,{once:true});setTimeout(()=>{if(ui.state.view)start();},1000);}
  }
  if (new URLSearchParams(location.search).get("shot") === "settings") {
    const params = new URLSearchParams(location.search);
    const openLook = () => window.dispatchEvent(new CustomEvent("phoenix:open-settings", { detail: { section: params.get("section") || "Appearance" } }));
    const run = () => { openLook(); if (params.get("focus")==="icons") setTimeout(()=>document.querySelector(".icon-family-section")?.scrollIntoView({block:"center"}),200);if(params.get("view")==="providers")setTimeout(()=>document.querySelector('[data-model-view="providers"]')?.click(),240);if(params.get("focus")==="provider-connect")setTimeout(()=>{document.querySelector('[data-model-view="providers"]')?.click();setTimeout(()=>document.querySelector('.provider-toolbar [data-provider-add]')?.click(),180);},240); };
    if (ui.state.view) setTimeout(run, 40);
    else addEventListener("phoenix:directory-ready", () => setTimeout(run, 40), { once: true });
  }
  if (new URLSearchParams(location.search).get("shot") === "model-picker") {
    const run = () => setTimeout(() => $("modelButton")?.click(), 220);
    if (ui.state.view) run();
    else addEventListener("phoenix:directory-ready", run, { once:true });
  }
  // Visual proof of the working-turn layout: thinking lives only on the
  // live cube; the agent's own messages sit between tool rows.
  if (preview&&previewShot==="work-updates-proof") {
    runPreviewWhenDirectoryReady(async()=>{
      const pause=(ms=60)=>new Promise((resolve)=>setTimeout(resolve,ms));
      clearFeed();replaceDisplayRows([],false);
      const ask="Is gpt-6-sol cheaper than 5.6 sol, and can you switch all my agents to it?";
      appendDisplay("history",{role:"user",text:ask},true);const prompt=renderUser(ask);
      state.turnStartedAt=Date.now()-42000;setWorking(true);beginTurnActivity(prompt);
      renderStory({kind:"thinking",agent:"phoenix",text:"**Checking the model catalog**\n\nPrivate planning."});await pause();
      renderStory({kind:"commentary",agent:"phoenix",text:"Yes — gpt-6-sol is cheaper per token and stronger on the hard trials. I'll switch your agent lanes now and verify each one answers."});await pause();
      renderStory({kind:"tool_start",agent:"phoenix",tool:"read",target:"~/.phoenix/config.toml"});renderStory({kind:"tool",agent:"phoenix",tool:"read",target:"~/.phoenix/config.toml",ok:true,detail:"Read 196 lines."});await pause();
      renderStory({kind:"tool_start",agent:"phoenix",tool:"str_replace",target:"~/.phoenix/config.toml"});renderStory({kind:"tool",agent:"phoenix",tool:"str_replace",target:"~/.phoenix/config.toml",ok:true,detail:"Updated 6 lanes."});await pause();
      renderStory({kind:"thinking",agent:"phoenix",text:"**Verifying each lane**"});await pause();
      renderStory({kind:"commentary",agent:"phoenix",text:"Config updated for Phoenix, Theo, Iris, Leo and Avery. Running a live check on each lane."});await pause();
      renderStory({kind:"tool_start",agent:"phoenix",tool:"bash",target:"phoenix check"});await pause(300);
      renderStory({kind:"thinking",agent:"phoenix",text:"**Waiting for the provider check**"});
      document.title="READY work-updates-proof";
    },120);
  }
  if(preview&&previewShot==="repair-acceptance"){
    runPreviewWhenDirectoryReady(()=>{
      clearFeed();state.displayRows=[];state.activeTurnId="";state.painting=false;state.working=false;
      renderStory({kind:"user",text:"Prepare a reply and ask me before sending.",turn_id:"repair-question"});
      const ask={kind:"ask_pending",id:"repair-ask",agent:"phoenix",turn_id:"repair-question",questions:[{header:"Reply",question:"Send this reviewed reply?",options:["Send","Keep draft"],multi_select:false}]};
      renderStory(ask);
      renderStory({kind:"answer",markdown:"The reviewed draft is ready. I am waiting for your choice.",turn_id:"repair-question"});
      const pendingQuestionHasNoFinal=$("conversationFeed").querySelectorAll(".agent-message .answer-footer").length===0&&Boolean($("conversationFeed").querySelector(".awaiting-input-update"))&&Boolean(document.querySelector('[data-ask-id="repair-ask"]'));
      const card=document.querySelector('[data-ask-id="repair-ask"]');card._askState.answers=["Keep draft"];resolveAskDisplay(card,"Keep draft","answered");
      const resolvedQuestionDoesNotResurrectFinal=$("conversationFeed").querySelectorAll(".agent-message .answer-footer").length===0&&Boolean($("conversationFeed").querySelector(".awaiting-input-update"));
      renderStory({kind:"user",text:"Keep draft",turn_id:"repair-response"});
      renderStory({kind:"answer",markdown:"Saved the reviewed draft. Nothing was sent.",turn_id:"repair-response"});
      const oneFinalAfterResponse=$("conversationFeed").querySelectorAll(".agent-message .answer-footer").length===1;
      renderStory({kind:"answer",markdown:"Nothing new: the earlier visualization was already checked.",turn_id:"repair-response"});
      const lateStatusKeepsCompletedAnswer=$("conversationFeed").querySelectorAll(".agent-message:not(.superseded-answer) .answer-footer").length===1&&$("conversationFeed").querySelector(".agent-message:not(.superseded-answer) .markdown")?.textContent.includes("Saved the reviewed draft.");
      const previousView=document.documentElement.dataset.conversationView;document.documentElement.dataset.conversationView="detailed";
      renderStory({kind:"tool",agent:"phoenix",tool:"read",target:"draft.md",ok:true,detail:"Reviewed draft"});
      const group=$("conversationFeed").querySelector(".trace-subgroup"),toolsInitiallyCollapsed=Boolean(group&&!group.open&&getComputedStyle(group.querySelector(".trace-subgroup-body")).display==="none");
      group.open=true;updateTraceSubgroup(group);const explicitExpansionSurvivesUpdates=group.open&&getComputedStyle(group.querySelector(".trace-subgroup-body")).display!=="none";group.open=false;
      document.documentElement.dataset.conversationView=previousView||"compact";
      const checks={pendingQuestionHasNoFinal,resolvedQuestionDoesNotResurrectFinal,oneFinalAfterResponse,lateStatusKeepsCompletedAnswer,toolsInitiallyCollapsed,explicitExpansionSurvivesUpdates};
      document.documentElement.dataset.repairAcceptanceChecks=JSON.stringify(checks);document.documentElement.dataset.repairAcceptance=Object.values(checks).every(Boolean)?"pass":"fail";document.title=`${document.documentElement.dataset.repairAcceptance.toUpperCase()} Phoenix repair acceptance`;
    },120);
  }
  if (preview&&new URLSearchParams(location.search).get("shot")==="conversation-acceptance") {
    runPreviewWhenDirectoryReady(runConversationAcceptance,120);
  }
  if(preview&&previewShot==="owned-journal-acceptance"){
    runPreviewWhenDirectoryReady(runOwnedJournalAcceptance,120);
  }
  if(preview&&previewShot==="response-timing-acceptance"){
    runPreviewWhenDirectoryReady(runResponseTimingAcceptance,120);
  }
  if(preview&&previewShot==="conversation-isolation"){
    runPreviewWhenDirectoryReady(runConversationIsolationAcceptance,120);
  }
  if(preview&&previewShot==="conversation-paging"){
    runPreviewWhenDirectoryReady(runConversationPagingAcceptance,120);
  }
  if(preview&&previewShot==="chrome-controls-acceptance"){
    runPreviewWhenDirectoryReady(runChromeControlsAcceptance,120);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="attachment"){
    runPreviewWhenDirectoryReady(runAttachmentPreview,160);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="inspection"){
    runPreviewWhenDirectoryReady(runInspectionPreview,900);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="summary-proof"){
    runPreviewWhenDirectoryReady(runSummaryProofPreview,520);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="sources-proof"){
    runPreviewWhenDirectoryReady(runSourcesProofPreview,320);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="handoff-proof"){
    runPreviewWhenDirectoryReady(runHandoffProofPreview,420);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="browser-proof"){
    runPreviewWhenDirectoryReady(runBrowserProofPreview,420);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="image-tabs-proof"){
    runPreviewWhenDirectoryReady(runImageTabsProofPreview,420);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="image-comments-proof"){
    runPreviewWhenDirectoryReady(runImageCommentsProofPreview,420);
  }
  if(preview&&new URLSearchParams(location.search).get("shot")==="review-empty"){
    runPreviewWhenDirectoryReady(runEmptyReviewPreview,180);
  }
  if(preview&&previewShot==="compact-errors-proof"){runPreviewWhenDirectoryReady(runCompactErrorsProof,160);}
  if(preview&&previewShot==="agent-components"){runPreviewWhenDirectoryReady(runAgentComponentsProof,160);}
  if(preview&&previewShot==="worker-presence-proof"){runPreviewWhenDirectoryReady(runWorkerPresenceProof,160);}
  if(preview&&previewShot==="owner-answers-proof"){runPreviewWhenDirectoryReady(runOwnerAnswersProof,160);}
  if(preview&&previewShot==="team-roster-proof"){runPreviewWhenDirectoryReady(runTeamRosterProof,160);}
  if(preview&&previewShot==="team-block-proof"){runPreviewWhenDirectoryReady(runTeamBlockProof,160);}
  if(preview&&previewShot==="live-controls-proof"){runPreviewWhenDirectoryReady(runLiveControlsProof,160);}
  if(preview&&previewShot==="provider-retry-proof"){
    runPreviewWhenDirectoryReady(runProviderRetryProofPreview,160);
  }
  if(preview&&previewShot==="markdown-table-proof"){
    runPreviewWhenDirectoryReady(runMarkdownTableProofPreview,160);
  }
  if(preview&&["approval-question","approval-decision"].includes(new URLSearchParams(location.search).get("shot"))){
    const kind=new URLSearchParams(location.search).get("shot").replace("approval-","");
    runPreviewWhenDirectoryReady(()=>runApprovalPreview(kind),140);
  }
}
