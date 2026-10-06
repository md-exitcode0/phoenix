"use strict";

const { contextBridge, ipcRenderer } = require("electron");

const eventListeners = new Map();
let nextListenerId = 1;

ipcRenderer.on("phoenix:event", (_event, name, payload) => {
  for (const listener of eventListeners.values()) {
    if (listener.name === name) listener.callback({ event: name, payload });
  }
});

async function invoke(command, args = {}) {
  const reply = await ipcRenderer.invoke("phoenix:invoke", command, args);
  if (!reply || reply.__phoenixInvokeResult !== true) return reply;
  if (!reply.ok) throw new Error(String(reply.error || "native command failed"));
  return reply.value;
}

function listen(name, callback) {
  const id = nextListenerId++;
  eventListeners.set(id, { name, callback });
  return Promise.resolve(() => eventListeners.delete(id));
}

function currentWindow() {
  return {
    minimize: () => invoke("plugin:window|minimize"),
    toggleMaximize: () => invoke("plugin:window|toggle_maximize"),
    close: () => invoke("plugin:window|close"),
    setTheme: (theme) => invoke("plugin:window|set_theme", { theme }),
    setBackgroundColor: (color) => invoke("plugin:window|set_background_color", { color }),
  };
}

contextBridge.exposeInMainWorld("__PHOENIX_CHROMIUM_SHELL__", Object.freeze({
  enabled: true,
  engine: "chromium",
  invoke,
  probe: () => ipcRenderer.invoke("phoenix:gpu-probe"),
}));

// Preserve the frontend's existing Tauri-shaped boundary. Native commands
// that still live in Rust are forwarded by the bridge in the integrated
// build; Chromium-owned window and browser-surface commands terminate here.
contextBridge.exposeInMainWorld("__TAURI__", Object.freeze({
  core: Object.freeze({ invoke }),
  event: Object.freeze({ listen }),
  window: Object.freeze({ getCurrentWindow: currentWindow }),
  webviewWindow: Object.freeze({ getCurrentWebviewWindow: currentWindow }),
}));
