#!/usr/bin/env node

import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const port = Number(process.env.PHOENIX_CHROMIUM_DEBUG_PORT || "17442");
const screenshot = process.env.PHOENIX_BUILD_GROUP_SCREENSHOT || "";
const cleanupTerminals = process.env.PHOENIX_BUILD_GROUP_CLEAN_TERMINALS === "1";

async function targets() {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`, {
    signal: AbortSignal.timeout(5000),
  });
  if (!response.ok) throw new Error(`DevTools target list failed: HTTP ${response.status}`);
  return response.json();
}

function cdp(webSocketDebuggerUrl) {
  const socket = new WebSocket(webSocketDebuggerUrl);
  let nextId = 1;
  const pending = new Map();
  const ready = new Promise((resolveReady, reject) => {
    socket.addEventListener("open", resolveReady, { once: true });
    socket.addEventListener("error", () => reject(new Error("DevTools socket failed")), { once: true });
  });
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(String(event.data));
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(new Error(message.error.message));
    else request.resolve(message.result);
  });
  return {
    async send(method, params = {}) {
      await ready;
      const id = nextId++;
      return new Promise((resolveRequest, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error(`DevTools command timed out: ${method}`));
        }, 30000);
        pending.set(id, { resolve: resolveRequest, reject, timer });
        socket.send(JSON.stringify({ id, method, params }));
      });
    },
    close() { socket.close(); },
  };
}

async function evaluate(client, expression) {
  const response = await client.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (response.exceptionDetails) {
    throw new Error(response.exceptionDetails.exception?.description || "renderer evaluation failed");
  }
  return response.result?.value;
}

const uiTarget = (await targets()).find((target) => target.type === "page" && target.title === "Phoenix");
if (!uiTarget) throw new Error("Phoenix Chromium UI target is missing");
const ui = cdp(uiTarget.webSocketDebuggerUrl);
await ui.send("Page.enable");

const report = await evaluate(ui, `(async () => {
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const deadline = performance.now() + 15000;
  while (performance.now() < deadline) {
    if (window.PhoenixUI?.state?.view && window.PhoenixConversation) break;
    await sleep(50);
  }
  // Selection commits synchronously and refreshes in the background. Do not
  // make the proof wait on a slow remote history refresh.
  void window.PhoenixUI.selectItem({ kind: "group", id: "build-group" });
  const selectedDeadline = performance.now() + 15000;
  while (performance.now() < selectedDeadline) {
    if (document.getElementById("stageName")?.textContent?.trim() === "Build Group"
      && !document.querySelector(".conversation-loading")) break;
    await sleep(50);
  }
  await sleep(500);
  const input = document.getElementById("composerInput");
  input.focus();
  input.replaceChildren(document.createTextNode("Hey Theo "));
  input.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: " " }));
  await sleep(100);
  const token = input.querySelector(".composer-inline-agent");
  const exactToken = {
    count: input.querySelectorAll(".composer-inline-agent").length,
    label: token?.innerText?.trim() || "",
    agent: token?.dataset.composerAgent || "",
    avatar: Boolean(token?.querySelector(".agent-chip-avatar")),
  };
  input.replaceChildren(document.createTextNode("Hey theo "));
  input.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: " " }));
  await sleep(80);
  const wrongCaseTokenCount = input.querySelectorAll(".composer-inline-agent").length;
  // Leave the correct token visible for the optional screenshot proof. The
  // outer check clears it immediately after capture, so no draft is left in
  // the user's group composer.
  input.replaceChildren(document.createTextNode("Hey Theo "));
  input.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: " " }));
  await sleep(80);
  let terminalTabs = [];
  const onTerminalTabs = (event) => { terminalTabs = event.detail?.tabs || []; };
  addEventListener("phoenix:terminal-tabs", onTerminalTabs);
  await window.PhoenixView.toggleTerminal(true);
  await sleep(250);
  const existingTerminalKeys = new Set(terminalTabs.map((tab) => tab.key));
  document.getElementById("termNewTab")?.click();
  const terminalDeadline = performance.now() + 8000;
  let testTerminal = null;
  while (performance.now() < terminalDeadline) {
    testTerminal = [...terminalTabs].reverse().find((tab) => tab.id > 0 && !existingTerminalKeys.has(tab.key)) || null;
    if (testTerminal) break;
    await sleep(40);
  }
  let terminalOutput = "";
  let unlistenTerminal = null;
  if (testTerminal) {
    unlistenTerminal = await window.__TAURI__.event.listen("term-data", (event) => {
      if (event.payload?.id === testTerminal.id) terminalOutput += String(event.payload?.data || "");
    });
    await window.PhoenixUI.invoke("term_write", { id: testTerminal.id, data: "printf '__PHOENIX_TERMINAL_LIVE_OK__\\n'\\n" });
    const outputDeadline = performance.now() + 5000;
    while (performance.now() < outputDeadline && !terminalOutput.includes("__PHOENIX_TERMINAL_LIVE_OK__")) await sleep(35);
  }
  unlistenTerminal?.();
  await window.PhoenixView.toggleTerminal(true);
  await sleep(360);
  if (!document.body.classList.contains("term-open")) {
    await window.PhoenixView.toggleTerminal(true);
    await sleep(360);
  }
  document.getAnimations().forEach((animation) => { try { if (Number.isFinite(animation.effect?.getComputedTiming().endTime)) animation.finish(); } catch {} });
  const terminalPanel = document.getElementById("termPanel");
  const terminalProof = {
    id: testTerminal?.id || 0,
    marker: terminalOutput.includes("__PHOENIX_TERMINAL_LIVE_OK__"),
    open: document.body.classList.contains("term-open") && !terminalPanel.hidden,
    height: terminalPanel.getBoundingClientRect().height,
    headerHeight: terminalPanel.querySelector(".term-head")?.getBoundingClientRect().height || 0,
    xterm: Boolean(terminalPanel.querySelector(".xterm")),
  };
  if (testTerminal) terminalPanel.querySelector('[data-term-tab="' + CSS.escape(testTerminal.key) + '"] [data-term-tab-close]')?.click();
  await sleep(120);
  await window.PhoenixView.toggleTerminal(false);
  removeEventListener("phoenix:terminal-tabs", onTerminalTabs);
  window.PhoenixConversation.showInspectionSidebar("changes");
  await sleep(380);
  if (!document.body.classList.contains("inspection-open")) {
    window.PhoenixConversation.showInspectionSidebar("changes");
    await sleep(380);
  }
  // The proof runner itself can leave Phoenix unfocused, which lets Chromium
  // throttle purely visual transitions. Finish them before measuring the
  // same final geometry a focused user sees.
  document.getAnimations().forEach((animation) => { try { if (Number.isFinite(animation.effect?.getComputedTiming().endTime)) animation.finish(); } catch {} });
  const inspection = document.getElementById("inspectionSidebar");
  const stage = document.getElementById("conversationStage");
  const inspectionHandle = document.getElementById("inspectionResize");
  const inspectionBefore = inspection.getBoundingClientRect();
  const stageBefore = stage.getBoundingClientRect();
  const handleBefore = inspectionHandle.getBoundingClientRect();
  inspectionHandle.focus();
  inspectionHandle.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
  await sleep(90);
  const inspectionAfter = inspection.getBoundingClientRect();
  inspectionHandle.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
  await sleep(90);
  const workspaceProof = {
    viewportWidth: innerWidth,
    documentWidth: document.documentElement.clientWidth,
    bodyWidth: document.body.clientWidth,
    visualViewportWidth: visualViewport?.width || 0,
    bodyClasses: document.body.className,
    inspectionComputed: {
      right: getComputedStyle(inspection).right,
      width: getComputedStyle(inspection).width,
      transform: getComputedStyle(inspection).transform,
      opacity: getComputedStyle(inspection).opacity,
    },
    inspectionBefore: inspectionBefore.toJSON(),
    inspectionAfter: inspectionAfter.toJSON(),
    stageBefore: stageBefore.toJSON(),
    handleBefore: handleBefore.toJSON(),
    rightAligned: Math.abs(inspectionBefore.right - innerWidth) < 2,
    pushesConversation: Math.abs(stageBefore.right - inspectionBefore.left) < 2,
    fullHeightHandle: handleBefore.height >= inspectionBefore.height - 2 && handleBefore.width >= 8,
    keyboardResize: inspectionAfter.width >= inspectionBefore.width + 14,
  };
  window.PhoenixConversation.closeInspectionSidebar();
  await sleep(120);
  const feed = document.getElementById("conversationFeed");
  const groupMessages = [...feed.querySelectorAll(".group-message")];
  const messageIds = groupMessages.map((row) => row.dataset.messageId).filter(Boolean);
  const workClusters = [...feed.querySelectorAll(".group-work-cluster")];
  return {
    selected: window.PhoenixUI.state.selected,
    stageName: document.getElementById("stageName")?.textContent?.trim(),
    opacity: getComputedStyle(feed).opacity,
    userPrompts: feed.querySelectorAll(".user-message").length,
    toolRows: feed.querySelectorAll(".work-tool").length,
    groupMessages: groupMessages.length,
    markdown: {
      headings: feed.querySelectorAll(".group-message .markdown h1,.group-message .markdown h2,.group-message .markdown h3").length,
      lists: feed.querySelectorAll(".group-message .markdown li").length,
      code: feed.querySelectorAll(".group-message .markdown code").length,
    },
    internalToolRows: [...feed.querySelectorAll(".work-tool")]
      .filter((row) => /^__phoenix_/i.test(row.dataset.tool || "") || /phoenix group user boundary/i.test(row.textContent || ""))
      .length,
    uniqueMessageIds: new Set(messageIds).size === messageIds.length,
    workClusters: workClusters.map((cluster) => {
      const agent = cluster.dataset.agent || "";
      const label = cluster.querySelector(".group-work-agent strong")?.textContent?.trim() || "";
      const profile = window.PhoenixUI.state.view?.directory?.agents?.find((candidate) =>
        candidate.agent_id === agent || candidate.internal_role === agent);
      return {
        agent,
        label,
        expectedLabel: profile?.display_name || "",
        identityAligned: !profile || profile.display_name === label,
        accent: getComputedStyle(cluster).getPropertyValue("--agent-accent").trim(),
        reasoning: cluster.querySelectorAll(".reasoning-subgroup").length,
        reasoningDetails: cluster.querySelectorAll(".commentary-line").length,
        reasoningLabels: [...cluster.querySelectorAll(".reasoning-subgroup-label")]
          .map((row) => row.textContent?.trim())
          .filter(Boolean),
        cubes: cluster.querySelectorAll(".pixel-loader").length,
        tools: cluster.querySelectorAll(".work-tool").length,
      };
    }),
    exactToken,
    wrongCaseTokenCount,
    terminalProof,
    workspaceProof,
    consoleErrors: window.__PHOENIX_TEST_CONSOLE_ERRORS__ || [],
  };
})()`);

if (screenshot) {
  const absolute = resolve(screenshot);
  await mkdir(dirname(absolute), { recursive: true });
  const capture = await ui.send("Page.captureScreenshot", { format: "png", fromSurface: true });
  await writeFile(absolute, Buffer.from(capture.data, "base64"));
  report.screenshot = absolute;
}

await evaluate(ui, `(() => {
  const input = document.getElementById("composerInput");
  input?.replaceChildren();
  input?.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "deleteContentBackward" }));
  return true;
})()`);
if (cleanupTerminals) {
  report.remainingTerminals = await evaluate(ui, `(async () => {
    for (let attempt = 0; attempt < 20; attempt += 1) {
      const close = document.querySelector("#termTabs .term-tab [data-term-tab-close]");
      if (!close) break;
      close.click();
      await new Promise((resolve) => setTimeout(resolve, 60));
    }
    await window.PhoenixView.toggleTerminal(false);
    return document.querySelectorAll("#termTabs .term-tab").length;
  })()`);
}

report.ok = Boolean(
  report.stageName === "Build Group"
    && report.opacity === "1"
    && report.userPrompts > 0
    && report.toolRows > 0
    && report.groupMessages > 0
    && report.markdown.headings > 0
    && report.markdown.lists > 0
    && report.internalToolRows === 0
    && report.uniqueMessageIds
    && report.workClusters.every((cluster) => cluster.identityAligned)
    && report.workClusters.some((cluster) => cluster.accent && cluster.reasoning > 0)
    && report.exactToken.count === 1
    && report.exactToken.label === "Theo"
    && report.exactToken.avatar
    && report.wrongCaseTokenCount === 0
    && report.terminalProof.marker
    && report.terminalProof.open
    && report.terminalProof.height >= 140
    && report.terminalProof.headerHeight <= 36
    && report.terminalProof.xterm
    && Object.values(report.workspaceProof).every(Boolean)
);

ui.close();
console.log(`PHOENIX_BUILD_GROUP_LIVE ${JSON.stringify(report)}`);
if (!report.ok) process.exitCode = 2;
