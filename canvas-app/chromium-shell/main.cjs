"use strict";

const fs = require("node:fs");
const http = require("node:http");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const frontendRecovery = require("./frontend-recovery.cjs");
const rendererResources = require("./renderer-resources.cjs");
const { WebSocketServer, WebSocket } = require("ws");
const { captureScreenshot, prepareInput } = require("./browser-capture.cjs");
const {
  app,
  BaseWindow,
  BrowserWindow,
  WebContentsView,
  dialog,
  ipcMain,
  Menu,
  nativeImage,
  Notification,
  session,
  shell,
  Tray,
  webContents,
} = require("electron");

const UI_ROOT = path.resolve(__dirname, "..", "ui");
const PRELOAD = path.join(__dirname, "preload.cjs");
const SELFTEST = process.env.PHOENIX_CHROMIUM_SELFTEST === "1";
const CONVERSATION_URL = SELFTEST ? null : require("./conversation-entry.cjs").validate(process.env.PHOENIX_CHROMIUM_CONVERSATION_URL);
// The alternate conversation shell needs native services without a second chat renderer.
const SERVICES_ONLY = !SELFTEST && !CONVERSATION_URL && process.env.PHOENIX_CHROMIUM_SERVICES_ONLY === "1";
// Self-test runs beside the user's live Phoenix process. Giving it its own
// debugger port keeps the proof run isolated instead of logging a misleading
// "address already in use" error against the healthy live shell.
const REMOTE_DEBUGGING_PORT = String(process.env.PHOENIX_CHROMIUM_DEBUG_PORT || (SELFTEST ? "17452" : "17442"));
const BRIDGE_PORT = Number(process.env.PHOENIX_CHROMIUM_BRIDGE_PORT || "17443");
const BRIDGE_TOKEN = String(process.env.PHOENIX_CHROMIUM_BRIDGE_TOKEN || "");
const RUST_PARENT_PID = Number(process.env.PHOENIX_RUST_PARENT_PID || "0");
const SHOW_DELAY_MS = Math.max(0, Number(process.env.PHOENIX_CHROMIUM_SHOW_DELAY_MS || "500"));
const OZONE_PLATFORM = String(
  process.env.PHOENIX_CHROMIUM_OZONE_PLATFORM
    || (process.env.WAYLAND_DISPLAY ? "wayland" : "x11"),
).trim().toLowerCase();
const REQUESTED_GPU_MODE = String(
  process.env.PHOENIX_CHROMIUM_GPU_MODE
    || "default",
).trim().toLowerCase();
const WAYLAND_VULKAN_DOWNGRADED = OZONE_PLATFORM === "wayland" && REQUESTED_GPU_MODE === "vulkan";
const GPU_MODE = WAYLAND_VULKAN_DOWNGRADED ? "default" : REQUESTED_GPU_MODE;
const GPU_POWER = String(process.env.PHOENIX_CHROMIUM_GPU_POWER || (SELFTEST ? "high" : "low"))
  .trim()
  .toLowerCase();
const surfaces = new Map();
// Phoenix's resolved theme colour, pushed over the bridge on every theme change.
// Browser tabs paint with it so a blank tab never flashes white in dark mode.
let surfaceBackground = "#ffffff";
let mainWindow = null;
let captureWindow = null;
// Closing the window keeps Phoenix (and every agent's browser) running in the
// system tray. Only "Quit Phoenix" ends the app, stops the gateway daemon, and
// drops the unlocked Passes key. PHOENIX_CLOSE_TO_TRAY=0 restores quit-on-close.
const CLOSE_TO_TRAY = !SELFTEST && process.env.PHOENIX_CLOSE_TO_TRAY !== "0";
let fullQuitRequested = false;
let tray = null;
let trayHintShown = false;
let bridgeServer = null;
let rustSocket = null;
let nextBridgeRequest = 1;
const bridgeRequests = new Map();

function bootLog(message) {
  process.stderr.write(`phoenix chromium: ${message}\n`);
}

bootLog(`launch policy ozone=${OZONE_PLATFORM} gpu=${GPU_MODE}`);
if (WAYLAND_VULKAN_DOWNGRADED) {
  bootLog("Wayland shell selected the compatible default ANGLE/OpenGL path instead of Vulkan");
}

function revealMainWindow(source = "unknown") {
  if (SELFTEST || SERVICES_ONLY || !mainWindow || mainWindow.isDestroyed()) {
    bootLog(`reveal skipped (${source})`);
    return;
  }
  bootLog(`revealing Phoenix window (${source}); visible=${mainWindow.isVisible()}`);
  if (mainWindow.isMinimized()) mainWindow.restore();
  mainWindow.show();
  mainWindow.focus();
  bootLog(`reveal returned (${source}); visible=${mainWindow.isVisible()} focused=${mainWindow.isFocused()}`);
}

app.setName("Phoenix");
// Match the Rust gateway's selected home, including private acceptance profiles.
const PHOENIX_HOME = String(process.env.PHOENIX_HOME || "").trim() || path.join(app.getPath("home"), ".phoenix");
const browserState = require("./browser-state.cjs").create({ phoenixHome: PHOENIX_HOME });
const browserBlocking = require("./browser-blocking.cjs").create({ phoenixHome: PHOENIX_HOME });
const shellData = path.join(PHOENIX_HOME, SELFTEST ? "chromium-shell-selftest" : "chromium-shell");
fs.mkdirSync(shellData, { recursive: true, mode: 0o700 });
app.setPath("userData", shellData);
app.setPath("sessionData", shellData);
const PRIMARY_INSTANCE = app.requestSingleInstanceLock();
if (!PRIMARY_INSTANCE) app.quit();
app.on("second-instance", () => revealMainWindow("second-instance"));
app.commandLine.appendSwitch("remote-debugging-port", REMOTE_DEBUGGING_PORT);
app.commandLine.appendSwitch("remote-allow-origins", `http://127.0.0.1:${REMOTE_DEBUGGING_PORT}`);
// Rust selects this from the real desktop session before the hidden WebKit
// relay applies its own X11 workaround. The visible shell and its Chromium
// WebContentsViews therefore stay native to GNOME Wayland.
if (process.platform === "linux") app.commandLine.appendSwitch("ozone-platform", OZONE_PLATFORM);
if (process.platform === "linux" && GPU_MODE === "vulkan") {
  app.commandLine.appendSwitch("use-gl", "angle");
  app.commandLine.appendSwitch("use-angle", "vulkan");
  app.commandLine.appendSwitch("enable-features", "Vulkan,DefaultANGLEVulkan,VulkanFromANGLE");
}
// Chromium's GPU sandbox blocks the distro NVIDIA GBM plugin from being
// dlopened on this X11 stack, which crashes the GPU process and silently drops
// the whole app to software compositing. Renderer processes remain sandboxed;
// relax only the isolated GPU helper when Rust positively identified NVIDIA.
if (process.platform === "linux" && OZONE_PLATFORM === "x11" && process.env.PHOENIX_CHROMIUM_NVIDIA === "1") {
  app.commandLine.appendSwitch("disable-gpu-sandbox");
}
// The visible shell is an always-on desktop surface, so prefer the low-power
// adapter unless a benchmark or diagnostic explicitly opts into `high`.
// Self-test stays high-performance because it verifies hardware WebGPU.
app.commandLine.appendSwitch(GPU_POWER === "high" ? "force_high_performance_gpu" : "force_low_power_gpu");
app.commandLine.appendSwitch("disable-renderer-backgrounding");

function normalizeInstance(value) {
  const instance = String(value || "agent-phoenix").trim() || "agent-phoenix";
  if (instance.length > 128 || !/^[a-zA-Z0-9_-]+$/.test(instance)) {
    throw new Error("invalid browser instance id");
  }
  return instance;
}

const zoomControl = { apply: null };
function zoomedRect(rect) {
  const zoom = mainWindow && !mainWindow.isDestroyed() ? mainWindow.webContents.getZoomFactor() : 1;
  if (!rect || Math.abs(zoom - 1) < 0.001) return rect;
  return { x: Math.round(rect.x * zoom), y: Math.round(rect.y * zoom), width: Math.max(1, Math.round(rect.width * zoom)), height: Math.max(1, Math.round(rect.height * zoom)) };
}
function normalizeRect(value) {
  const rect = value || {};
  const numbers = [rect.x, rect.y, rect.width, rect.height].map(Number);
  if (!numbers.every(Number.isFinite)) throw new Error("browser surface bounds must be finite");
  const [x, y, width, height] = numbers;
  if (x < 0 || y < 0 || width < 64 || height < 64 || width > 16384 || height > 16384) {
    throw new Error("browser surface bounds are outside the Phoenix window");
  }
  return {
    x: Math.round(x),
    y: Math.round(y),
    width: Math.round(width),
    height: Math.round(height),
  };
}

function surfaceStatus(entry, reason = null) {
  const tabs = entry ? Array.from(entry.tabs.values()).map((tab) => ({
    id: tab.targetId,
    title: tab.view.webContents.getTitle() || "New tab",
    url: tab.view.webContents.getURL() || "about:blank",
    active: tab.targetId === entry.activeTargetId,
    // The page's declared icon. Guessing `${origin}/favicon.ico` only works for
    // the minority of sites that still serve that path; Chromium already
    // resolves <link rel="icon"> for us, so report what it found.
    favicon: tab.favicon || "",
  })) : [];
  return {
    supported: true,
    embedded: Boolean(entry?.mounted),
    visible: Boolean(entry?.mounted),
    fallback: false,
    reason,
    engine: "chromium",
    targetId: entry?.activeTargetId || null,
    tabs,
  };
}

function emit(name, payload) {
  if (!mainWindow?.isDestroyed()) mainWindow.webContents.send("phoenix:event", name, payload);
}

function rejectBridgeRequests(message, socket = null) {
  for (const [id, request] of bridgeRequests) {
    if (socket && request.socket !== socket) continue;
    bridgeRequests.delete(id);
    clearTimeout(request.timer);
    request.reject(new Error(message));
  }
}

async function waitForRustSocket() {
  if (rustSocket?.readyState === WebSocket.OPEN) return rustSocket;
  if (SELFTEST) throw new Error("Phoenix native-services bridge is unavailable in self-test mode");
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 40));
    if (rustSocket?.readyState === WebSocket.OPEN) return rustSocket;
  }
  throw new Error("Phoenix native-services bridge did not connect during startup");
}

async function invokeRust(command, args) {
  const socket = await waitForRustSocket();
  const id = nextBridgeRequest++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      bridgeRequests.delete(id);
      reject(new Error(`native command '${command}' timed out`));
    }, 5 * 60 * 1000);
    const request = { resolve, reject, timer, socket };
    bridgeRequests.set(id, request);
    const failedSend = (error) => {
      if (!error || bridgeRequests.get(id) !== request) return;
      bridgeRequests.delete(id);
      clearTimeout(timer);
      reject(error);
    };
    try {
      socket.send(JSON.stringify({ type: "invoke", id, command, args }), failedSend);
    } catch (error) {
      failedSend(error);
    }
  });
}

function bridgeAuthorized(request) {
  if (!BRIDGE_TOKEN || BRIDGE_TOKEN.length < 24) return false;
  const url = new URL(request.url || "/", `http://127.0.0.1:${BRIDGE_PORT}`);
  return url.searchParams.get("token") === BRIDGE_TOKEN;
}

async function readJsonBody(request) {
  let total = 0;
  const chunks = [];
  for await (const chunk of request) {
    total += chunk.length;
    if (total > 64 * 1024) throw new Error("bridge request is too large");
    chunks.push(chunk);
  }
  if (!chunks.length) return {};
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function writeJson(response, status, value) {
  const body = Buffer.from(JSON.stringify(value));
  response.writeHead(status, {
    "content-type": "application/json",
    "content-length": body.length,
    "cache-control": "no-store",
  });
  response.end(body);
}

function startBridgeServer() {
  if (!BRIDGE_TOKEN) return;
  bridgeServer = http.createServer(async (request, response) => {
    try {
      if (!bridgeAuthorized(request)) return writeJson(response, 403, { error: "forbidden" });
      const url = new URL(request.url || "/", `http://127.0.0.1:${BRIDGE_PORT}`);
      if (request.method === "GET" && url.pathname === "/health") {
        return writeJson(response, 200, {
          ok: true,
          engine: process.versions.chrome,
          rustConnected: Boolean(rustSocket && rustSocket.readyState === WebSocket.OPEN),
        });
      }
      if (request.method === "POST" && url.pathname === "/browser/open") {
        const body = await readJsonBody(request);
        const entry = createSurface(body.instance, body.profileOwner);
        return writeJson(response, 200, {
          ok: true,
          instance: entry.instance,
          targetId: entry.activeTargetId,
          debuggerPort: Number(REMOTE_DEBUGGING_PORT),
          mounted: entry.mounted,
          tabs: surfaceStatus(entry).tabs,
        });
      }
      if (request.method === "POST" && url.pathname === "/browser/status") {
        const body = await readJsonBody(request);
        const entry = surfaces.get(normalizeInstance(body.instance));
        return writeJson(response, 200, surfaceStatus(entry, entry ? null : "browser surface is not open"));
      }
      if (request.method === "POST" && url.pathname === "/browser/capture") {
        const body = await readJsonBody(request);
        if (typeof body.instance !== "string" || !body.instance || typeof body.targetId !== "string" || !body.targetId) {
          throw new Error("Browser capture requires its instance and exact target ID");
        }
        if (body.fullPage !== undefined && typeof body.fullPage !== "boolean") throw new Error("fullPage must be a boolean");
        const entry = surfaces.get(normalizeInstance(body.instance));
        if (!entry) throw new Error("Browser surface is not open");
        const bytes = await captureSurfaceTab(entry, body.targetId, body.fullPage === true, body.clip ?? null);
        response.writeHead(200, {
          "content-type": "image/png", "content-length": bytes.length, "cache-control": "no-store",
          "x-phoenix-instance": entry.instance, "x-phoenix-target-id": body.targetId,
        });
        return response.end(bytes);
      }
      if (request.method === "POST" && url.pathname === "/browser/prepare-input") {
        const body = await readJsonBody(request);
        if (typeof body.instance !== "string" || !body.instance || typeof body.targetId !== "string"
            || !body.targetId || body.targetId.length > 128) throw new Error("Browser input requires its instance and exact target ID");
        const entry = surfaces.get(normalizeInstance(body.instance));
        const tab = entry?.tabs.get(body.targetId);
        if (!tab || tab.view.webContents.isDestroyed()) throw new Error("Browser input target is no longer open in this surface");
        // Typing goes to the agent's own tab. Refusing whenever another tab of
        // the same surface was in front left the agent no way to type at all,
        // so it fell back to script-set values that real sites ignore.
        if (entry.activeTargetId !== body.targetId) activateSurfaceTab(entry, body.targetId);
        const revision = tab.inputRevision;
        if (!entry.mounted) await prepareInput(tab.view.webContents, drawableLease(entry, tab));
        if (surfaces.get(entry.instance) !== entry || entry.tabs.get(body.targetId) !== tab
            || entry.activeTargetId !== body.targetId || tab.view.webContents.isDestroyed()
            || tab.inputRevision !== revision) throw new Error("Browser input target changed during preparation; observe it again");
        return writeJson(response, 200, { ready: true, instance: entry.instance, targetId: body.targetId });
      }
      if (request.method === "POST" && url.pathname === "/browser/new-tab") {
        const body = await readJsonBody(request);
        const existing = surfaces.get(normalizeInstance(body.instance));
        const alreadyOpen = Boolean(existing && activeSurfaceTab(existing) && !activeSurfaceTab(existing).view.webContents.isDestroyed());
        const entry = createSurface(body.instance);
        // Creating a surface already creates its first tab. Reuse that tab on
        // the first New tab request so an unopened browser cannot become two
        // blank tabs in one click.
        const tab = alreadyOpen
          ? createSurfaceTab(entry, body.url || "about:blank", true)
          : activeSurfaceTab(entry);
        if (!alreadyOpen && body.url && body.url !== "about:blank") {
          await tab.view.webContents.loadURL(body.url);
        }
        return writeJson(response, 200, { ...surfaceStatus(entry), targetId: tab.targetId });
      }
      if (request.method === "POST" && url.pathname === "/browser/switch-tab") {
        const body = await readJsonBody(request);
        const entry = surfaces.get(normalizeInstance(body.instance));
        if (!entry) throw new Error("browser surface is not open");
        activateSurfaceTab(entry, String(body.targetId || ""));
        return writeJson(response, 200, surfaceStatus(entry));
      }
      if (request.method === "POST" && url.pathname === "/browser/close-tab") {
        const body = await readJsonBody(request);
        const entry = surfaces.get(normalizeInstance(body.instance));
        if (!entry) throw new Error("browser surface is not open");
        closeSurfaceTab(entry, String(body.targetId || ""));
        return writeJson(response, 200, surfaceStatus(entry));
      }
      if (request.method === "POST" && url.pathname === "/browser/close") {
        const body = await readJsonBody(request);
        const entry = surfaces.get(normalizeInstance(body.instance));
        if (entry) unmountSurface(entry);
        return writeJson(response, 200, { ok: true });
      }
      if (request.method === "POST" && url.pathname === "/browser/discard") {
        const body = await readJsonBody(request);
        const key = normalizeInstance(body.instance);
        if (!key.startsWith("volume-worker-volume-") && !/^agent-.+-job-/.test(key)) {
          throw new Error("only disposable worker browser surfaces may be discarded");
        }
        const entry = surfaces.get(key);
        if (entry) destroySurface(entry);
        return writeJson(response, 200, { ok: true });
      }
      return writeJson(response, 404, { error: "not found" });
    } catch (error) {
      return writeJson(response, 400, { error: String(error?.message || error) });
    }
  });
  // Native session/context responses are capped at 32 MiB in Rust. Allow that
  // payload plus JSON framing without making the authenticated bridge unbounded.
  const webSockets = new WebSocketServer({ noServer: true, maxPayload: 40 * 1024 * 1024 });
  bridgeServer.on("upgrade", (request, socket, head) => {
    if (!bridgeAuthorized(request)) return socket.destroy();
    webSockets.handleUpgrade(request, socket, head, (webSocket) => {
      webSockets.emit("connection", webSocket, request);
    });
  });
  webSockets.on("connection", (socket) => {
    const previous = rustSocket;
    rustSocket = socket;
    if (previous && previous !== socket) {
      // Sent native mutations may already have taken effect. Fail their
      // receipts explicitly; never replay them against the replacement.
      rejectBridgeRequests("Phoenix native-services connection was replaced; completion of sent commands is unconfirmed", previous);
      if (previous.readyState === WebSocket.OPEN) previous.close(1000, "replaced");
    }
    socket.on("message", (raw) => {
      if (rustSocket !== socket) return;
      try {
        const message = JSON.parse(String(raw));
        if (message.type === "result") {
          const request = bridgeRequests.get(message.id);
          if (!request || request.socket !== socket) return;
          bridgeRequests.delete(message.id);
          clearTimeout(request.timer);
          if (message.ok) request.resolve(message.value);
          else request.reject(new Error(String(message.error || "native command failed")));
        } else if (message.type === "event") {
          emit(String(message.name || ""), message.payload);
        }
      } catch (error) {
        process.stderr.write(`phoenix chromium bridge: ${error.message}\n`);
      }
    });
    socket.on("close", () => {
      if (rustSocket === socket) rustSocket = null;
      rejectBridgeRequests("Phoenix native-services bridge disconnected; completion of sent commands is unconfirmed", socket);
    });
    socket.on("error", (error) => {
      process.stderr.write(`phoenix chromium relay: ${error.message}\n`);
    });
    socket.send(JSON.stringify({ type: "ready", engine: process.versions.chrome }));
  });
  bridgeServer.listen(BRIDGE_PORT, "127.0.0.1", () => {
    bootLog(`bridge listening on loopback port ${BRIDGE_PORT}`);
  });
  bridgeServer.on("error", (error) => {
    process.stderr.write(`phoenix chromium bridge failed: ${error.message}\n`);
    app.exit(3);
  });
}

function createSurface(instance, requestedProfileOwner = null) {
  const key = normalizeInstance(instance);
  const existing = surfaces.get(key);
  const requestedOwner = requestedProfileOwner == null
    ? null
    : normalizeInstance(requestedProfileOwner);
  if (existing && activeSurfaceTab(existing) && !activeSurfaceTab(existing).view.webContents.isDestroyed()) {
    if (requestedOwner && existing.profileOwner !== requestedOwner) {
      throw new Error("browser authentication owner cannot change while the worker surface is open");
    }
    return existing;
  }
  const entry = {
    instance: key,
    profileOwner: requestedOwner || existing?.profileOwner || key,
    tabs: new Map(),
    activeTargetId: null,
    rect: { x: 0, y: 0, width: 800, height: 600 },
    mounted: false,
    downloadBinding: null,
  };
  surfaces.set(key, entry);
  // Continue where you left off: reopen this browser's saved tabs.
  const saved = browserState.savedTabs(key);
  if (saved) {
    const opened = saved.tabs.map((tab, index) => createSurfaceTab(entry, tab.url, index === saved.active));
    if (!entry.activeTargetId && opened[0]) activateSurfaceTab(entry, opened[0].targetId);
  } else {
    createSurfaceTab(entry, "about:blank", true);
  }
  return entry;
}

function activeSurfaceTab(entry) {
  return entry?.tabs.get(entry.activeTargetId) || null;
}

function renderHost(rect) {
  if (!captureWindow || captureWindow.isDestroyed()) {
    const host = new BaseWindow({ width: rect.width, height: rect.height,
      show: false, focusable: false, skipTaskbar: true });
    captureWindow = host;
    host.on("closed", () => { if (captureWindow === host) captureWindow = null; });
  }
  const [width, height] = captureWindow.getContentSize();
  if (width < rect.width || height < rect.height) {
    captureWindow.setContentSize(Math.max(width, rect.width), Math.max(height, rect.height));
  }
  return captureWindow;
}

function detachSurfaceTab(tab) {
  if (tab.host && !tab.host.isDestroyed()) tab.host.contentView.removeChildView(tab.view);
  tab.host = null;
}

function attachSurfaceTab(tab, host, rect, invalidateInput = true) {
  const previous=tab.view.getBounds();
  // Identical status-tick bounds do not change the input target. A capture
  // lease also restores its own temporary layout without invalidating input.
  if (invalidateInput && (tab.host !== host || tab.leased || ["x","y","width","height"].some(key=>previous[key]!==rect[key]))) tab.inputRevision++;
  tab.leased = false;
  if (tab.host !== host) {
    detachSurfaceTab(tab);
    tab.view.setBounds(rect);
    host.contentView.addChildView(tab.view);
    tab.host = host;
  }
  tab.view.setBounds(rect);
}

function parkSurfaceTab(tab, rect, invalidateInput = true) {
  // Hidden or unparented native views cannot provide compositor frames even
  // with background throttling disabled. Keep a drawable view in a window
  // that is never shown or focused; captures pump frames only while needed.
  attachSurfaceTab(tab, renderHost(rect), { x: 0, y: 0, width: rect.width, height: rect.height }, invalidateInput);
}

// A parked tab's hidden host window never draws on Wayland, so captures of it
// timed out. For one capture the tab borrows a 1x1 spot in the visible Phoenix
// window while CDP emulation keeps the page at its real size.
function drawableLease(entry, tab) {
  if (tab.host === mainWindow || !mainWindow || mainWindow.isDestroyed() || !mainWindow.isVisible()) return {};
  const { width, height } = tab.view.getBounds();
  return {
    viewport: { width: Math.max(width, 320), height: Math.max(height, 240) },
    lease: () => {
      detachSurfaceTab(tab);
      mainWindow.contentView.addChildView(tab.view, 0);
      tab.view.setBounds({ x: 0, y: 0, width: 1, height: 1 });
      tab.host = mainWindow;
      tab.leased = true;
      return () => {
        // Mounting the tab during the capture ends the lease; leave it shown.
        if (!tab.leased || tab.view.webContents.isDestroyed()) return;
        parkSurfaceTab(tab, entry.rect, false);
      };
    },
  };
}

async function captureSurfaceTab(entry, targetId, fullPage = false, clipOverride = null) {
  const tab = entry.tabs.get(targetId);
  if (!tab || tab.view.webContents.isDestroyed()) throw new Error("Browser capture target is not owned by this surface");
  const bytes = await captureScreenshot(tab.view.webContents, { fullPage, clipOverride, ...drawableLease(entry, tab) });
  if (surfaces.get(entry.instance) !== entry || entry.tabs.get(targetId) !== tab || tab.view.webContents.isDestroyed()) {
    throw new Error("Browser capture target closed before completion");
  }
  if (bytes.length < 33 || bytes.toString("ascii", 12, 16) !== "IHDR"
      || bytes.readUInt32BE(16) * bytes.readUInt32BE(20) > 32 * 1024 * 1024) {
    throw new Error("Browser screenshot exceeds the supported image dimensions");
  }
  const image = nativeImage.createFromBuffer(bytes);
  if (image.isEmpty()) throw new Error("Browser page returned no rendered PNG frame");
  const size = image.getSize();
  if (size.width * size.height > 32 * 1024 * 1024) throw new Error("Browser screenshot exceeds the supported image dimensions");
  return bytes;
}

function createSurfaceTab(entry, initialUrl = "about:blank", activate = true) {
  const view = new WebContentsView({
    webPreferences: {
      // `instance` isolates tabs/lifecycle. `profileOwner` deliberately does
      // not: a worker spawned by Avery gets a different WebContentsView in
      // Avery's exact persistent browser session, including login state.
      partition: `persist:phoenix-${entry.profileOwner}`,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: true,
      backgroundThrottling: false,
      focusOnNavigation: false,
    },
  });
  view.setBackgroundColor(surfaceBackground);
  // Match the 12px rounded page frame the UI draws around #browserViewport.
  if (typeof view.setBorderRadius === "function") view.setBorderRadius(12);
  browserState.ensureExtensions(view.webContents.session);
  browserBlocking.attach(view.webContents.session).catch(error => console.warn("[phoenix] ad blocking:", error.message));
  const tab = { view, targetId: view.webContents.getOrCreateDevToolsTargetId(), host: null, inputRevision: 0 };
  entry.tabs.set(tab.targetId, tab);
  parkSurfaceTab(tab, entry.rect);

  const downloadRoot = path.join(PHOENIX_HOME, "downloads", entry.instance);
  fs.mkdirSync(downloadRoot, { recursive: true });
  if (!entry.downloadBinding) {
    const session = view.webContents.session;
    const listener = (_event, item, origin) => {
      // A parent and its workers share cookies, but own different downloads.
      // Session events reach every listener, so route by the originating tab.
      if (![...entry.tabs.values()].some((tab) => tab.view.webContents === origin)) return;
      const filename = path.basename(item.getFilename()).replace(/[\u0000-\u001f]/g, "_");
      item.setSavePath(path.join(downloadRoot, filename || "download"));
    };
    session.on("will-download", listener);
    entry.downloadBinding = { session, listener };
  }

  view.webContents.setWindowOpenHandler(({ url }) => {
    // Page-authored popups become real background tabs. Keeping the current
    // tab active prevents the visible WebContentsView from outrunning the
    // gateway's working CDP target; selecting the new tab in Phoenix performs
    // one correlated shell+gateway switch.
    if (/^(?:https?:|about:)/i.test(url)) setImmediate(() => createSurfaceTab(entry, url, false));
    return { action: "deny" };
  });
  view.webContents.on("did-navigate", (_event, url) => {
    emit("browser-location", { instance: entry.instance, targetId: tab.targetId, url, title: view.webContents.getTitle() });
    browserState.scheduleSave(entry);
  });
  view.webContents.on("did-navigate-in-page", (_event, _url, isMainFrame) => { if (isMainFrame) browserState.scheduleSave(entry); });
  view.webContents.on("page-title-updated", (_event, title) => {
    emit("browser-title", { instance: entry.instance, targetId: tab.targetId, url: view.webContents.getURL(), title });
  });
  view.webContents.on("page-favicon-updated", (_event, favicons) => {
    tab.favicon = Array.isArray(favicons) && favicons.length ? String(favicons[0]) : "";
    emit("browser-favicon", { instance: entry.instance, targetId: tab.targetId, favicon: tab.favicon });
  });
  // A new document may have no icon at all; clear the previous page's so a tab
  // never keeps showing the icon of a site it has navigated away from.
  view.webContents.on("did-start-navigation", (_event, _url, _isInPlace, isMainFrame) => {
    if (isMainFrame) { tab.favicon = ""; tab.inputRevision++; }
  });
  view.webContents.on("render-process-gone", (_event, details) => {
    emit("browser-renderer-gone", { instance: entry.instance, targetId: tab.targetId, ...details });
  });
  view.webContents.loadURL(initialUrl);
  if (activate) activateSurfaceTab(entry, tab.targetId);
  else emit("browser-tab-created", { instance: entry.instance, targetId: tab.targetId, tabs: surfaceStatus(entry).tabs });
  return tab;
}

function activateSurfaceTab(entry, targetId) {
  const next = entry.tabs.get(targetId);
  if (!next || next.view.webContents.isDestroyed()) throw new Error("browser tab is no longer open");
  const previous = activeSurfaceTab(entry);
  const mounted = entry.mounted;
  if (mounted && previous && previous !== next && mainWindow && !mainWindow.isDestroyed()) {
    parkSurfaceTab(previous, entry.rect);
  }
  entry.activeTargetId = targetId;
  if (mounted && previous !== next && mainWindow && !mainWindow.isDestroyed()) {
    attachSurfaceTab(next, mainWindow, entry.rect);
  }
  if (mounted) next.view.setBounds(entry.rect);
  emit("browser-tab-activated", { instance: entry.instance, targetId, tabs: surfaceStatus(entry).tabs });
  browserState.scheduleSave(entry);
  return next;
}

function closeSurfaceTab(entry, targetId) {
  const closing = entry.tabs.get(targetId);
  if (!closing) throw new Error("browser tab is no longer open");
  if (entry.tabs.size <= 1) throw new Error("the last browser tab cannot be closed");
  const wasActive = entry.activeTargetId === targetId;
  detachSurfaceTab(closing);
  entry.tabs.delete(targetId);
  if (!closing.view.webContents.isDestroyed()) closing.view.webContents.close({ waitForBeforeUnload: false });
  if (wasActive) {
    entry.activeTargetId = entry.tabs.keys().next().value;
    const replacement = activeSurfaceTab(entry);
    if (entry.mounted && replacement && mainWindow && !mainWindow.isDestroyed()) {
      attachSurfaceTab(replacement, mainWindow, entry.rect);
    }
  }
  emit("browser-tab-closed", { instance: entry.instance, targetId, tabs: surfaceStatus(entry).tabs });
  browserState.scheduleSave(entry);
}

function destroySurface(entry) {
  unmountSurface(entry);
  if (entry.downloadBinding) {
    entry.downloadBinding.session.removeListener("will-download", entry.downloadBinding.listener);
    entry.downloadBinding = null;
  }
  for (const tab of entry.tabs.values()) {
    detachSurfaceTab(tab);
    if (!tab.view.webContents.isDestroyed()) {
      tab.view.webContents.close({ waitForBeforeUnload: false });
    }
  }
  entry.tabs.clear();
  entry.activeTargetId = null;
  surfaces.delete(entry.instance);
  if (!surfaces.size && captureWindow && !captureWindow.isDestroyed()) captureWindow.close();
}

function mountSurface(entry) {
  if (!mainWindow || mainWindow.isDestroyed()) throw new Error("Phoenix window is unavailable");
  const tab = activeSurfaceTab(entry);
  if (!tab) throw new Error("browser surface has no open tab");
  // A conversation switch can enqueue the previous hide and the next show
  // within the same renderer frame. Enforce the invariant here as well as in
  // the UI: only one conversation-owned WebContentsView may be mounted, so a
  // late show can never leave Avery's page painting over Phoenix (or vice versa).
  for (const other of surfaces.values()) {
    if (other !== entry && other.mounted) unmountSurface(other);
  }
  if (!entry.mounted) {
    attachSurfaceTab(tab, mainWindow, entry.rect);
    entry.mounted = true;
  }
  tab.view.setBounds(entry.rect);
}

function unmountSurface(entry) {
  if (entry?.mounted) {
    const tab = activeSurfaceTab(entry);
    if (tab) {
      if (mainWindow && !mainWindow.isDestroyed()) parkSurfaceTab(tab, entry.rect);
      else detachSurfaceTab(tab);
    }
    entry.mounted = false;
  }
}

// Runs in an isolated world, so page scripts cannot see or move it; the
// arrow is inline SVG so page image policies cannot block it.
const CURSOR_MOTION_SCRIPT = fs.readFileSync(path.join(__dirname, "cursor-motion.js"), "utf8");
const AGENT_CURSOR_SCRIPT = String(function agentCursor(spec) {
  let host = document.getElementById("__phoenix_agent_cursor");
  // Hiding fades it in place; removing it made the next action re-create it
  // mid-page and slide it across.
  if (!spec.visible) { if (window.__phoenixCursor) { window.__phoenixCursor.cursor.style.opacity = "0"; PhoenixCursorMotion.cancel(window.__phoenixCursor.cursor); } return; }
  if (!host || !window.__phoenixCursor) {
    host?.remove();
    host = document.createElement("div");
    host.id = "__phoenix_agent_cursor";
    host.style.cssText = "position:fixed;left:0;top:0;width:0;height:0;z-index:2147483647;pointer-events:none";
    const root = host.attachShadow({ mode: "closed" });
    root.innerHTML = '<style>.c{position:fixed;left:50vw;top:50vh;width:22px;height:30px;pointer-events:none;transition:opacity .15s ease;filter:drop-shadow(0 0 6px rgba(255,110,20,.55))}.r{position:absolute;left:-5px;top:-5px;width:10px;height:10px;border:2px solid #ff6e14;border-radius:50%;opacity:0}.r.on{animation:p .42s ease-out}@keyframes p{0%{opacity:1;transform:scale(.2)}to{opacity:0;transform:scale(2.7)}}</style><div class="c"><svg viewBox="0 0 22 30" width="22" height="30"><path d="M2 2L2 24L8 18L12 28L16 26L12 16L20 16Z" fill="#161616" stroke="#ff6e14" stroke-width="1.8" stroke-linejoin="round"/></svg><i class="r"></i></div>';
    window.__phoenixCursor = { cursor: root.querySelector(".c"), ring: root.querySelector(".r") };
    document.documentElement.appendChild(host);
  }
  const { cursor, ring } = window.__phoenixCursor;
  cursor.style.opacity = "1";
  const reduced=spec.reduced||matchMedia("(prefers-reduced-motion:reduce)").matches;
  ring.classList.remove("on");
  PhoenixCursorMotion.animate(cursor,{x:spec.x/100*innerWidth,y:spec.y/100*innerHeight},{style:spec.motion,width:innerWidth,height:innerHeight,reduced,onComplete:()=>{if(spec.click&&!reduced){void ring.offsetWidth;ring.classList.add("on");}}});
});

async function browserInvoke(command, args) {
  if (command === "browser_extension_blocking_status") return browserBlocking.status();
  if (command === "browser_extension_blocking_set") return browserBlocking.setEnabled(args.enabled);
  if (command === "browser_extensions_list") return browserState.installed().map(({ path: _path, ...item }) => item);
  if (command === "browser_extension_set_enabled") { await browserState.setEnabled(String(args.id || ""), Boolean(args.enabled)); return browserState.installed().map(({ path: _path, ...item }) => item); }
  if (command === "browser_extension_remove") { await browserState.remove(String(args.id || "")); return browserState.installed().map(({ path: _path, ...item }) => item); }
  if (command === "browser_extension_open") {
    const entry = surfaces.get(normalizeInstance(args.instance)), tab = entry && activeSurfaceTab(entry);
    if (!tab) throw new Error("open this conversation's browser first");
    return browserState.openPopup(String(args.id || ""), tab.view.webContents.session, mainWindow);
  }
  const key = normalizeInstance(args.instance);
  if (command === "browser_surface_attach") {
    const entry = createSurface(key);
    entry.cssRect = normalizeRect(args.rect);
    entry.rect = zoomedRect(entry.cssRect);
    mountSurface(entry);
    return surfaceStatus(entry);
  }
  const entry = surfaces.get(key);
  // `restorable`: tabs saved from an earlier run are waiting to reopen.
  if (!entry) return { ...surfaceStatus(null, "browser surface is not open"), restorable: Boolean(browserState.savedTabs(key)) };
  // The agent's cursor, drawn inside the page: the native view covers the
  // Phoenix UI, so a cursor in the UI only showed while the page was blank.
  if (command === "browser_surface_cursor") {
    const tab = activeSurfaceTab(entry);
    if (entry.mounted && tab && !tab.view.webContents.isDestroyed()) {
      const coordinate=value=>Number.isFinite(Number(value))?Math.max(0,Math.min(100,Number(value))):50;
      const spec = { x: coordinate(args.x), y: coordinate(args.y), click: Boolean(args.click), visible: args.visible !== false, motion:String(args.motion||"signature_arc"), reduced:Boolean(args.reduced) };
      tab.view.webContents.executeJavaScriptInIsolatedWorld(1337, [{ code: `if(!globalThis.PhoenixCursorMotion){${CURSOR_MOTION_SCRIPT}};(${AGENT_CURSOR_SCRIPT})(${JSON.stringify(spec)})` }]).catch(() => {});
    }
    return { ok: true };
  }
  // Toolbar back / forward / reload act on the tab the user is looking at.
  // Routing them through the agent's browser connection hit the agent's
  // current tab instead (reload jumped to another tab and did nothing).
  if (command === "browser_surface_nav") {
    const tab = args.targetId ? entry.tabs.get(String(args.targetId)) : activeSurfaceTab(entry);
    const contents = tab?.view.webContents;
    if (!contents || contents.isDestroyed()) throw new Error("browser surface has no open tab");
    const action = String(args.action || "");
    if (action === "navigate") {
      const url = String(args.url || "").trim();
      if (!url) throw new Error("navigation requires a URL");
      await contents.loadURL(url);
    } else if (action === "reload") contents.reload();
    else if (action === "back" && contents.navigationHistory.canGoBack()) contents.navigationHistory.goBack();
    else if (action === "forward" && contents.navigationHistory.canGoForward()) contents.navigationHistory.goForward();
    else if (!["back", "forward"].includes(action)) throw new Error("unknown browser navigation action");
    return surfaceStatus(entry);
  }
  if (command === "browser_surface_screenshot") {
    const root = path.join(PHOENIX_HOME, "downloads", key);
    fs.mkdirSync(root, { recursive: true });
    const stamp = new Date().toISOString().replace(/[:.]/g, "-");
    const output = path.join(root, `Phoenix screenshot ${stamp}.png`);
    const tab = activeSurfaceTab(entry);
    if (!tab) throw new Error("browser surface has no open tab");
    const bytes = await captureSurfaceTab(entry, tab.targetId);
    fs.writeFileSync(output, bytes);
    return { supported: true, path: output };
  }
  if (command === "browser_surface_set_bounds") {
    // The renderer measures in CSS pixels; with interface zoom the native
    // view needs window pixels.
    entry.cssRect = normalizeRect(args.rect);
    entry.rect = zoomedRect(entry.cssRect);
    if (entry.mounted) activeSurfaceTab(entry)?.view.setBounds(entry.rect);
    else if (activeSurfaceTab(entry)) parkSurfaceTab(activeSurfaceTab(entry), entry.rect);
  } else if (command === "browser_surface_new_tab") {
    createSurfaceTab(entry, args.url || "about:blank", true);
  } else if (command === "browser_surface_switch_tab") {
    activateSurfaceTab(entry, String(args.targetId || args.tabId || ""));
  } else if (command === "browser_surface_close_tab") {
    closeSurfaceTab(entry, String(args.targetId || args.tabId || ""));
  } else if (command === "browser_surface_show") {
    mountSurface(entry);
  } else if (command === "browser_surface_hide" || command === "browser_surface_detach") {
    unmountSurface(entry);
  }
  return surfaceStatus(entry);
}

async function invoke(command, args = {}) {
  if (command.startsWith("browser_surface_") || command.startsWith("browser_extension")) return browserInvoke(command, args);
  if (command === "app_zoom") {
    if (!mainWindow || !zoomControl.apply) return null;
    const step = Number(args.step) || 0;
    zoomControl.apply(step === 0 ? 1 : mainWindow.webContents.getZoomFactor() + step * 0.1);
    return mainWindow.webContents.getZoomFactor();
  }
  if (command === "plugin:window|minimize") return mainWindow?.minimize();
  if (command === "plugin:window|toggle_maximize") {
    if (mainWindow?.isMaximized()) mainWindow.unmaximize(); else mainWindow?.maximize();
    return null;
  }
  if (command === "plugin:window|close") return mainWindow?.close();
  if (command === "plugin:window|set_background_color") {
    // Phoenix sends its resolved theme colour here. Remember it so a blank or
    // still-loading browser tab paints in the app's colour instead of flashing
    // white, and repaint the tabs that already exist.
    surfaceBackground = String(args.color || "#ffffff");
    mainWindow?.setBackgroundColor(surfaceBackground);
    for (const entry of surfaces.values()) {
      for (const tab of entry.tabs.values()) {
        if (!tab.view.webContents.isDestroyed()) tab.view.setBackgroundColor(surfaceBackground);
      }
    }
    return null;
  }
  if (command === "plugin:window|set_theme") return null;
  if (command === "open_external") {
    const url = String(args.url || "");
    if (!/^https?:\/\//i.test(url)) throw new Error("only HTTP(S) links may be opened externally");
    await shell.openExternal(url);
    return null;
  }
  return invokeRust(command, args);
}

async function gpuProbe() {
  const renderer = await mainWindow.webContents.executeJavaScript(`(async () => {
    const result = { webgpu: Boolean(navigator.gpu), adapter: null };
    if (navigator.gpu) {
      const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
      result.adapter = adapter ? {
        features: Array.from(adapter.features || []),
        limits: adapter.limits ? { maxTextureDimension2D: adapter.limits.maxTextureDimension2D } : null
      } : null;
    }
    return result;
  })()`);
  return {
    hardwareAcceleration: app.isHardwareAccelerationEnabled(),
    features: app.getGPUFeatureStatus(),
    renderer,
  };
}

async function runSelftest() {
  mainWindow.showInactive();
  await new Promise((resolve) => setTimeout(resolve, 500));
  const entry = createSurface("selftest");
  entry.rect = { x: 120, y: 120, width: 900, height: 620 };
  mountSurface(entry);
  const page = `data:text/html;charset=utf-8,${encodeURIComponent(`<!doctype html>
    <meta charset="utf-8"><title>Embedded Chromium OK</title>
    <style>html,body{height:100%;margin:0}body{display:grid;place-items:center;background:linear-gradient(135deg,#ff815a,#7037a7);color:white;font:700 42px system-ui}</style>
    <main>Embedded Chromium · native pixels</main>`)}`;
  const tab = activeSurfaceTab(entry);
  await tab.view.webContents.loadURL(page);
  const firstTargetId = tab.targetId;
  const second = createSurfaceTab(entry, "about:blank", false);
  await second.view.webContents.loadURL("data:text/html,<title>Second embedded tab</title>");
  const backgroundCreated = entry.tabs.size === 2 && entry.activeTargetId === firstTargetId;
  activateSurfaceTab(entry, second.targetId);
  const switched = entry.activeTargetId === second.targetId && activeSurfaceTab(entry) === second;
  const navigationPage = "data:text/html,<title>Address edit reached selected tab</title>";
  await browserInvoke("browser_surface_nav", { instance: "selftest", targetId: second.targetId, action: "navigate", url: navigationPage });
  const selectedNavigationTitle = await second.view.webContents.executeJavaScript("document.title");
  const selectedTabNavigation = selectedNavigationTitle === "Address edit reached selected tab"
    && tab.view.webContents.getURL() === page && entry.activeTargetId === second.targetId;
  // An agent may change the active tab while the toolbar request waits. Its
  // captured target must still win without altering the unrelated tab.
  activateSurfaceTab(entry, firstTargetId);
  await browserInvoke("browser_surface_nav", { instance: "selftest", targetId: second.targetId, action: "navigate", url: "data:text/html,<title>Exact toolbar target</title>" });
  const capturedNavigationTitle = await second.view.webContents.executeJavaScript("document.title");
  const capturedTabNavigation = capturedNavigationTitle === "Exact toolbar target"
    && tab.view.webContents.getURL() === page && entry.activeTargetId === firstTargetId;
  let closedTargetRejected = false;
  try { await browserInvoke("browser_surface_nav", { instance: "selftest", targetId: "closed-target", action: "navigate", url: navigationPage }); }
  catch { closedTargetRejected = tab.view.webContents.getURL() === page; }
  activateSurfaceTab(entry, second.targetId);
  // Let Chromium commit the activated widget before destroying it. Closing in
  // the same task is legal but makes Mojo report a rejected late Widget
  // message, which obscures real shell errors in the verification log.
  await new Promise((resolve) => setTimeout(resolve, 50));
  closeSurfaceTab(entry, second.targetId);
  const closed = entry.tabs.size === 1 && entry.activeTargetId === firstTargetId;
  const tabLifecycle = backgroundCreated && switched && closed;
  const ipcRejectionRoundTrip = await mainWindow.webContents.executeJavaScript(`(async () => {
    try {
      await window.__TAURI__.core.invoke("phoenix_selftest_expected_rejection");
      return false;
    } catch (error) {
      return String(error?.message || error).includes("unavailable in self-test mode");
    }
  })()`);
  const otherEntry = createSurface("selftest-isolated");
  await activeSurfaceTab(otherEntry).view.webContents.loadURL("data:text/html,<title>Isolated coworker tab</title>");
  mountSurface(otherEntry);
  const surfaceIsolation = otherEntry.mounted && !entry.mounted
    && Array.from(surfaces.values()).filter((surface) => surface.mounted).length === 1;
  const firstSurfaceState = surfaceStatus(entry);
  const isolatedSurfaceState = surfaceStatus(otherEntry);
  const tabStateIsolation = firstSurfaceState.tabs.length === 1
    && isolatedSurfaceState.tabs.length === 1
    && firstSurfaceState.tabs[0].id !== isolatedSurfaceState.tabs[0].id
    && firstSurfaceState.tabs[0].url !== isolatedSurfaceState.tabs[0].url;
  // A worker gets its own target/tab strip but the exact same Electron
  // Session object as its parent. This is the real browser-profile boundary:
  // cookies, storage, service workers, and authenticated site state are
  // inherited, while page navigation remains independent and parallel-safe.
  const authOwner = createSurface("selftest-auth-owner");
  const authWorker = createSurface("selftest-auth-worker", "selftest-auth-owner");
  const ownerTab = activeSurfaceTab(authOwner);
  const workerTab = activeSurfaceTab(authWorker);
  const authProbeUrl = "https://auth-inheritance.phoenix.invalid/";
  await ownerTab.view.webContents.session.cookies.set({
    url: authProbeUrl,
    name: "phoenix_worker_auth_probe",
    value: "inherited",
    secure: true,
    httpOnly: true,
  });
  const inheritedProbe = await workerTab.view.webContents.session.cookies.get({ url: authProbeUrl });
  const isolatedProbe = await activeSurfaceTab(otherEntry).view.webContents.session.cookies.get({ url: authProbeUrl });
  const workerAuthInheritance = authWorker.profileOwner === authOwner.instance
    && ownerTab.view.webContents.session === workerTab.view.webContents.session
    && inheritedProbe.some((cookie) => cookie.name === "phoenix_worker_auth_probe" && cookie.value === "inherited")
    && !isolatedProbe.some((cookie) => cookie.name === "phoenix_worker_auth_probe")
    && ownerTab.targetId !== workerTab.targetId
    && authOwner.tabs !== authWorker.tabs;
  destroySurface(authWorker);
  const workerSurfaceCleanup = !surfaces.has("selftest-auth-worker")
    && surfaces.has("selftest-auth-owner")
    && !ownerTab.view.webContents.isDestroyed();
  unmountSurface(otherEntry);
  mountSurface(entry);
  const gpu = await gpuProbe();
  const result = {
    ok: gpu.hardwareAcceleration && gpu.renderer.webgpu && Boolean(gpu.renderer.adapter) && tabLifecycle && selectedTabNavigation && capturedTabNavigation && closedTargetRejected && ipcRejectionRoundTrip && surfaceIsolation && tabStateIsolation && workerAuthInheritance && workerSurfaceCleanup
      && !(OZONE_PLATFORM === "wayland" && GPU_MODE === "vulkan"),
    engine: process.versions.chrome,
    electron: process.versions.electron,
    launchPolicy: {
      ozonePlatform: OZONE_PLATFORM,
      requestedGpuMode: REQUESTED_GPU_MODE,
      gpuMode: GPU_MODE,
    },
    targetId: entry.activeTargetId,
    browserTitle: tab.view.webContents.getTitle(),
    tabLifecycle,
    selectedTabNavigation,
    selectedNavigationTitle,
    capturedTabNavigation,
    capturedNavigationTitle,
    closedTargetRejected,
    ipcRejectionRoundTrip,
    surfaceIsolation,
    tabStateIsolation,
    workerAuthInheritance,
    workerSurfaceCleanup,
    gpu,
  };
  const screenshot = process.env.PHOENIX_CHROMIUM_SELFTEST_SCREENSHOT;
  if (screenshot) {
    const browserImage = await tab.view.webContents.capturePage();
    fs.writeFileSync(screenshot, browserImage.toPNG());
    result.screenshot = screenshot;
  }
  process.stdout.write(`PHOENIX_CHROMIUM_SELFTEST ${JSON.stringify(result)}\n`);
  setTimeout(() => app.exit(result.ok ? 0 : 2), 50);
}

function createMainWindow() {
  mainWindow = new BrowserWindow({
    title: "Phoenix",
    icon: path.join(__dirname, "..", "icons", "fluffy-butter-surprised.png"),
    width: 1440,
    height: 900,
    minWidth: 680,
    minHeight: 480,
    center: true,
    frame: false,
    // A normal launch must create its X11 widget immediately. Loading the
    // renderer while hidden leaves Ozone with the sentinel window id `1`, so
    // Electron can report `isVisible() === true` without a mapped window.
    // Self-tests remain hidden until their capture surface is ready.
    show: !SELFTEST && !SERVICES_ONLY,
    skipTaskbar: SERVICES_ONLY,
    backgroundColor: "#ffffff",
    webPreferences: {
      preload: PRELOAD,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: true,
      backgroundThrottling: false,
    },
  });
  mainWindow.setMenu(null);
  rendererResources.install(mainWindow.webContents, { log: bootLog });
  if (!SELFTEST && !SERVICES_ONLY) {
    frontendRecovery.install(mainWindow, {
      frontendURL: CONVERSATION_URL
        || pathToFileURL(path.join(UI_ROOT, "index.html")).href + "?chromium=1",
      dialog, getAllWebContents: () => webContents.getAllWebContents(), log: bootLog,
    });
  }
  // Interface zoom: Ctrl + / Ctrl - / Ctrl 0 and Ctrl + scroll scale the whole
  // Phoenix UI. The level is remembered across launches.
  const zoomFile = path.join(PHOENIX_HOME, "desktop-zoom.json");
  const ZOOM_MIN = 0.6, ZOOM_MAX = 1.8, ZOOM_STEP = 0.1;
  const applyZoom = zoomControl.apply = (factor) => {
    const next = Math.round(Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, factor)) * 100) / 100;
    if (!mainWindow || mainWindow.isDestroyed()) return;
    mainWindow.webContents.setZoomFactor(next);
    try { fs.writeFileSync(zoomFile, JSON.stringify({ factor: next })); } catch {}
    emit("app-zoom", { factor: next });
    // Native browser views are placed in window pixels; re-place them.
    for (const entry of surfaces.values()) if (entry.mounted && entry.cssRect) {
      entry.rect = zoomedRect(entry.cssRect);
      activeSurfaceTab(entry)?.view.setBounds(entry.rect);
    }
  };
  mainWindow.webContents.on("did-finish-load", () => {
    try { const saved = JSON.parse(fs.readFileSync(zoomFile, "utf8")).factor; if (Number.isFinite(saved)) mainWindow.webContents.setZoomFactor(saved); } catch {}
  });
  mainWindow.webContents.on("before-input-event", (event, input) => {
    if (input.type !== "keyDown" || !(input.control || input.meta) || input.alt) return;
    const current = mainWindow.webContents.getZoomFactor();
    if (input.key === "=" || input.key === "+") { event.preventDefault(); applyZoom(current + ZOOM_STEP); }
    else if (input.key === "-" || input.key === "_") { event.preventDefault(); applyZoom(current - ZOOM_STEP); }
    else if (input.key === "0" && !input.shift) { event.preventDefault(); applyZoom(1); }
  });
  mainWindow.webContents.on("zoom-changed", (_event, direction) => {
    applyZoom(mainWindow.webContents.getZoomFactor() + (direction === "in" ? ZOOM_STEP : -ZOOM_STEP));
  });
  // A renderer reload forgets which agent browser it was showing, but the
  // native views stay mounted over the window with no panel around them.
  // Park them all; the reloaded renderer re-shows the one it restores.
  mainWindow.webContents.on("did-start-navigation", (details) => {
    if (!details.isMainFrame || details.isSameDocument) return;
    for (const entry of surfaces.values()) unmountSurface(entry);
  });
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:/i.test(url)) shell.openExternal(url);
    return { action: "deny" };
  });
  mainWindow.on("close", (event) => {
    if (!CLOSE_TO_TRAY || SERVICES_ONLY || fullQuitRequested || stopping) return;
    // Hide instead of destroying: the renderer stays connected to the
    // gateway, so reopening is instant and no agent work is interrupted.
    event.preventDefault();
    mainWindow.hide();
    refreshTrayMenu();
    if (!trayHintShown && Notification.isSupported()) {
      trayHintShown = true;
      try {
        new Notification({ title: "Phoenix is still running", body: "Your coworkers keep working in the background. Open Phoenix from the tray, or choose Quit Phoenix to stop everything.", silent: true }).show();
      } catch {}
    }
  });
  mainWindow.on("closed", () => {
    mainWindow = null;
    // The invisible render host must not keep Phoenix alive after its UI closes.
    for (const entry of [...surfaces.values()]) destroySurface(entry);
    if (captureWindow && !captureWindow.isDestroyed()) captureWindow.close();
  });
  if (!SELFTEST && !SERVICES_ONLY) {
    // Never leave the real shell stranded as an invisible background process.
    // The load path is preferred, `ready-to-show` covers renderer variations,
    // and the bounded timer is the final guarantee if neither event arrives.
    mainWindow.once("ready-to-show", () => {
      bootLog("Phoenix window ready to show");
      setTimeout(() => revealMainWindow("ready-to-show"), SHOW_DELAY_MS);
    });
    mainWindow.webContents.once("did-finish-load", () => {
      bootLog("Phoenix renderer finished loading");
      setTimeout(() => revealMainWindow("did-finish-load"), SHOW_DELAY_MS);
    });
    setTimeout(() => revealMainWindow("startup-timeout"), Math.max(3000, SHOW_DELAY_MS + 1000));
  }
  if (SELFTEST) {
    mainWindow.webContents.once("did-finish-load", () => {
      runSelftest().catch((error) => {
        process.stderr.write(`PHOENIX_CHROMIUM_SELFTEST_ERROR ${error.stack || error}\n`);
        app.exit(2);
      });
    });
    mainWindow.loadFile(path.join(UI_ROOT, "relay.html"), { query: { selftest: "1" } });
  } else if (CONVERSATION_URL) {
    mainWindow.loadURL(CONVERSATION_URL);
  } else {
    mainWindow.loadFile(path.join(UI_ROOT, SERVICES_ONLY ? "native-services.html" : "index.html"), { query: { chromium: "1" } });
  }
}

// Expected native failures (a removed attachment, cancelled picker, stale
// optional artifact) are handled by the renderer. Letting them reject the
// Electron IPC handler makes Electron print a full "Error occurred in handler"
// stack for every caught error. Return a typed envelope and let preload restore
// normal Promise rejection semantics without polluting the desktop log.
function gatewayRunning() {
  try { return fs.statSync(path.join(PHOENIX_HOME, "gateway.sock")).isSocket(); } catch { return false; }
}

function openFromTray() {
  if (!mainWindow || mainWindow.isDestroyed()) createMainWindow();
  else revealMainWindow("tray");
  refreshTrayMenu();
}

// A full quit is the one path that also stops the background gateway. The
// Rust parent reads this marker after Electron exits and performs the
// verified gateway stop (which wipes the in-memory Passes key with it).
function quitPhoenixCompletely() {
  if (fullQuitRequested) return;
  fullQuitRequested = true;
  bootLog("full quit requested from tray/menu; gateway will stop");
  try {
    fs.writeFileSync(path.join(PHOENIX_HOME, "desktop-quit-requested.json"), JSON.stringify({ at: Date.now(), pid: process.pid }), { mode: 0o600 });
  } catch (error) { bootLog(`could not record full quit: ${error.message}`); }
  Promise.race([flushBrowserStorage(), new Promise((done) => setTimeout(done, 1500))])
    .finally(() => { tray?.destroy(); tray = null; app.quit(); });
}

function refreshTrayMenu() {
  if (!tray) return;
  const visible = Boolean(mainWindow && !mainWindow.isDestroyed() && mainWindow.isVisible());
  const running = gatewayRunning();
  tray.setToolTip(running ? "Phoenix — coworkers running" : "Phoenix — gateway starting…");
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: visible ? "Show Phoenix" : "Open Phoenix", click: openFromTray },
    { type: "separator" },
    { label: running ? "● Coworkers running in the background" : "○ Gateway starting…", enabled: false },
    { label: "Closing the window keeps them working", enabled: false },
    { type: "separator" },
    { label: "Quit Phoenix", accelerator: "CmdOrCtrl+Q", click: quitPhoenixCompletely },
  ]));
}

function createTray() {
  if (!CLOSE_TO_TRAY || SERVICES_ONLY || tray) return;
  try {
    const iconPath = [path.join(__dirname, "..", "icons", "fluffy-butter-surprised.png")].find((file) => fs.existsSync(file));
    let image = iconPath ? nativeImage.createFromPath(iconPath) : nativeImage.createEmpty();
    if (!image.isEmpty()) image = image.resize({ width: process.platform === "darwin" ? 18 : 22, height: process.platform === "darwin" ? 18 : 22, quality: "best" });
    tray = new Tray(image);
    tray.on("click", openFromTray);
    tray.on("double-click", openFromTray);
    refreshTrayMenu();
    setInterval(refreshTrayMenu, 10000).unref();
  } catch (error) {
    // Without a tray (e.g. a desktop with no status-notifier host) the window
    // still hides on close; launching Phoenix again brings it back.
    bootLog(`system tray unavailable: ${error.message}`);
  }
}

ipcMain.handle("phoenix:invoke", async (_event, command, args) => {
  try {
    return { __phoenixInvokeResult: true, ok: true, value: await invoke(command, args) };
  } catch (error) {
    return { __phoenixInvokeResult: true, ok: false, error: String(error?.message || error) };
  }
});
ipcMain.handle("phoenix:gpu-probe", gpuProbe);

// Site logins kept in local storage (Discord, X) are written to disk lazily
// and were lost on every restart: systemd stops all Phoenix processes at
// once and the shell exited without saving. Save every agent browser's
// storage regularly, and once more on the stop signal.
function flushBrowserStorage() {
  const owners = new Set([...surfaces.values()].map((entry) => entry.profileOwner));
  return Promise.all([...owners].map((owner) => {
    const store = session.fromPartition(`persist:phoenix-${owner}`);
    store.flushStorageData();
    return store.cookies.flushStore().catch(() => {});
  }));
}
let stopping = false;
for (const signal of ["SIGTERM", "SIGINT"]) {
  process.on(signal, () => {
    if (stopping) return; stopping = true;
    Promise.race([flushBrowserStorage(), new Promise((done) => setTimeout(done, 1500))])
      .finally(() => app.quit());
  });
}

// A click on a Phoenix notification writes desktop-open.json; bring the
// window forward on that conversation instead of leaving the user nowhere.
function watchNotificationClicks() {
  const file = "desktop-open.json";
  let last = 0;
  try {
    fs.watch(PHOENIX_HOME, (_event, name) => {
      if (name !== file) return;
      let request;
      try { request = JSON.parse(fs.readFileSync(path.join(PHOENIX_HOME, file), "utf8")); } catch { return; }
      const at = Number(request?.at) || 0;
      if (at <= last || Date.now() - at > 60000 || !mainWindow || mainWindow.isDestroyed()) return;
      last = at;
      if (mainWindow.isMinimized()) mainWindow.restore();
      mainWindow.show(); mainWindow.focus(); app.focus({ steal: true });
      emit("open-conversation", { kind: String(request.kind || ""), id: String(request.id || "") });
    });
  } catch (error) { bootLog(`notification click watch unavailable: ${error.message}`); }
}

app.whenReady().then(() => {
  setInterval(() => { flushBrowserStorage().catch(() => {}); }, 15000).unref();
  watchNotificationClicks();
  bootLog("Electron ready; starting native bridge");
  startBridgeServer();
  bootLog("creating Phoenix window");
  createMainWindow();
  createTray();
  bootLog("Phoenix window creation returned");
  setTimeout(() => bootLog("main loop responsive"), 1000).unref();
  if (Number.isInteger(RUST_PARENT_PID) && RUST_PARENT_PID > 1) {
    setInterval(() => {
      try { process.kill(RUST_PARENT_PID, 0); }
      catch { app.quit(); }
    }, 2000).unref();
  }
});
// With close-to-tray the window is hidden, not closed, so this fires only on a
// real quit (or when tray mode is disabled).
app.on("window-all-closed", () => { if (!CLOSE_TO_TRAY || fullQuitRequested || stopping) app.quit(); });
app.on("activate", () => openFromTray());
// An OS-level quit of the primary window (macOS Cmd+Q, a desktop "Quit"
// action) is a full quit too. A second instance handing off, a vanished Rust
// parent, or a stop signal is not, and never stops the gateway.
app.on("before-quit", (event) => {
  if (!CLOSE_TO_TRAY || SELFTEST || SERVICES_ONLY || stopping || fullQuitRequested || !PRIMARY_INSTANCE) return;
  if (Number.isInteger(RUST_PARENT_PID) && RUST_PARENT_PID > 1) {
    try { process.kill(RUST_PARENT_PID, 0); } catch { return; }
  }
  event.preventDefault();
  quitPhoenixCompletely();
});
