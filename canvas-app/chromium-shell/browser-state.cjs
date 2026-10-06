"use strict";
// Chrome-like browser state for Phoenix's embedded browsers:
//  - tabs are saved per browser and reopened next time ("continue where you
//    left off"), except disposable worker/job browsers;
//  - Chrome Web Store installs work in every browser session, share one
//    extension folder, and can be switched off, removed, or opened (popup).
const fs = require("node:fs");
const path = require("node:path");
const { BrowserWindow } = require("electron");

function create({ phoenixHome }) {
  const stateRoot = path.join(phoenixHome, "browser");
  const tabsRoot = path.join(stateRoot, "tabs");
  const extensionsPath = path.join(stateRoot, "extensions");
  const disabledFile = path.join(stateRoot, "extensions-disabled.json");
  fs.mkdirSync(tabsRoot, { recursive: true, mode: 0o700 });
  fs.mkdirSync(extensionsPath, { recursive: true, mode: 0o700 });

  // ── Tabs ────────────────────────────────────────────────────────────────
  const saveTimers = new Map();
  const persistent = (instance) => !/(?:^volume-worker|-job-)/.test(instance);
  const tabsFile = (instance) => path.join(tabsRoot, `${instance.replace(/[^a-zA-Z0-9_.-]/g, "_")}.json`);
  const restorable = (url) => /^(?:https?|file):/i.test(String(url || ""));

  function savedTabs(instance) {
    if (!persistent(instance)) return null;
    try {
      const value = JSON.parse(fs.readFileSync(tabsFile(instance), "utf8"));
      const tabs = (Array.isArray(value.tabs) ? value.tabs : []).filter((tab) => restorable(tab.url)).slice(0, 40);
      if (!tabs.length) return null;
      return { tabs, active: Math.min(Math.max(0, Number(value.active) || 0), tabs.length - 1) };
    } catch { return null; }
  }

  function scheduleSave(entry) {
    if (!persistent(entry.instance)) return;
    clearTimeout(saveTimers.get(entry.instance));
    saveTimers.set(entry.instance, setTimeout(() => {
      saveTimers.delete(entry.instance);
      const tabs = [];
      let active = 0;
      for (const tab of entry.tabs.values()) {
        const contents = tab.view.webContents;
        if (contents.isDestroyed()) continue;
        const url = contents.getURL();
        if (!restorable(url)) continue;
        if (tab.targetId === entry.activeTargetId) active = tabs.length;
        tabs.push({ url, title: contents.getTitle() || "" });
      }
      // An all-blank browser keeps the previous session instead of erasing it.
      if (!tabs.length) return;
      const file = tabsFile(entry.instance), temp = `${file}.tmp`;
      try { fs.writeFileSync(temp, JSON.stringify({ version: 1, tabs, active }), { mode: 0o600 }); fs.renameSync(temp, file); } catch {}
    }, 400));
  }

  // ── Extensions ──────────────────────────────────────────────────────────
  const sessions = new Set();
  let webStore = null;
  const loadWebStore = () => (webStore ||= require("electron-chrome-web-store"));
  const readDisabled = () => { try { return new Set(JSON.parse(fs.readFileSync(disabledFile, "utf8"))); } catch { return new Set(); } };
  const writeDisabled = (set) => { try { fs.writeFileSync(disabledFile, JSON.stringify([...set]), { mode: 0o600 }); } catch {} };

  function mirror(session) {
    // An extension installed in one browser shows up in every other one.
    session.extensions.on("extension-loaded", (_event, extension) => {
      if (readDisabled().has(extension.id)) return;
      for (const other of sessions) {
        if (other === session || other.extensions.getExtension(extension.id)) continue;
        other.extensions.loadExtension(extension.path, { allowFileAccess: false }).catch(() => {});
      }
    });
  }

  async function ensureExtensions(session) {
    if (!session || sessions.has(session)) return;
    sessions.add(session);
    mirror(session);
    try {
      await loadWebStore().installChromeWebStore({ session, extensionsPath, autoUpdate: true });
      const disabled = readDisabled();
      for (const extension of session.extensions.getAllExtensions()) if (disabled.has(extension.id)) session.extensions.removeExtension(extension.id);
    } catch (error) { console.error("[phoenix] chrome web store unavailable:", error?.message || error); }
  }

  function manifestOf(dir) { try { return JSON.parse(fs.readFileSync(path.join(dir, "manifest.json"), "utf8")); } catch { return null; } }
  function iconOf(dir, manifest) {
    const icons = { ...(manifest?.icons || {}), ...(manifest?.action?.default_icon && typeof manifest.action.default_icon === "object" ? manifest.action.default_icon : {}) };
    const size = Object.keys(icons).map(Number).filter(Boolean).sort((a, b) => Math.abs(a - 48) - Math.abs(b - 48))[0];
    const file = size ? path.join(dir, icons[size]) : null;
    try { return file && fs.existsSync(file) ? `data:image/png;base64,${fs.readFileSync(file).toString("base64")}` : ""; } catch { return ""; }
  }
  const localized = (value, dir, manifest) => {
    const key = /^__MSG_(.+)__$/.exec(String(value || ""))?.[1];
    if (!key) return String(value || "");
    const locale = manifest?.default_locale || "en";
    try { const messages = JSON.parse(fs.readFileSync(path.join(dir, "_locales", locale, "messages.json"), "utf8")); return messages[key]?.message || messages[key.toLowerCase()]?.message || value; } catch { return value; }
  };

  // Installed web-store extensions live in <id>/<version>/.
  function installed() {
    const disabled = readDisabled(), out = [];
    for (const id of fs.existsSync(extensionsPath) ? fs.readdirSync(extensionsPath) : []) {
      const root = path.join(extensionsPath, id);
      if (!/^[a-p]{32}$/.test(id) || !fs.statSync(root).isDirectory()) continue;
      const version = fs.readdirSync(root).filter((name) => fs.statSync(path.join(root, name)).isDirectory()).sort().pop();
      if (!version) continue;
      const dir = path.join(root, version), manifest = manifestOf(dir);
      if (!manifest) continue;
      const popup = manifest.action?.default_popup || manifest.browser_action?.default_popup || "";
      out.push({ id, name: localized(manifest.name, dir, manifest), version: manifest.version || version, enabled: !disabled.has(id), icon: iconOf(dir, manifest), popup, path: dir });
    }
    return out.sort((a, b) => a.name.localeCompare(b.name));
  }

  async function setEnabled(id, enabled) {
    const disabled = readDisabled(), extension = installed().find((item) => item.id === id);
    if (!extension) throw new Error("extension is not installed");
    enabled ? disabled.delete(id) : disabled.add(id);
    writeDisabled(disabled);
    for (const session of sessions) {
      if (!enabled && session.extensions.getExtension(id)) session.extensions.removeExtension(id);
      if (enabled && !session.extensions.getExtension(id)) await session.extensions.loadExtension(extension.path, { allowFileAccess: false }).catch(() => {});
    }
  }

  async function remove(id) {
    if (!/^[a-p]{32}$/.test(id)) throw new Error("invalid extension id");
    for (const session of sessions) if (session.extensions.getExtension(id)) session.extensions.removeExtension(id);
    const first = sessions.values().next().value;
    try { await loadWebStore().uninstallExtension(id, { session: first, extensionsPath }); } catch {}
    fs.rmSync(path.join(extensionsPath, id), { recursive: true, force: true });
    const disabled = readDisabled(); disabled.delete(id); writeDisabled(disabled);
  }

  function openPopup(id, session, parent) {
    const extension = installed().find((item) => item.id === id);
    if (!extension?.popup) throw new Error("this extension has no popup");
    const popup = new BrowserWindow({
      parent, width: 380, height: 560, show: false, resizable: true, minimizable: false, maximizable: false,
      title: extension.name, autoHideMenuBar: true,
      webPreferences: { session, contextIsolation: true, sandbox: true, nodeIntegration: false },
    });
    popup.once("ready-to-show", () => popup.show());
    popup.on("blur", () => { if (!popup.isDestroyed()) popup.close(); });
    popup.loadURL(`chrome-extension://${id}/${extension.popup.replace(/^\/+/, "")}`);
    return { opened: true };
  }

  return { savedTabs, scheduleSave, ensureExtensions, installed, setEnabled, remove, openPopup };
}

module.exports = { create };
