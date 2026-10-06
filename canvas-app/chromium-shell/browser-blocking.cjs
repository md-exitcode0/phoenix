"use strict";
const fs = require("node:fs");
const path = require("node:path");

const LISTS = Object.freeze([
  "https://easylist.to/easylist/easylist.txt",
  "https://easylist.to/easylist/easyprivacy.txt",
]);
const MAX_LIST_BYTES = 12 * 1024 * 1024;
const MAX_CACHE_BYTES = 24 * 1024 * 1024;

function atomicWrite(file, data) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const temp = file + "." + process.pid + ".tmp";
  try { fs.writeFileSync(temp, data, { mode: 0o600 }); fs.renameSync(temp, file); }
  finally { try { fs.unlinkSync(temp); } catch (error) { if (error.code !== "ENOENT") throw error; } }
}

async function fetchList(url, fetcher) {
  const response = await fetcher(url, { signal: AbortSignal.timeout(15000), redirect: "error" });
  if (!response.ok) throw new Error("Could not download blocking lists (" + response.status + ").");
  if (Number(response.headers.get("content-length")) > MAX_LIST_BYTES) throw new Error("Blocking list is too large.");
  const reader = response.body.getReader(), chunks = [];
  let bytes = 0;
  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      bytes += chunk.value.byteLength;
      if (bytes > MAX_LIST_BYTES) throw new Error("Blocking list is too large.");
      chunks.push(Buffer.from(chunk.value));
    }
  } finally { await reader.cancel().catch(() => {}); }
  const text = Buffer.concat(chunks, bytes).toString("utf8");
  if (!/^\[Adblock(?: Plus)?[^\]]*\]/m.test(text)) throw new Error("The downloaded blocking list is invalid.");
  return text;
}

function create({ phoenixHome, library = () => require("./vendor/adblocker/engine.cjs"), fetcher = globalThis.fetch, write = atomicWrite }) {
  const root = path.join(phoenixHome, "browser", "blocking");
  const prefsFile = path.join(root, "settings.json"), cacheFile = path.join(root, "engine.bin");
  let enabled = false, engine = null, enginePromise = null, lastError = "", sequence = Promise.resolve();
  try { enabled = JSON.parse(fs.readFileSync(prefsFile, "utf8")).enabled === true; } catch {}
  const sessions = new Map();
  const enqueue = fn => {
    const operation = sequence.then(fn);
    sequence = operation.catch(() => {});
    return operation;
  };
  async function getEngine() {
    if (engine) return engine;
    if (!enginePromise) enginePromise = (async () => {
      const { FiltersEngine } = library();
      try {
        const stat = fs.statSync(cacheFile);
        if (stat.size > MAX_CACHE_BYTES) throw new Error("Blocking cache is too large.");
        engine = FiltersEngine.deserialize(new Uint8Array(fs.readFileSync(cacheFile)));
      } catch {
        const lists = await Promise.all(LISTS.map(url => fetchList(url, fetcher)));
        const built = FiltersEngine.parse(lists.join("\n"), { loadCosmeticFilters: false });
        const data = built.serialize();
        if (data.byteLength > MAX_CACHE_BYTES) throw new Error("Blocking cache is too large.");
        write(cacheFile, data);
        engine = built;
      }
      return engine;
    })().catch(error => { enginePromise = null; throw error; });
    return enginePromise;
  }
  function apply(session, active) {
    const item = sessions.get(session);
    if (item.active === active) return;
    if (!active) {
      session.webRequest.onBeforeRequest(null);
      item.active = false;
      return;
    }
    const { Request } = library();
    session.webRequest.onBeforeRequest({ urls: ["http://*/*", "https://*/*"] }, (details, callback) => {
      let cancel = false;
      try {
        if (details.resourceType !== "mainFrame") {
          const sourceUrl = details.referrer || details.webContents?.getURL() || "";
          cancel = !!engine.match(Request.fromRawDetails({
            url: details.url, sourceUrl, type: details.resourceType || "other",
          })).match;
          if (cancel) item.blocked = Math.min(Number.MAX_SAFE_INTEGER, item.blocked + 1);
        }
      } catch (error) { lastError = error.message || String(error); }
      callback({ cancel });
    });
    item.active = true;
  }
  function status() {
    const entries = [...sessions.values()];
    return {
      enabled, active: enabled && entries.length > 0 && entries.every(item => item.active),
      ready: !!engine, blocked: entries.reduce((n, item) => n + item.blocked, 0),
      error: lastError, networkOnly: true,
    };
  }
  function attach(session) {
    if (!session || sessions.has(session)) return Promise.resolve(status());
    sessions.set(session, { active: false, blocked: 0 });
    return enqueue(async () => {
      try { if (enabled) await getEngine(); apply(session, enabled); lastError = ""; }
      catch (error) { lastError = error.message || String(error); throw error; }
      return status();
    });
  }
  function setEnabled(value) {
    if (typeof value !== "boolean") return Promise.reject(new TypeError("Blocking requires an on/off value."));
    return enqueue(async () => {
      const previous = new Map([...sessions].map(([session, item]) => [session, item.active]));
      try {
        if (value) await getEngine();
        for (const session of sessions.keys()) apply(session, value);
        write(prefsFile, JSON.stringify({ version: 1, enabled: value }));
        enabled = value; lastError = "";
      } catch (error) {
        for (const [session, active] of previous) { try { apply(session, active); } catch {} }
        lastError = error.message || String(error);
        throw error;
      }
      return status();
    });
  }
  return { attach, setEnabled, status };
}
module.exports = { create, fetchList, LISTS };
