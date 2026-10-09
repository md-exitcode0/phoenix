// Phoenix status pill: one small, plain-words line under the latest message
// that says what the agent is doing right now ("Reading school-guide.md",
// "Messaging Leon", "Waiting for Leon"). Pure logic lives here so it can be
// tested in node; conversation.js owns the DOM and feeds it events.
(function (root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  if (root) root.PhoenixStatusPill = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  // After this long with no runtime activity for the selected conversation,
  // while the gateway no longer reports it live, the indicator is stale and is
  // removed even if a socket was never closed cleanly.
  const STALE_MS = 30000;

  function baseName(value) {
    const clean = String(value || "").trim().replace(/[?#].*$/, "").replace(/\/+$/, "");
    if (!clean) return "";
    const last = clean.split(/[\\/]/).pop();
    return last.length > 40 ? `${last.slice(0, 37)}…` : last;
  }
  function hostOf(value) {
    const raw = String(value || "").trim();
    if (!raw) return "";
    try { return new URL(/^[a-z]+:\/\//i.test(raw) ? raw : `https://${raw}`).hostname.replace(/^www\./, ""); }
    catch { return ""; }
  }
  // `target` is whatever the runtime summarised for the call: a JSON input,
  // a path, a URL, or a short phrase. Read the useful hint out of it.
  function parseTarget(target) {
    if (target && typeof target === "object") return target;
    const raw = String(target || "").trim();
    if (!raw) return {};
    if (/^[\[{]/.test(raw)) { try { const value = JSON.parse(raw); if (value && typeof value === "object" && !Array.isArray(value)) return value; } catch {} }
    return { text: raw };
  }
  function firstName(value) {
    const text = String(value || "").trim().replace(/\s*\([^)]*\)\s*$/, "");
    return text.split(/\s+/)[0] || "";
  }
  function recipientOf(input, resolveName) {
    const to = Array.isArray(input.to) ? input.to : input.to ? [input.to] : [];
    const raw = to.length ? to[0] : input.agent || input.group || "";
    if (!raw) return "";
    const named = typeof resolveName === "function" ? resolveName(String(raw)) : "";
    const name = firstName(named || raw);
    return to.length > 1 ? `${name} and others` : name;
  }

  const SIMPLE = {
    web_search: "Searching the web", composio_search: "Searching the web",
    web_crawl: "Reading the web", memory_recall: "Checking notes", recall: "Checking notes",
    memory_save: "Saving a note", remember: "Saving a note",
    todo_write: "Updating the to-do list", work: "Updating the plan",
    image_gen: "Making an image", image_generate: "Making an image",
    image_analyze: "Looking at an image", ui_snap: "Checking the page",
    design_website: "Designing the website", design_reference: "Looking at references",
    ask_user: "Waiting for you", ask_for_login: "Waiting for you",
    user_update: "Writing an update", final_answer: "Writing the answer",
    skill: "Loading a skill", load_tools: "Getting tools ready",
    volume_work: "Running parallel work", agent_control: "Checking on coworkers",
    react: "Reacting", terminal: "Running a command",
  };

  function toolLabel(tool, target, resolveName) {
    const name = String(tool || "").trim().toLowerCase();
    const input = parseTarget(target);
    const path = input.path || input.file_path || input.file || (input.text && /[\\/.]/.test(input.text) && !/\s/.test(input.text) ? input.text : "");
    const file = baseName(path);
    if (!name) return "Working";
    if (name === "read" || name === "read_file" || name === "view") return file ? `Reading ${file}` : "Reading files";
    if (name === "write" || name === "create_file") return file ? `Writing ${file}` : "Writing a file";
    if (name === "str_replace" || name === "edit" || name === "apply_patch" || name === "multi_edit") return file ? `Editing ${file}` : "Editing files";
    if (name === "grep" || name === "glob" || name === "search_files") return "Searching files";
    if (name === "list_directory" || name === "ls") return "Looking through files";
    if (name === "bash" || name === "shell" || name === "terminal_run") {
      const command = String(input.command || input.cmd || input.text || "");
      if (/\b(test|pytest|jest|vitest|cargo\s+test|npm\s+(run\s+)?test|node\s+--test)\b/.test(command)) return "Running tests";
      if (/\b(build|cargo\s+(check|build)|tsc|webpack|vite\s+build)\b/.test(command)) return "Building";
      if (/\b(npm|pnpm|yarn|pip)\s+(i|install|add)\b/.test(command)) return "Installing packages";
      return "Running a command";
    }
    if (name === "web_fetch" || name === "fetch") { const host = hostOf(input.url || input.text); return host ? `Reading ${host}` : "Reading a web page"; }
    if (name === "message_agent") { const who = recipientOf(input, resolveName); return who ? `Messaging ${who}` : "Messaging a coworker"; }
    if (name === "talk") { const who = recipientOf(input, resolveName); return who ? `Asking ${who}` : "Asking a coworker"; }
    if (/^browser_/.test(name) || name === "browser") {
      const host = hostOf(input.url || "");
      if (/navigate|open|goto/.test(name)) return host ? `Opening ${host}` : "Opening a page";
      if (/screenshot|snapshot|read|extract|content/.test(name)) return "Reading the page";
      if (/type|fill|click|press|select|scroll/.test(name)) return "Using the browser";
      return "Using the browser";
    }
    if (/^computer_/.test(name)) return "Using the computer";
    if (/^(gmail|email)_/.test(name)) return "Checking email";
    if (/^calendar_/.test(name)) return "Checking the calendar";
    if (SIMPLE[name]) return SIMPLE[name];
    if (/search/.test(name)) return "Searching";
    return "Working";
  }

  // One source of truth for the indicator.
  //   facts.ownerLive   gateway says the selected conversation's agent is running
  //   facts.socketOpen  this window holds the live turn socket
  //   facts.waitingFor  display name of a coworker whose return the owner awaits
  //   facts.waitingPeer the gateway says the owner waits on a coworker
  //   facts.waitingUser the owner is blocked on the user's answer
  //   facts.lastEventAt last runtime activity (story/done/journal) for this chat
  //   facts.now         clock
  //   facts.action      latest tool label for the running turn
  function indicatorState(facts) {
    const f = facts || {};
    const now = Number(f.now) || Date.now();
    const quietFor = f.lastEventAt ? now - Number(f.lastEventAt) : Infinity;
    // The gateway's own status wins: the agent is running.
    if (f.ownerLive) return { mode: "running", label: f.action || "Working" };
    // The owner's turn is over but it is waiting on someone. That is not
    // "working": say who it waits for.
    if (f.waitingFor) return { mode: "waiting", label: `Waiting for ${firstName(f.waitingFor) || "a coworker"}` };
    if (f.waitingPeer) return { mode: "waiting", label: "Waiting for a coworker" };
    if (f.waitingUser) return { mode: "waiting", label: "Waiting for you" };
    // Only this window believes a turn runs (it just sent one, or the gateway
    // status lags). Trust it while runtime activity is recent; after that the
    // flag is stale and the caller clears it.
    const stale = quietFor > (Number(f.staleMs) || STALE_MS);
    if (f.socketOpen && !stale) return { mode: "running", label: f.action || "Working" };
    return { mode: "idle", label: "", stale: Boolean(f.socketOpen && stale) };
  }

  return { STALE_MS, toolLabel, indicatorState, firstName };
});
