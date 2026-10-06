#!/usr/bin/env node

import { writeFile } from "node:fs/promises";

const port = Number(process.env.PHOENIX_CHROMIUM_DEBUG_PORT || "17442");
const screenshot = process.env.PHOENIX_CHROMIUM_INTEGRATION_SCREENSHOT || "";

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
  const ready = new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
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
  socket.addEventListener("close", () => {
    for (const request of pending.values()) {
      clearTimeout(request.timer);
      request.reject(new Error("DevTools socket closed"));
    }
    pending.clear();
  });
  return {
    async send(method, params = {}) {
      await ready;
      const id = nextId++;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error(`DevTools command timed out: ${method}`));
        }, 45000);
        pending.set(id, { resolve, reject, timer });
        socket.send(JSON.stringify({ id, method, params }));
      });
    },
    close() { socket.close(); },
  };
}

async function evaluate(client, expression) {
  const result = await client.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description || "renderer evaluation failed");
  }
  return result.result?.value;
}

const initial = await targets();
const uiTarget = initial.find((target) => target.type === "page" && target.title === "Phoenix");
if (!uiTarget) throw new Error("Phoenix Chromium UI target is missing");
const ui = cdp(uiTarget.webSocketDebuggerUrl);
await evaluate(ui, `(async () => {
  const deadline = performance.now() + 12000;
  while (performance.now() < deadline) {
    if (window.PhoenixUI?.state?.view && window.PhoenixUI.state.gateway?.token) return true;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("Phoenix gateway state did not become ready");
})()`);
const preflight = await evaluate(ui, `(async () => {
  let frame = false;
  await Promise.race([
    new Promise((resolve) => requestAnimationFrame(() => { frame = true; resolve(); })),
    new Promise((resolve) => setTimeout(resolve, 1000)),
  ]);
  return {
    visibility: document.visibilityState,
    frame,
    conversationReady: Boolean(window.PhoenixConversation),
    gatewayReady: Boolean(window.PhoenixUI?.state?.view && window.PhoenixUI.state.gateway?.token),
    conversationScript: Array.from(document.scripts).find((script) => script.src.includes("conversation.js"))?.src || null,
    shell: window.__PHOENIX_CHROMIUM_SHELL__?.engine || null,
  };
})()`);
console.log(`PHOENIX_CHROMIUM_PREFLIGHT ${JSON.stringify(preflight)}`);
const opened = await evaluate(ui, `Promise.race([
  (async () => {
      await window.PhoenixConversation.openBrowser("phoenix", "login");
      const status = await window.__TAURI__.core.invoke("browser_surface_show", { instance: "agent-phoenix" });
      const gpu = await window.__PHOENIX_CHROMIUM_SHELL__.probe();
      const viewport = document.getElementById("browserViewport");
      return {
        overlayVisible: !document.getElementById("browserOverlay").hidden,
        nativeViewport: viewport.classList.contains("native-surface"),
        nativeError: viewport.dataset.nativeError || null,
        viewportRect: viewport.getBoundingClientRect().toJSON(),
      browserMessage: document.getElementById("browserEmpty").textContent.trim(),
      toast: document.getElementById("toastRegion").textContent.trim(),
      status,
      gpu,
    };
  })(),
  new Promise((_, reject) => setTimeout(() => reject(new Error("openBrowser integration timeout")), 30000)),
])`);
console.log(`PHOENIX_CHROMIUM_OPENED ${JSON.stringify(opened)}`);

const afterOpen = await targets();
const browserTarget = afterOpen.find((target) => target.type === "page" && target.id !== uiTarget.id);
if (!browserTarget) throw new Error("embedded browser target is missing after openBrowser");
const browser = cdp(browserTarget.webSocketDebuggerUrl);
await browser.send("Page.enable");
await browser.send("Page.navigate", { url: new URL("../ui/relay.html", import.meta.url).href });
await evaluate(browser, `(async () => {
  const deadline = performance.now() + 5000;
  while (performance.now() < deadline) {
    if (document.readyState === "complete") return true;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error("embedded browser probe page did not load");
})()`);
await evaluate(browser, `(() => {
  document.open();
  document.write('<!doctype html><meta charset="utf-8"><title>Phoenix embedded browser live</title><style>*{box-sizing:border-box}html,body{min-height:100%;margin:0}body{background:#f4f1eb;color:#1d1c1a;font:15px system-ui,sans-serif}.shell{min-height:100vh;padding:22px}.top{display:flex;align-items:center;justify-content:space-between}.brand{font-weight:700}.badge{padding:7px 10px;border-radius:999px;background:#e9e2d8;font-size:12px}.hero{margin-top:54px;max-width:520px}.hero small{color:#d75f35;font-weight:700;letter-spacing:.08em;text-transform:uppercase}.hero h1{margin:12px 0 10px;font:700 46px/1.02 system-ui}.hero p{max-width:460px;color:#68645d;line-height:1.55}.cards{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:12px;margin-top:34px}.card{min-height:150px;padding:17px;border:1px solid #ddd6cc;border-radius:17px;background:#fff}.card b{display:block;font-size:18px}.card span{display:block;margin-top:8px;color:#7b766e;line-height:1.45}.proof{margin-top:18px;padding:12px 14px;border-radius:12px;background:#252422;color:#fff;font-weight:650}</style><main class="shell"><header class="top"><span class="brand">Phoenix browser proof</span><span class="badge">Private profile</span></header><section class="hero"><small>Embedded surface</small><h1>The browser stays inside the conversation.</h1><p>This page is rendered by the real managed Chromium target, fitted to Phoenix’s right workspace with its own named tab and navigation controls.</p></section><section class="cards"><article class="card"><b>One surface</b><span>No detached window and no screenshot-stream fallback.</span></article><article class="card"><b>Real interaction</b><span>Clicks, typing, navigation, downloads, and teaching share this viewport.</span></article></section><div class="proof">Real in-app Chromium</div></main>');
  document.close();
  return { title: document.title, text: document.body.innerText };
})()`);
const rendered = await evaluate(browser, `({ title: document.title, text: document.body.innerText, webgpu: Boolean(navigator.gpu) })`);


if (screenshot) {
  const capture = await browser.send("Page.captureScreenshot", { format: "png", fromSurface: true });
  await writeFile(screenshot, Buffer.from(capture.data, "base64"));
}

const report = {
  ok: Boolean(
    opened.overlayVisible
      && opened.nativeViewport
      && opened.status?.embedded
      && opened.status?.engine === "chromium"
      && opened.gpu?.hardwareAcceleration
      && opened.gpu?.renderer?.webgpu
      && rendered?.webgpu
      && rendered?.title === "Phoenix embedded browser live"
  ),
  uiTargetId: uiTarget.id,
  browserTargetId: browserTarget.id,
  opened,
  rendered,
  screenshot: screenshot || null,
};

browser.close();
ui.close();
console.log(`PHOENIX_CHROMIUM_INTEGRATION ${JSON.stringify(report)}`);
if (!report.ok) process.exitCode = 2;
