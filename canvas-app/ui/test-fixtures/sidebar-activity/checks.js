// Browser fixture for the production sidebar. Load after index.html?shot=group.
// Synthetic directory projections only: no gateway, model or persistent writes.
window.runSidebarAcceptance = async () => {
  const ui = window.PhoenixUI, s = ui.state, checks = [];
  if (ui.TAURI && !ui.SIDEBAR_PREVIEW) throw new Error("Use the isolated browser preview for this fixture.");
  const check = (name, run) => {
    try { checks.push({ name, ok: true, detail: run() }); }
    catch (error) { checks.push({ name, ok: false, error: String(error.message || error) }); }
  };
  const expect = (condition, message) => { if (!condition) throw new Error(message); };
  const row = id => document.querySelector(`.company-row[data-id="${id}"]`);
  const selected = { kind: "group", id: "launch-room" };
  const fixture = {
    scribe: ["waiting_user", "Approve the draft before sending"],
    finance: ["failed", "Provider sign-in needs attention"],
    coder: ["working", "Running the acceptance checks"],
    frontend: ["integrating", "Combining the team's contributions"],
    researcher: ["waiting_peer", "Waiting for a source review"],
    presentation: ["queued", "Prepare the release notes"],
    critic: ["completed_unverified", "The result still needs review"],
    marketing: ["starting", "Preparing the campaign workspace"],
  };
  s.view = mockView();
  for (const [id, [status, label]] of Object.entries(fixture)) {
    Object.assign(s.view.activities.find(a => a.item.id === id), {
      status, activity_label: label, title: "Previous request", unread: true,
    });
  }
  s.selected = selected; s.query = ""; s.workingOnly = false; s.unreadOnly = false; s.showArchived = false;
  s.collapsedSections.clear(); render();

  check("waiting input is visible with its reason", () => {
    const title = row("scribe").querySelector(".row-title").textContent;
    expect(title.includes("Waiting for you") && title.includes("Approve the draft"), title);
    expect(!row("scribe").classList.contains("working"), "Waiting has a running indicator");
    expect(row("scribe").title.includes(title), "Truncated status has no full tooltip");
    return title;
  });
  check("failed and unverified work keep attention labels", () => {
    expect(row("finance").textContent.includes("Needs attention"), row("finance").textContent);
    expect(row("critic").textContent.includes("Needs review"), row("critic").textContent);
  });
  check("peer waiting is distinct from running", () => {
    expect(row("researcher").textContent.includes("Waiting for a coworker"), row("researcher").textContent);
    expect(!row("researcher").classList.contains("working"), "Peer wait looks active");
  });
  check("working filter includes starting and integrating", () => {
    try {
      s.workingOnly = true; render();
      expect(row("frontend") && row("marketing"), "Active coworkers disappeared");
      expect(!row("scribe") && !row("researcher"), "Waiting coworkers counted as running");
    } finally { s.workingOnly = false; render(); }
  });
  check("refresh preserves focused Options and sidebar scroll", () => {
    const list = document.getElementById("sidebarList");
    const oldHeight = list.style.maxHeight;
    try {
      list.style.maxHeight = "210px";
      row("scribe").querySelector(".row-more").focus({ preventScroll: true });
      list.scrollTop = 30;
      expect(list.scrollTop === 30, "Fixture did not exercise actual overflow");
      render();
      expect(document.activeElement === row("scribe").querySelector(".row-more"), "Focus was lost on refresh");
      expect(Math.abs(list.scrollTop - 30) < 1, "Scroll changed on refresh");
    } finally { list.style.maxHeight = oldHeight; }
  });
  check("a disappearing focused coworker leaves focus in its section", () => {
    const profile = s.view.directory.agents.find(p => p.agent_id === "researcher");
    try {
      row("researcher").focus({ preventScroll: true });
      profile.lifecycle = "archived"; render();
      expect(!row("researcher"), "Archived row remained visible");
      expect(document.activeElement === document.querySelector('[data-section="Coworkers"] .section-toggle'), "Focus fell out of the sidebar");
    } finally { profile.lifecycle = "active"; render(); }
  });
  check("Options keys preserve native activation without selecting the row", () => {
    for (const key of ["Enter", " "]) {
      s.selected = selected;
      const button = row("scribe").querySelector(".row-more");
      const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
      button.dispatchEvent(event);
      expect(s.selected.id === selected.id, `${key} opened the conversation`);
      expect(!event.defaultPrevented, `${key} prevented native button activation`);
    }
  });
  check("row Enter still opens its conversation", () => {
    row("scribe").dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    expect(s.selected.id === "scribe", "Enter no longer selects the row");
    s.selected = selected; render();
  });
  // selectItem also resolves a read-marker mutation; let it settle before the
  // following focus assertions and async polling checks touch the directory.
  await new Promise(resolve => setTimeout(resolve, 0));
  check("section disclosure keeps focus and accessible state", () => {
    const toggle = () => document.querySelector('[data-section="Coworkers"] .section-toggle');
    expect(toggle().getAttribute("aria-expanded") === "true", "Expanded section lacks state");
    expect(toggle().getAttribute("aria-label") === "Collapse Coworkers", "Section name missing");
    toggle().focus({ preventScroll: true }); toggle().click();
    expect(toggle().getAttribute("aria-expanded") === "false", "Collapsed state is wrong");
    expect(toggle().getAttribute("aria-label") === "Expand Coworkers", "Collapsed label is wrong");
    expect(document.activeElement === toggle(), "Collapse lost focus");
    toggle().click();
  });
  check("refresh does not steal composer focus", () => {
    const composer = document.getElementById("composerInput");
    composer.focus({ preventScroll: true }); render();
    expect(document.activeElement === composer, "Focus outside the sidebar was stolen");
  });

  const originalNotify = window.notify, recorded = [];
  const transition = (from, to, options = {}) => {
    recorded.length = 0; s.completionNotifications.clear(); s.recentCompletionItems.clear();
    const item = { kind: "agent", id: "researcher" };
    const before = { item, status: from, unread: false, transcript_revision: 7, title: "Review the sources" };
    const after = { ...before, status: to, unread: true, transcript_revision: 8, ...options };
    s.selected = selected;
    notifyActivityTransitions({ activities: [before] }, { activities: [after] });
    return { before, after };
  };
  window.notify = notice => recorded.push(notice);
  try {
    check("idle without a newer transcript is not a completion", () => {
      transition("working", "idle", { transcript_revision: 7 });
      expect(recorded.length === 0, JSON.stringify(recorded));
    });
    check("idle with a newer transcript is a neutral update", () => {
      transition("working", "idle");
      expect(recorded.length === 1 && recorded[0].title === "Theo has an update", JSON.stringify(recorded));
    });
    check("a new question notifies before the unread projection arrives", () => {
      transition("working", "waiting_user", { unread: false, transcript_revision: 7 });
      expect(recorded.length === 1 && recorded[0].title === "Theo needs your input", JSON.stringify(recorded));
    });
    check("failed and unverified notices never announce success", () => {
      transition("working", "failed");
      expect(recorded.length === 1 && recorded[0].title === "Theo needs attention" && recorded[0].error, JSON.stringify(recorded));
      transition("working", "completed_unverified");
      expect(recorded.length === 1 && recorded[0].title === "Theo needs review", JSON.stringify(recorded));
    });
    check("only an explicit verified completion announces finished", () => {
      transition("reviewing", "completed_verified");
      expect(recorded.length === 1 && recorded[0].title === "Theo finished", JSON.stringify(recorded));
    });
    check("peer waiting, queued work and archiving are not completions", () => {
      for (const status of ["waiting_peer", "queued", "integrating", "archived", "pending_deletion"]) {
        transition("working", status);
        expect(recorded.length === 0, `${status}: ${JSON.stringify(recorded)}`);
      }
    });
    check("duplicate snapshots notify once", () => {
      const { before, after } = transition("working", "waiting_user");
      notifyActivityTransitions({ activities: [before] }, { activities: [after] });
      expect(recorded.length === 1, JSON.stringify(recorded));
    });
    check("a blocker after a question is not hidden at the same revision", () => {
      const { after } = transition("working", "waiting_user");
      notifyActivityTransitions({ activities: [after] }, { activities: [{ ...after, status: "failed" }] });
      expect(recorded.length === 2 && recorded[1].title === "Theo needs attention", JSON.stringify(recorded));
      expect(recorded[1].key.startsWith("activity:"), "Attention used the completion cooldown");
    });
    check("selected conversations do not notify themselves", () => {
      const { before, after } = transition("working", "idle");
      recorded.length = 0; s.completionNotifications.clear(); s.selected = after.item;
      notifyActivityTransitions({ activities: [before] }, { activities: [after] });
      expect(recorded.length === 0, JSON.stringify(recorded));
      s.selected = selected;
    });
  } finally { window.notify = originalNotify; }

  check("dismissing a toast by keyboard does not open the conversation", () => {
    s.selected = selected;
    const toast = originalNotify({ title: "Fixture update", item: { kind: "agent", id: "researcher" } });
    try {
      const button = toast.querySelector("button");
      for (const key of ["Enter", " "]) {
        const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
        button.dispatchEvent(event);
        expect(s.selected.id === selected.id && !event.defaultPrevented, `${key}: dismiss key activated the conversation`);
      }
      button.click();
      expect(!toast.isConnected, "Dismiss did not remove the toast");
    } finally { toast.remove(); }
  });
  check("read markers, provider routes and team changes trigger refresh", () => {
    expect(typeof directoryViewChanged === "function", "No shared directory refresh comparison");
    const before = structuredClone(s.view);
    expect(!directoryViewChanged(before, structuredClone(before)), "Equal snapshot triggers refresh");
    for (const [key, value] of Object.entries({ unread: false, last_read_revision: 50, provider_id: "different-provider", model: "different-model", active_agent_ids: ["researcher", "critic"] })) {
      const after = structuredClone(before);
      Object.assign(after.activities.find(a => a.item.id === "researcher"), { [key]: value });
      expect(directoryViewChanged(before, after), `${key} was ignored`);
    }
  });
  const originalCommand = window.directoryCommand;
  const events = [];
  const onStatus = event => events.push(event.detail.view);
  addEventListener("phoenix:directory-status", onStatus);
  try {
    let reply;
    window.directoryCommand = () => new Promise(resolve => { reply = resolve; });
    const oldView = structuredClone(s.view), pending = refreshDirectoryActivity();
    const newer = structuredClone(s.view);
    newer.activities.find(a => a.item.id === "researcher").unread = false;
    s.view = newer;
    reply(oldView); await pending;
    check("an older poll cannot undo a newer local read marker", () => {
      expect(s.view === newer && !s.view.activities.find(a => a.item.id === "researcher").unread, "Old poll replaced local change");
      expect(events.length === 0, "Rejected poll still published an event");
    });
    const fresh = structuredClone(s.view);
    fresh.activities.find(a => a.item.id === "researcher").model = "new-route";
    window.directoryCommand = async () => fresh;
    await refreshDirectoryActivity();
    check("a current poll refreshes route and publishes one directory event", () => {
      expect(s.view === fresh && events.length === 1 && events[0] === fresh, "Fresh poll did not publish");
      expect(row("researcher").querySelector(".provider-chip").title.includes("new-route"), "Visible provider route is stale");
    });
  } catch (error) {
    checks.push({ name: "directory polling lifecycle", ok: false, error: String(error.message || error) });
  } finally {
    window.directoryCommand = originalCommand;
    removeEventListener("phoenix:directory-status", onStatus);
  }
  s.selected = selected; s.collapsedSections.clear(); render();
  document.getElementById("toastRegion").replaceChildren();
  document.getElementById("sidebarList").scrollTop = 0;
  window.dispatchEvent(new CustomEvent("phoenix:select-conversation", { detail: { item: selected, sessionId: "group-launch-room" } }));
  check("document does not gain horizontal overflow", () => {
    expect(document.documentElement.scrollWidth <= innerWidth, `Overflow ${document.documentElement.scrollWidth}/${innerWidth}`);
  });
  check("sidebar rows and attention markers fit their container", () => {
    const side = document.getElementById("companySidebar").getBoundingClientRect();
    for (const item of document.querySelectorAll(".company-row")) {
      const rect = item.getBoundingClientRect();
      expect(rect.left >= side.left && rect.right <= side.right, `${item.dataset.id} exceeds the sidebar`);
      if (["attention", "error"].includes(item.dataset.activityTone)) {
        const avatar = item.querySelector(".agent-avatar"), marker = getComputedStyle(avatar, "::after");
        expect(marker.content === '"!"', `${item.dataset.id} has no collapsed attention indicator`);
        expect(avatar.getBoundingClientRect().right + 3 < rect.right, `${item.dataset.id} clips attention marker`);
      }
    }
  });
  return { ok: checks.every(c => c.ok), theme: document.documentElement.dataset.theme, width: innerWidth, checks };
};
