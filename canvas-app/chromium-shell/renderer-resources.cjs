"use strict";

// Chromium's Linux zygote can retain a 1024 soft descriptor limit after Node
// raises the main process limit. Canvas/image shared memory also uses file
// descriptors. Keep the interface and its future renderer replacements within
// the existing hard limit, without changing the gateway or browser views.
const fs = require("node:fs/promises");
const { execFile } = require("node:child_process");
const { promisify } = require("node:util");
const run = promisify(execFile);
const CONTROL = Symbol.for("phoenix.renderer.resources");
const MINIMUM = 4096;

async function ensureProcess(pid, {
  parentPid = process.pid, executable = process.execPath,
  uid = process.getuid?.(), read = fs.readFile, execute = run,
} = {}) {
  if (!Number.isSafeInteger(pid) || pid <= 1 || pid === parentPid) return [];
  const chain = [];
  let current = pid;
  // Validate the complete ownership chain before changing any process.
  for (let depth = 0; current !== parentPid && depth < 8; depth++) {
    const [cmd, status, limits] = await Promise.all([
      read(`/proc/${current}/cmdline`, "utf8"),
      read(`/proc/${current}/status`, "utf8"),
      read(`/proc/${current}/limits`, "utf8"),
    ]);
    // Chromium also uses a single flattened process title on Linux.
    const sameExecutable = cmd.startsWith(executable + "\0")
      || cmd.startsWith(executable + " ");
    const role = /(?:^|[\0\s])--type=renderer(?=$|[\0\s])/.test(cmd) ? "renderer"
      : /(?:^|[\0\s])--type=zygote(?=$|[\0\s])/.test(cmd) ? "zygote" : null;
    const owner = Number(status.match(/^Uid:\s+(\d+)/m)?.[1]);
    const parent = Number(status.match(/^PPid:\s+(\d+)/m)?.[1]);
    const limit = limits.match(/^Max open files\s+(\d+)\s+(\d+)\s+files\s*$/m);
    if (!sameExecutable || owner !== uid || !limit || !parent
        || role !== (depth === 0 ? "renderer" : "zygote")) {
      throw new Error("Renderer resource ownership could not be established");
    }
    chain.push({ pid: current, soft: Number(limit[1]), hard: Number(limit[2]) });
    current = parent;
  }
  if (current !== parentPid) throw new Error("Renderer is outside this Phoenix process");
  const changed = [];
  // Parents first so future replacement renderers inherit the same limit.
  for (const entry of chain.reverse()) {
    const soft = Math.min(MINIMUM, entry.hard);
    if (entry.soft >= soft) continue;
    await execute("/usr/bin/prlimit", ["--pid", String(entry.pid),
      `--nofile=${soft}:${entry.hard}`], { timeout: 2000 });
    changed.push({ pid: entry.pid, before: entry.soft, after: soft, hard: entry.hard });
  }
  return changed;
}

function install(contents, { log = () => {}, ensure = ensureProcess,
  platform = process.platform, setTimer = setTimeout, clearTimer = clearTimeout } = {}) {
  if (contents[CONTROL]) return contents[CONTROL];
  let timer = null, closed = false, generation = 0;
  const schedule = () => {
    if (closed || platform !== "linux") return;
    const token = ++generation;
    if (timer !== null) clearTimer(timer);
    const check = async (attempt = 0) => {
      timer = null;
      if (closed || token !== generation || contents.isDestroyed()) return;
      const pid = contents.getOSProcessId();
      if (pid <= 1) {
        if (attempt < 20) timer = setTimer(() => void check(attempt + 1), 50);
        return;
      }
      try {
        const changes = await ensure(pid);
        for (const change of changes) log(`interface resource limit ${change.pid}: ${change.before} → ${change.after}`);
      } catch (error) { log("interface resource limit: " + String(error?.message || error)); }
    };
    void check();
  };
  const dispose = () => {
    if (closed) return;
    closed = true; generation++;
    if (timer !== null) clearTimer(timer);
    contents.removeListener("did-start-loading", schedule);
    contents.removeListener("dom-ready", schedule);
    contents.removeListener("destroyed", dispose);
    delete contents[CONTROL];
  };
  const control = Object.freeze({ ensure: schedule, dispose });
  contents[CONTROL] = control;
  contents.on("did-start-loading", schedule);
  contents.on("dom-ready", schedule);
  contents.once("destroyed", dispose);
  schedule();
  return control;
}

module.exports = { ensureProcess, install };
