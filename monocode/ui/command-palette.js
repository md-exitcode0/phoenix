"use strict";

(() => {
  const ui = window.PhoenixUI;
  if (!ui) return;
  const escape = ui.escapeHtml;
  const state = { open: false, query: "", index: 0, visible: [], actions: new Map() };
  const host = document.createElement("section");
  host.id = "commandPalette";
  host.className = "command-palette";
  host.hidden = true;
  host.setAttribute("aria-label", "Phoenix command palette");
  host.innerHTML = `<div class="command-palette-shell" role="dialog" aria-modal="true" aria-labelledby="commandPaletteLabel"><label class="command-palette-search"><i aria-hidden="true"></i><input id="commandPaletteInput" type="search" autocomplete="off" spellcheck="false" placeholder="Find anything or run a command"><kbd>esc</kbd><span id="commandPaletteLabel" hidden>Find anything or run a command</span></label><div id="commandPaletteResults" class="command-palette-results" role="listbox"></div><footer class="command-palette-footer"><span><kbd>↑↓</kbd> move</span><span><kbd>enter</kbd> open</span><span><kbd>esc</kbd> close</span></footer></div>`;
  document.body.appendChild(host);
  const input = host.querySelector("#commandPaletteInput");
  const results = host.querySelector("#commandPaletteResults");

  function add(items, item) {
    item.id = `${item.group}:${item.label}:${items.length}`;
    item.search = `${item.label} ${item.detail || ""} ${item.keywords || ""}`.toLowerCase();
    items.push(item);
  }

  function inventory() {
    const items = [];
    const view = ui.state.view || { directory: {}, activities: [] };
    for (const agent of view.directory.agents || []) {
      if (["pending_deletion", "deleted"].includes(agent.lifecycle)) continue;
      add(items, { group: "Coworkers", mark: (agent.display_name || "A").slice(0, 2), label: agent.display_name, detail: agent.role_title || "Coworker", keywords: `${agent.agent_id} ${agent.description || ""}`, action: () => ui.selectItem({ kind: "agent", id: agent.agent_id }) });
    }
    for (const group of view.directory.groups || []) {
      if (["pending_deletion", "deleted"].includes(group.lifecycle)) continue;
      add(items, { group: "Groups", mark: "#", label: group.name, detail: group.description || "Shared company room", keywords: group.group_id, action: () => ui.selectItem({ kind: "group", id: group.group_id }) });
    }
    for (const activity of view.activities || []) {
      const item = activity.item || {};
      for (const prompt of (activity.recent_prompts || []).slice(-4).reverse()) {
        const owner = ui.displayName(item) || (item.kind === "group" ? "Group" : "Coworker");
        add(items, { group: "Recent prompts", mark: "↳", label: prompt.preview || activity.title || "Recent prompt", detail: `Open ${owner}`, keywords: `${activity.title || ""} ${owner}`, action: () => ui.selectItem(item) });
      }
    }
    const settings = [
      ["General", "General settings", "Data, storage, and company defaults"], ["Models & Providers", "Models and providers", "Accounts, fallbacks, reasoning, context"], ["Usage & cost", "Usage and cost", "Tokens, cache, provider time, reported spend"], ["Permissions", "Permissions", "Approvals, shadow review, action firewall"], ["Skills", "Skills", "Installed playbooks"], ["Composio", "Composio and Figma", "Connected apps and OAuth"], ["MCP", "MCP servers", "Local and remote tool servers"], ["Remote runners", "Remote runners", "Pinned SSH machines for Workspace commands"], ["Memory", "Memory", "Librarian, retention, shared knowledge"], ["Workflows & Routines", "Workflows and routines", "Taught and recurring work"], ["Appearance", "Appearance", "Theme, type, motion, density"], ["Advanced", "Diagnostics and action ledger", "Runtime health, audit trail, relationships"],
    ];
    for (const [section, label, detail] of settings) add(items, { group: "Settings", mark: "S", label, detail, keywords: section, action: () => dispatchEvent(new CustomEvent("phoenix:open-settings", { detail: { section } })) });
    const commands = [
      ["+", "Create a coworker", "Add a permanent responsibility owner", () => ui.createItem("agent")],
      ["#", "Create a group", "Start a shared company room", () => ui.createItem("group")],
      [">_", "Toggle terminal", "Open or close the workspace terminal", () => window.PhoenixView?.toggleTerminal?.()],
      ["◎", "Toggle agent browser", "Open or close the selected coworker browser", () => window.PhoenixView?.toggleBrowser?.()],
      ["□", "Toggle full screen", "Use the whole display", () => window.PhoenixView?.toggleFullscreen?.()],
    ];
    for (const [mark, label, detail, action] of commands) add(items, { group: "Commands", mark, label, detail, action });
    return items;
  }

  function score(item, query) {
    if (!query) return 1;
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.every((term) => item.search.includes(term))) return 0;
    const label = item.label.toLowerCase();
    return terms.reduce((total, term) => total + (label.startsWith(term) ? 12 : label.includes(term) ? 6 : 2), 0);
  }

  function render() {
    const query = state.query.trim();
    state.visible = inventory().map((item) => ({ item, score: score(item, query) })).filter((row) => row.score > 0).sort((a, b) => b.score - a.score || a.item.group.localeCompare(b.item.group) || a.item.label.localeCompare(b.item.label)).slice(0, query ? 36 : 24).map((row) => row.item);
    state.index = Math.max(0, Math.min(state.index, state.visible.length - 1));
    state.actions.clear();
    if (!state.visible.length) { results.innerHTML = `<div class="command-palette-empty">Nothing in Phoenix matches “${escape(query)}”.</div>`; return; }
    let lastGroup = "";
    results.innerHTML = state.visible.map((item, index) => {
      state.actions.set(item.id, item.action);
      const group = item.group === lastGroup ? "" : `<div class="command-palette-group">${escape(item.group)}</div>`;
      lastGroup = item.group;
      return `${group}<button type="button" class="command-palette-item ${index === state.index ? "active" : ""}" data-command-id="${escape(item.id)}" data-command-index="${index}" role="option" aria-selected="${index === state.index}"><span class="command-palette-mark">${escape(item.mark || "·")}</span><span class="command-palette-copy"><strong>${escape(item.label)}</strong><small>${escape(item.detail || "")}</small></span></button>`;
    }).join("");
    results.querySelector(".active")?.scrollIntoView({ block: "nearest" });
  }

  function open(query = "") {
    if (document.body.classList.contains("onboarding-open")) return;
    ui.closeLayers?.();
    window.PhoenixSettings?.close?.();
    state.open = true; state.query = query; state.index = 0;
    host.hidden = false; document.body.classList.add("command-palette-open");
    input.value = query; render();
    requestAnimationFrame(() => input.focus());
  }

  function close() {
    if (!state.open) return;
    state.open = false; host.hidden = true; document.body.classList.remove("command-palette-open");
  }

  function run(index = state.index) {
    const item = state.visible[index];
    if (!item) return;
    const action = item.action; close();
    Promise.resolve().then(action).catch((error) => ui.toast(error?.message || String(error), true));
  }

  input.addEventListener("input", () => { state.query = input.value; state.index = 0; render(); });
  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); const direction = event.key === "ArrowDown" ? 1 : -1; state.index = (state.index + direction + state.visible.length) % Math.max(1, state.visible.length); render(); }
    else if (event.key === "Enter") { event.preventDefault(); run(); }
    else if (event.key === "Escape") { event.preventDefault(); close(); }
  });
  results.addEventListener("mousemove", (event) => { const button = event.target.closest("[data-command-index]"); if (!button) return; const index = Number(button.dataset.commandIndex); if (index !== state.index) { state.index = index; render(); } });
  results.addEventListener("click", (event) => { const button = event.target.closest("[data-command-index]"); if (button) run(Number(button.dataset.commandIndex)); });
  host.addEventListener("mousedown", (event) => { if (event.target === host) close(); });
  addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") { event.preventDefault(); state.open ? close() : open(); return; }
    if (state.open && event.key === "Escape") { event.preventDefault(); close(); }
  });
  window.PhoenixCommandPalette = Object.freeze({ open, close });
  if (new URLSearchParams(location.search).get("shot") === "palette") setTimeout(() => open(), 180);
})();
