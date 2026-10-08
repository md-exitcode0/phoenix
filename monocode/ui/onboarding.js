"use strict";

(() => {
  const ui = window.PhoenixUI;
  if (!ui) return;
  const $ = (id) => document.getElementById(id);
  const params = new URLSearchParams(location.search);
  const shot = params.get("shot");
  const preview = shot === "onboarding";
  const replay = params.get("onboarding") === "replay";
  const shellFixture = Boolean(shot) && !preview;
  const state = {
    snapshot: null,
    settingsSnapshot: null,
    powersSnapshot: null,
    inspection: null,
    recoveryKey: null,
    busy: false,
    error: "",
    selectedCompany: null,
    screen: null,
    lastScreen: null,
    direction: 1,
    runtimeError: "",
  };
  const backIcon = '<svg viewBox="0 0 20 20"><path d="m12 5-5 5 5 5"/></svg>';
  const nextIcon = '<svg viewBox="0 0 20 20"><path d="m8 5 5 5-5 5"/></svg>';
  const arrow = nextIcon;
  const optionEnd = `<span class="option-end">${nextIcon}${ui.checkIcon}</span>`;
  const shield = `<svg viewBox="0 0 20 20"><path d="M10 2.5 16 5v4.8c0 3.6-2.1 6.1-6 7.7-3.9-1.6-6-4.1-6-7.7V5l6-2.5Z"/><g transform="translate(3.25 4.05) scale(.64)">${ui.checkIcon}</g></svg>`;
  const company = '<svg viewBox="0 0 20 20"><path d="M3 17V7l7-4 7 4v10M7 17v-6h6v6M7 8h.1M13 8h.1"/></svg>';
  const escape = (value) => ui.escapeHtml(value);

  function setOnboardingOpen(open) {
    const view = $("onboardingView");
    const appShell = $("appShell");
    const wasOpen = document.body.classList.contains("onboarding-open");
    if (view) view.hidden = !open;
    document.body.classList.toggle("onboarding-open", open);
    if (appShell) {
      appShell.toggleAttribute("inert", open);
      if (open) appShell.setAttribute("aria-hidden", "true");
      else appShell.removeAttribute("aria-hidden");
    }
    if (open && !wasOpen) {
      window.PhoenixSettings?.close?.();
      ui.closeLayers?.();
    }
    $("viewMenuButton")?.setAttribute("aria-expanded", "false");
    window.dispatchEvent(new CustomEvent("phoenix:onboarding-visibility", { detail: { open } }));
  }

  function initialMock() {
    return {
      state: {
        version: 1,
        company_choice: null,
        default_account_email: null,
        cookie_import: null,
        provider_verification: null,
        account_email_skipped: false,
        company_defaults_reviewed_at: null,
        powers_setup_reviewed_at: null,
        created_at: new Date().toISOString(),
        updated_at: new Date().toISOString(),
        completed_at: null,
      },
      complete: false,
      provider_configured: true,
      provider_ready: false,
      configured_provider: "openai-codex",
      configured_model: "gpt-5.6-sol",
      vault_status: "uninitialized",
      agents: [{ agent_id: "phoenix", display_name: "Phoenix", role_title: "Chief of Staff", color: "#e55732", icon_seed: "phoenix", lifecycle: "active" }],
      supported_cookie_sources: ["firefox", "zen", "chrome", "chromium", "brave", "edge"],
      required_actions: ["choose_company", "verify_provider", "initialize_vault", "set_default_account_email", "review_company_defaults", "choose_cookie_import", "review_powers_setup"],
    };
  }

  function initialSettingsMock() {
    const setting = (key, value) => ({ definition: { key }, value });
    return { revision: 1, scope: { kind: "global" }, settings: [
      setting("appearance.theme", localStorage.getItem("phoenix-theme") || "light"),
      setting("composer.default_permission", "workspace"),
      setting("browser.enabled", true),
      setting("notifications.enabled", true),
      // Phoenix's conversation surface is the primary notification relay.
      // Native notifications stay available as an explicit Settings opt-in.
      setting("notifications.system", false),
      setting("agents.volume_workers_enabled", true),
      setting("agents.volume_worker_limit", 8),
    ] };
  }

  function rpc(command) {
    if (preview) return mockRpc(command);
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(ui.wsUrl());
      let done = false;
      const finish = (fn, value) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        try { ws.close(); } catch {}
        fn(value);
      };
      const timer = setTimeout(() => finish(reject, new Error("Onboarding did not answer in time.")), 30000);
      ws.onopen = () => ws.send(JSON.stringify({ Onboarding: command }));
      ws.onerror = () => finish(reject, new Error("Could not reach the Phoenix onboarding runtime."));
      ws.onmessage = (event) => {
        try {
          const value = JSON.parse(event.data);
          value.Error ? finish(reject, new Error(value.Error.message)) : finish(resolve, value.Onboarding);
        } catch (error) {
          finish(reject, error);
        }
      };
    });
  }

  function mockAgents(full) {
    const base = ui.state.view?.directory.agents || [];
    return (full ? base : base.filter((a) => a.agent_id === "phoenix")).map((a) => ({
      agent_id: a.agent_id, display_name: a.display_name, role_title: a.role_title, color: a.color, icon_seed: a.icon_seed, lifecycle: a.lifecycle,
    }));
  }

  function mockRpc(command) {
    const snap = state.snapshot || initialMock();
    switch (command.action) {
      case "status": break;
      case "choose_company":
        snap.state.company_choice = command.choice;
        snap.agents = mockAgents(command.choice === "founding_company");
        snap.required_actions = snap.required_actions.filter((x) => x !== "choose_company");
        break;
      case "verify_provider":
      case "accept_detected_provider":
        snap.provider_ready = true;
        snap.state.provider_verification = { provider: snap.configured_provider || "openai", model: snap.configured_model || "gpt-5.6", verified_at: new Date().toISOString() };
        snap.required_actions = snap.required_actions.filter((x) => x !== "verify_provider" && x !== "configure_provider");
        break;
      case "set_default_account_email":
        snap.state.default_account_email = command.email;
        snap.state.account_email_skipped = false;
        snap.required_actions = snap.required_actions.filter((x) => x !== "set_default_account_email");
        break;
      case "skip_default_account_email":
        snap.state.account_email_skipped = true;
        snap.required_actions = snap.required_actions.filter((x) => x !== "set_default_account_email");
        break;
      case "review_company_defaults":
        snap.state.company_defaults_reviewed_at = new Date().toISOString();
        snap.required_actions = snap.required_actions.filter((x) => x !== "review_company_defaults");
        break;
      case "review_powers_setup":
        snap.state.powers_setup_reviewed_at = new Date().toISOString();
        snap.required_actions = snap.required_actions.filter((x) => x !== "review_powers_setup");
        break;
      case "inspect_cookie_source":
        return Promise.resolve({ result: "cookie_source", source: command.source, portable_cookie_count: 128, device_bound_cookie_count: 4, portable_sites: ["github.com", "google.com", "linear.app", "notion.so", "x.com"], omitted_site_count: 12 });
      case "set_cookie_import":
        snap.state.cookie_import = { choice: "all_portable", source: command.source, agent_ids: command.agent_ids, portable_cookie_count: 128, device_bound_cookie_count: 4, granted_at: new Date().toISOString() };
        snap.required_actions = snap.required_actions.filter((x) => x !== "choose_cookie_import");
        break;
      case "set_cookie_import_automatic":
        snap.state.cookie_import = { choice: "all_portable", source: "zen", agent_ids: snap.agents.map((agent) => agent.agent_id), company_wide: true, portable_cookie_count: 128, device_bound_cookie_count: 4, granted_at: new Date().toISOString() };
        snap.required_actions = snap.required_actions.filter((x) => x !== "choose_cookie_import");
        break;
      case "skip_cookie_import":
        snap.state.cookie_import = { choice: "skipped", decided_at: new Date().toISOString() };
        snap.required_actions = snap.required_actions.filter((x) => x !== "choose_cookie_import");
        break;
      case "complete":
        snap.complete = true;
        snap.state.completed_at = new Date().toISOString();
        break;
    }
    state.snapshot = snap;
    return Promise.resolve({ result: "snapshot", ...snap });
  }

  function settingsRpc(command) {
    if (preview) {
      const snap = state.settingsSnapshot || initialSettingsMock();
      if (command.action === "set") {
        const row = snap.settings.find((entry) => entry.definition.key === command.key);
        if (row) row.value = command.value;
        snap.revision += 1;
      }
      state.settingsSnapshot = snap;
      return Promise.resolve({ result: "snapshot", snapshot: snap });
    }
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(ui.wsUrl());
      let done = false;
      const finish = (fn, value) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        try { ws.close(); } catch {}
        fn(value);
      };
      const timer = setTimeout(() => finish(reject, new Error("Settings did not answer in time.")), 20000);
      ws.onopen = () => ws.send(JSON.stringify({ Settings: command }));
      ws.onerror = () => finish(reject, new Error("Could not reach company settings."));
      ws.onmessage = (event) => {
        try {
          const value = JSON.parse(event.data);
          value.Error ? finish(reject, new Error(value.Error.message)) : finish(resolve, value.Settings);
        } catch (error) {
          finish(reject, error);
        }
      };
    });
  }

  async function vault(command) {
    if (preview) {
      if (command.action === "initialize") return { result: "recovery_key", recovery_key: "PHX-RECOVERY-7ZQ4-K2MF-9HXA" };
      return { result: "unlocked" };
    }
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(ui.wsUrl());
      let done = false;
      const finish = (fn, value) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        try { ws.close(); } catch {}
        fn(value);
      };
      const timer = setTimeout(() => finish(reject, new Error("Vault did not answer in time.")), 20000);
      ws.onopen = () => ws.send(JSON.stringify({ Vault: command }));
      ws.onerror = () => finish(reject, new Error("Could not reach the secure vault."));
      ws.onmessage = (event) => {
        try {
          const value = JSON.parse(event.data);
          value.Error ? finish(reject, new Error(value.Error.message)) : finish(resolve, value.Vault);
        } catch (error) {
          finish(reject, error);
        }
      };
    });
  }

  function unwrap(reply) { return reply?.snapshot || reply; }
  function detectedCoworkers() { return (state.snapshot?.agents || []).length > 1; }
  function firstIncomplete() {
    const snap = state.snapshot;
    if (!snap?.state.company_choice) return 0;
    if (!snap.provider_ready) return 1;
    if (["uninitialized","unprotected"].includes(snap.vault_status) || state.recoveryKey) return 2;
    if (!snap.state.default_account_email && !snap.state.account_email_skipped) return 3;
    if (!snap.state.company_defaults_reviewed_at) return 4;
    if (!snap.state.cookie_import) return 5;
    if (!snap.state.powers_setup_reviewed_at) return 6;
    // Chat apps are optional; the step shows once after tools, then Ready.
    if (!state.channelsReviewed) return 7;
    return 8;
  }
  function currentScreen() { return state.screen ?? firstIncomplete(); }

  function progress() {
    const current = currentScreen();
    return `<div class="onboarding-progress">${[0, 1, 2, 3, 4, 5, 6, 7, 8].map((i) => `<i class="${i < current ? "done" : i === current ? "current" : ""}"></i>`).join("")}</div>`;
  }

  function navBar() {
    return `<div class="onboarding-nav"><button type="button" id="onboardBack" class="button secondary onboard-back" ${currentScreen() === 0 ? "disabled" : ""}>${backIcon}<span>Back</span></button>${progress()}</div>`;
  }

  function brand() {
    return `<aside class="onboarding-brand"><div class="onboarding-logo">${ui.avatarSvg({ agent_id: "phoenix", color: "#e55732", icon_seed: "phoenix" })}<span><strong>Phoenix</strong><small>your company</small></span></div><div class="onboarding-promise"><h1>A company that learns how you work.</h1><p>Your coworkers share context when it helps, keep private work private, and get sharper every time you correct them.</p></div><span class="onboarding-trust">${shield} Local-first · encrypted credentials · explicit authority</span></aside>`;
  }

  function ensureShell() {
    const view = $("onboardingView");
    if (view.querySelector(".onboarding-stage")) return view.querySelector(".onboarding-stage");
    view.innerHTML = `${brand()}<main class="onboarding-main"><div class="onboarding-stage"></div></main>`;
    return view.querySelector(".onboarding-stage");
  }

  function fillCard(card, content, bind) {
    card.innerHTML = `${navBar()}${content}${state.error ? `<div class="onboarding-error">${escape(state.error)}</div>` : ""}`;
    card.querySelector("#onboardBack")?.addEventListener("click", (event) => {
      event.preventDefault();
      go(currentScreen() - 1, -1);
    });
    bind?.(card);
  }

  function shell(content, bind) {
    const stage = ensureShell();
    const screen = currentScreen();
    const live = [...stage.querySelectorAll(".onboarding-card")].find((card) => !card.classList.contains("exit-left") && !card.classList.contains("exit-right"));
    const sameScreen = live && state.lastScreen === screen;
    state.lastScreen = screen;
    if (!sameScreen && state.direction !== 0) stage.closest(".onboarding-main").scrollTop = 0;
    if (sameScreen || state.direction === 0) {
      const card = live || Object.assign(document.createElement("section"), { className: "onboarding-card in" });
      if (!live) stage.appendChild(card);
      fillCard(card, content, bind);
      return;
    }
    const incoming = document.createElement("section");
    incoming.className = `onboarding-card enter-${state.direction > 0 ? "right" : "left"}`;
    fillCard(incoming, content, bind);
    const outgoing = live;
    stage.appendChild(incoming);
    requestAnimationFrame(() => {
      incoming.classList.add("in");
      outgoing?.classList.add(state.direction > 0 ? "exit-left" : "exit-right");
    });
    setTimeout(() => outgoing?.remove(), 280);
  }

  function go(screen, direction = 1) {
    state.direction = direction;
    state.screen = Math.max(0, Math.min(8, screen));
    state.error = "";
    render();
  }

  function render() {
    const current = currentScreen();
    if (current === 0) renderCompany();
    else if (current === 1) renderProvider();
    else if (current === 2) renderVault();
    else if (current === 3) renderEmail();
    else if (current === 4) renderDefaults();
    else if (current === 5) renderCookies();
    else if (current === 6) renderPowers();
    else if (current === 7) renderChannels();
    else renderReady();
  }

  function autodetectNote(text) {
    return `<div class="onboarding-note detect">${shield}<span>${text}</span></div>`;
  }

  function renderCompany() {
    const selected = state.selectedCompany;
    const existing = detectedCoworkers();
    shell(`<span class="onboarding-step-label">Your company</span><h2>How do you want to begin?</h2><p class="onboarding-lead">Phoenix is always your chief of staff. Start with Phoenix alone, or bring in the founding company of 12 human-named coworkers. You can archive or add coworkers whenever you want.</p><div class="onboarding-options"><button type="button" class="onboarding-option ${selected === "founding_company" ? "selected" : ""}" data-company="founding_company"><span class="onboarding-option-mark">${company}</span><span><strong>Start with the founding company</strong><small>Phoenix plus 11 focused coworkers for inbox, calendar, research, engineering, product design, knowledge, operations, finance, relationships, publishing, and reliability.</small></span>${optionEnd}</button><button type="button" class="onboarding-option ${selected === "phoenix_only" ? "selected" : ""}" data-company="phoenix_only"><span class="onboarding-option-mark">${ui.avatarSvg({ agent_id: "phoenix", color: "#e55732", icon_seed: "phoenix" })}</span><span><strong>Start with Phoenix</strong><small>A clean company with one chief of staff. Phoenix can create each coworker with you later, usually in about a minute.</small></span>${optionEnd}</button></div>${existing ? autodetectNote(`Detected ${state.snapshot.agents.length} coworkers already on this computer. Skip keeps them and continues.`) : ""}<div class="onboarding-actions"><button type="button" id="companySkip" class="button secondary">Skip</button><button type="button" id="companyContinue" class="button primary" ${selected ? "" : "disabled"}>Continue</button></div><div class="onboarding-note">${shield}<span>Existing Phoenix data is never cleared. If this computer already has canonical threads or memory, onboarding links them into the new company.</span></div>`, (root) => {
      root.querySelectorAll("[data-company]").forEach((button) => button.onclick = () => {
        state.selectedCompany = button.dataset.company;
        root.querySelectorAll("[data-company]").forEach((item) => item.classList.toggle("selected", item.dataset.company === state.selectedCompany));
        const next = root.querySelector("#companyContinue");
        if (next) next.disabled = !state.selectedCompany;
      });
      root.querySelector("#companyContinue").onclick = () => run({ action: "choose_company", choice: state.selectedCompany });
      root.querySelector("#companySkip").onclick = () => run({ action: "choose_company", choice: existing ? "founding_company" : "phoenix_only" });
    });
  }

  function renderProvider() {
    const snap = state.snapshot;
    if (!snap.provider_configured) {
      shell(`<span class="onboarding-step-label">AI provider</span><h2>Connect the intelligence behind your company</h2><p class="onboarding-lead">Phoenix supports local models, custom providers, API keys, and OAuth accounts. The provider setup uses the same secure account store as the rest of the app.</p><div class="onboarding-status"><span class="onboarding-status-mark">${shield}</span><span><strong>No provider configured yet</strong><small>Choose a provider, model, and account without leaving Phoenix.</small></span><button id="configureProvider" class="button primary">Set up</button></div><div class="onboarding-actions"><button id="refreshProvider" class="button secondary">Check again</button></div>`, (root) => {
        root.querySelector("#configureProvider").onclick = () => window.PhoenixProviderSetup.open({ initialize: true, onConnected: load });
        root.querySelector("#refreshProvider").onclick = load;
      });
      return;
    }
    const provider = snap.configured_provider || snap.state.provider_verification?.provider || "custom";
    const model = snap.configured_model || snap.state.provider_verification?.model || "Configured model";
    const expired = /expired|unauthori[sz]ed|sign.?in|oauth|token/i.test(state.error);
    shell(`<span class="onboarding-step-label">AI provider</span><h2>Make sure Phoenix can think</h2><p class="onboarding-lead">A tiny live request verifies the selected provider, model, and exact account. Or skip to accept the account already stored on this computer.</p><div class="onboarding-status"><span class="onboarding-status-mark">${ui.providerIcon(provider, model)}</span><span><strong>${escape(ui.providerLabel(provider))}</strong><small>${escape(model)} · detected on this computer.</small></span>${state.busy ? '<i class="onboarding-spinner"></i>' : `<span class="onboarding-status-actions"><button id="manageProvider" class="button secondary">${expired ? "Reconnect" : "Change"}</button><button id="verifyProvider" class="button primary">Verify</button></span>`}</div>${autodetectNote("Provider and model are already configured. Skip accepts them without a live probe.")}<div class="onboarding-actions"><button id="skipProvider" class="button secondary">Skip</button></div><div class="onboarding-note">${shield}<span>Local and custom providers work here too. You can assign every runtime lane and coworker separately later in Settings → Models.</span></div>`, (root) => {
      root.querySelector("#manageProvider")?.addEventListener("click", () => window.PhoenixProviderSetup.open({ initialize: true, onConnected: load }));
      root.querySelector("#verifyProvider")?.addEventListener("click", () => run({ action: "verify_provider" }));
      root.querySelector("#skipProvider")?.addEventListener("click", () => run({ action: "accept_detected_provider" }));
    });
  }

  function renderVault() {
    if (state.recoveryKey) {
      shell(`<span class="onboarding-step-label">Security</span><h2>Save your recovery key</h2><p class="onboarding-lead">This is the only time Phoenix shows it automatically. Keep it somewhere separate from this computer; it can unlock your local credential vault if you forget the master password.</p><div class="recovery-box"><code>${escape(state.recoveryKey)}</code></div><label class="onboarding-note ember-check-label"><input id="savedRecovery" class="ember-check-input" type="checkbox"><i class="ember-check-box" aria-hidden="true"></i><span>I saved the recovery key somewhere safe.</span></label><div class="onboarding-actions"><button id="recoveryContinue" class="button primary" disabled>Continue</button></div>`, (root) => {
        root.querySelector("#savedRecovery").onchange = (event) => { root.querySelector("#recoveryContinue").disabled = !event.target.checked; };
        root.querySelector("#recoveryContinue").onclick = async () => { state.recoveryKey = null; await load(); };
      });
      return;
    }
    const ready = !["uninitialized","unprotected"].includes(state.snapshot.vault_status);
    shell(`<span class="onboarding-step-label">Security</span><h2>${ready ? "Vault already on this computer" : "Protect every saved account"}</h2><p class="onboarding-lead">${ready ? "The local credential vault is already initialized. Continue to keep using it." : "Your master password encrypts website credentials locally. Agents only receive the scoped secret at the moment an approved login needs it."}</p>${ready ? "" : `<form id="vaultForm" class="onboarding-form"><label class="field"><span>Master password</span><span class="password-wrap"><input name="password" type="password" minlength="12" autocomplete="new-password" required placeholder="At least 12 characters"><button type="button" class="password-toggle" aria-pressed="false">Show</button></span></label><label class="field"><span>Confirm password</span><span class="password-wrap"><input name="confirm" type="password" minlength="12" autocomplete="new-password" required><button type="button" class="password-toggle" aria-pressed="false">Show</button></span></label><div class="onboarding-actions"><button class="button primary" type="submit">Create secure vault</button></div></form>`}${ready ? `${autodetectNote("A vault is already present. Continue uses it without creating another.")}<div class="onboarding-actions"><button id="vaultContinue" class="button primary">Continue</button></div>` : ""}<div class="onboarding-note">${shield}<span>Secrets never enter prompts, tool logs, workflow recordings, or settings data. Recovery keys and revealed values are zeroized after use.</span></div>`, (root) => {
      root.querySelector("#vaultSkip")?.addEventListener("click", () => go(3, 1));
      root.querySelector("#vaultContinue")?.addEventListener("click", () => go(3, 1));
      root.querySelectorAll(".password-toggle").forEach((button) => {
        button.onclick = () => {
          const input = button.previousElementSibling;
          if (!input) return;
          const show = input.type === "password";
          input.type = show ? "text" : "password";
          button.textContent = show ? "Hide" : "Show";
          button.setAttribute("aria-pressed", String(show));
        };
      });
      root.querySelector("#vaultForm")?.addEventListener("submit", async (event) => {
        event.preventDefault();
        const data = new FormData(event.currentTarget);
        const password = String(data.get("password"));
        const confirm = String(data.get("confirm"));
        if (password !== confirm) { state.error = "The two passwords do not match."; state.direction = 0; renderVault(); return; }
        setBusy(true);
        try {
          const reply = await vault({ action: "initialize", master_password: password });
          state.recoveryKey = reply.recovery_key;
          state.snapshot.vault_status = "locked";
          state.snapshot.required_actions = state.snapshot.required_actions.filter((action) => action !== "initialize_vault");
          state.error = "";
        } catch (error) {
          state.error = error.message;
        }
        setBusy(false);
        state.direction = 1;
        renderVault();
      });
    });
  }

  function renderEmail() {
    const existing = state.snapshot.state.default_account_email;
    shell(`<span class="onboarding-step-label">Default account</span><h2>What email should your company use?</h2><p class="onboarding-lead">When a coworker creates a free website account, this is the default address. If verification is needed, they can ask your communications coworker for the code.</p><form id="emailForm" class="onboarding-form"><label class="field"><span>Account email</span><input name="email" type="email" autocomplete="email" placeholder="you@example.com" value="${escape(existing || "")}"></label><div class="onboarding-actions"><button id="skipEmail" class="button secondary" type="button">Skip</button><button class="button primary" type="submit">Continue</button></div></form>${existing ? autodetectNote(`Already set to ${escape(existing)}. Continue keeps it, or skip and set it later.`) : ""}<div class="onboarding-note">${shield}<span>No purchases or paid plans happen automatically. Money always requires explicit approval.</span></div>`, (root) => {
      root.querySelector("#emailForm").onsubmit = (event) => {
        event.preventDefault();
        const email = new FormData(event.currentTarget).get("email");
        if (email) run({ action: "set_default_account_email", email });
        else run({ action: "skip_default_account_email" });
      };
      root.querySelector("#skipEmail").onclick = () => run({ action: "skip_default_account_email" });
    });
  }

  function defaultValue(key, fallback) {
    return state.settingsSnapshot?.settings.find((entry) => entry.definition.key === key)?.value ?? fallback;
  }

  function defaultToggle(key, label, description, fallback = true) {
    return `<label class="onboarding-default-row"><span><strong>${escape(label)}</strong><small>${escape(description)}</small></span><input class="onboarding-switch" type="checkbox" data-default-key="${escape(key)}" ${defaultValue(key, fallback) ? "checked" : ""}></label>`;
  }

  function defaultChoice(key, label, description, selected, options, variant = "") {
    const buttons = options.map(([value, text, mark, hint]) => `<button type="button" class="onboarding-choice ${String(value) === String(selected) ? "selected" : ""}" data-choice-value="${escape(value)}" data-hint="${escape(hint || text)}" role="radio" aria-checked="${String(value) === String(selected)}"><i aria-hidden="true">${mark || ""}</i><span>${escape(text)}</span></button>`).join("");
    return `<div class="onboarding-default-row onboarding-choice-row ${escape(variant)}-row"><span><strong>${escape(label)}</strong><small>${escape(description)}</small></span><div class="onboarding-choice-control ${escape(variant)}" role="radiogroup" aria-label="${escape(label)}"><input type="hidden" data-default-key="${escape(key)}" value="${escape(selected)}">${buttons}</div></div>`;
  }

  function workerStepper(value) {
    return `<div class="onboarding-default-row onboarding-choice-row worker-choice-row" data-worker-limit><span><strong>Worker ceiling</strong><small>Maximum temporary workers in one batch; named coworkers stay preferred.</small></span><div class="onboarding-stepper" aria-label="Worker ceiling"><input type="hidden" data-default-key="agents.volume_worker_limit" value="${escape(value)}"><button type="button" data-step="-1" data-hint="Lower the maximum number of temporary parallel workers." aria-label="Fewer volume workers">−</button><output>${escape(value)}</output><button type="button" data-step="1" data-hint="Raise the maximum number of temporary parallel workers." aria-label="More volume workers">+</button></div></div>`;
  }

  function renderDefaults() {
    if (!state.settingsSnapshot) {
      shell(`<span class="onboarding-step-label">Company defaults</span><h2>Loading practical defaults…</h2><p class="onboarding-lead">Phoenix is reading the real company settings so every choice on this page is connected.</p><div class="onboarding-loading"><i class="onboarding-spinner"></i><span>Reading settings</span></div>`, () => {});
      loadSettingsSnapshot().then(renderDefaults).catch((error) => {
        state.error = error.message;
        state.direction = 0;
        renderDefaultsUnavailable();
      });
      return;
    }
    const permission = defaultValue("composer.default_permission", "workspace");
    const theme = defaultValue("appearance.theme", "light");
    const workerLimit = defaultValue("agents.volume_worker_limit", 8);
    ui.setTheme(theme);
    const accessChoices = [
      ["talk", "Talk", "○", "Conversation only. Phoenix can think, remember, and answer, but cannot change files or use external tools."],
      ["workspace", "Workspace", "◇", "Work inside the workspace you choose. Anything outside that boundary asks for approval."],
      ["full_access", "Full access", "↗", "Use workspace, browser, terminal, and connected tools without routine approval prompts."],
    ];
    const themeChoices = [
      ["light", "Light", "", "A bright, paper-like Phoenix interface."],
      ["dark", "Dark", "", "A low-light ember interface with darker surfaces."],
      ["system", "Auto", "", "Follow your computer’s light or dark appearance automatically."],
    ];
    shell(`<span class="onboarding-step-label">Company defaults</span><h2>Set the way your company works</h2><p class="onboarding-lead">These are real global settings, not a tour. You can override models, access, browser behavior, and notifications per coworker or group later.</p><div class="onboarding-defaults">${defaultChoice("composer.default_permission", "Default access", "What a new message may do before asking.", permission, accessChoices, "access-choice")}${defaultChoice("appearance.theme", "Appearance", "Preview the whole app now; Auto follows your computer.", theme, themeChoices, "theme-choice")}${defaultToggle("browser.enabled", "Private browser tools", "Give every coworker an isolated managed browser profile.")}${defaultToggle("notifications.enabled", "Company notifications", "Show completions and requests that need your attention.")}${defaultToggle("notifications.system", "System notifications", "Use native Linux notifications when Phoenix is in the background.")}${defaultToggle("agents.volume_workers_enabled", "Volume workers", "Allow rare, independent batches to use temporary generic workers.")}${workerStepper(workerLimit)}</div><div class="onboarding-actions"><button id="recommendedDefaults" class="button secondary">Use recommended</button><button id="saveDefaults" class="button primary">Save and continue</button></div><div class="onboarding-note">${shield}<span>The temporary-worker model, reasoning, and fallbacks live in Settings → Models. All coworkers can use the same tools; their craft comes from memory and skills.</span></div>`, (root) => {
      const syncChoice = (control) => {
        const input = control.querySelector("[data-default-key]");
        control.querySelectorAll("[data-choice-value]").forEach((button) => {
          const selected = String(button.dataset.choiceValue) === String(input.value);
          button.classList.toggle("selected", selected);
          button.setAttribute("aria-checked", String(selected));
        });
      };
      root.querySelectorAll(".onboarding-choice-control").forEach((control) => {
        control.querySelectorAll("[data-choice-value]").forEach((button) => button.onclick = () => {
          const input = control.querySelector("[data-default-key]");
          input.value = button.dataset.choiceValue;
          syncChoice(control);
          if (input.dataset.defaultKey === "appearance.theme") ui.setTheme(input.value);
        });
      });
      const limits = [4, 8, 12, 16, 24, 32];
      root.querySelectorAll(".onboarding-stepper [data-step]").forEach((button) => button.onclick = () => {
        const stepper = button.closest(".onboarding-stepper");
        const input = stepper.querySelector("[data-default-key]");
        const index = Math.max(0, limits.indexOf(Number(input.value)));
        input.value = String(limits[Math.max(0, Math.min(limits.length - 1, index + Number(button.dataset.step)))]);
        stepper.querySelector("output").textContent = input.value;
      });
      const syncDependencies = () => {
        const workers = root.querySelector('[data-default-key="agents.volume_workers_enabled"]')?.checked;
        root.querySelector("[data-worker-limit]")?.classList.toggle("disabled", !workers);
        const limit = root.querySelector('[data-default-key="agents.volume_worker_limit"]');
        if (limit) limit.disabled = !workers;
      };
      root.querySelector('[data-default-key="agents.volume_workers_enabled"]')?.addEventListener("change", syncDependencies);
      root.querySelector('[data-default-key="notifications.enabled"]')?.addEventListener("change", (event) => {
        const system = root.querySelector('[data-default-key="notifications.system"]');
        if (system) system.disabled = !event.target.checked;
      });
      syncDependencies();
      root.querySelector("#recommendedDefaults").onclick = () => {
        root.querySelector('[data-default-key="composer.default_permission"]').value = "workspace";
        root.querySelector('[data-default-key="appearance.theme"]').value = "light";
        root.querySelectorAll('.onboarding-switch[data-default-key]').forEach((input) => { input.checked = true; input.disabled = false; });
        root.querySelector('[data-default-key="agents.volume_worker_limit"]').value = "8";
        root.querySelectorAll(".onboarding-choice-control").forEach(syncChoice);
        root.querySelector(".onboarding-stepper output").textContent = "8";
        ui.setTheme("light");
        syncDependencies();
      };
      root.querySelector("#saveDefaults").onclick = () => saveDefaults(root);
    });
  }

  function renderDefaultsUnavailable() {
    shell(`<span class="onboarding-step-label">Company defaults</span><h2>Settings are not ready yet.</h2><p class="onboarding-lead">Nothing was changed. Retry the live settings connection, then continue setup.</p><div class="onboarding-actions"><button id="retryDefaults" class="button primary">Retry settings</button></div>`, (root) => {
      root.querySelector("#retryDefaults").onclick = async () => {
        state.settingsSnapshot = null;
        state.error = "";
        renderDefaults();
      };
    });
  }

  async function loadSettingsSnapshot() {
    const reply = await settingsRpc({ action: "snapshot", scope: { kind: "global" } });
    state.settingsSnapshot = reply.snapshot;
    return state.settingsSnapshot;
  }

  async function saveDefaults(root) {
    if (state.busy) return;
    const button = root.querySelector("#saveDefaults");
    setBusy(true);
    button.disabled = true;
    button.textContent = "Saving…";
    try {
      const controls = [...root.querySelectorAll("[data-default-key]")];
      for (const control of controls) {
        if (control.disabled && control.dataset.defaultKey === "agents.volume_worker_limit") continue;
        const value = control.type === "checkbox" ? control.checked : control.dataset.defaultKey === "agents.volume_worker_limit" ? Number(control.value) : control.value;
        const current = defaultValue(control.dataset.defaultKey, undefined);
        if (current === value) continue;
        const reply = await settingsRpc({ action: "set", key: control.dataset.defaultKey, value, scope: { kind: "global" }, expected_revision: state.settingsSnapshot.revision });
        state.settingsSnapshot = reply.snapshot;
        if (control.dataset.defaultKey === "appearance.theme") ui.setTheme(value);
      }
      const receipt = await rpc({ action: "review_company_defaults" });
      state.snapshot = unwrap(receipt);
      state.direction = 1;
      state.screen = replay ? 5 : firstIncomplete();
      setBusy(false);
      render();
    } catch (error) {
      state.error = error.message;
      state.direction = 0;
      setBusy(false);
      renderDefaults();
    }
  }

  function renderCookies() {
    const snap = state.snapshot;
    const inspection = state.inspection;
    shell(`<span class="onboarding-step-label">Browser logins</span><h2>Give your coworkers the logins you already use</h2><p class="onboarding-lead">One click finds the browser you used most recently and gives every coworker its portable sessions. Each coworker still keeps a private browser profile.</p><button id="autoImportCookies" class="onboarding-option selected"><span class="onboarding-option-mark">${shield}</span><span><strong>Use my browser logins</strong><small>Phoenix selects the live profile automatically. Google and Microsoft device-bound sessions stay untouched.</small></span><span class="option-end">${arrow}<svg class="ember-check" viewBox="0 0 20 20"><path d="m5 10 3 3 7-7"/></svg></span></button><details class="cookie-advanced"><summary>Choose a browser or specific coworkers</summary><div class="onboarding-inline"><label class="field"><span>Browser</span><button type="button" class="menu-select" id="cookieSource" data-value="${escape(inspection?.source || "")}"><span>${inspection?.source ? escape(inspection.source[0].toUpperCase() + inspection.source.slice(1)) : "Choose a browser…"}</span><svg viewBox="0 0 20 20"><path d="m6 8 4 4 4-4"/></svg></button></label><button id="inspectCookies" class="button secondary">Inspect</button></div>${inspection ? `<section class="cookie-inspection"><header><strong>${escape(inspection.source)}</strong><span>${inspection.portable_cookie_count} portable cookies</span></header><div class="cookie-sites">${inspection.portable_sites.slice(0, 12).map((site) => `<span>${escape(site)}</span>`).join("")}${inspection.omitted_site_count ? `<span>+${inspection.omitted_site_count} sites</span>` : ""}</div><small>${inspection.device_bound_cookie_count} device-bound cookies will not be copied.</small></section><div class="onboarding-coworkers">${snap.agents.map((a) => `<label class="onboarding-coworker"><input class="ember-check-input" type="checkbox" name="cookieAgents" value="${escape(a.agent_id)}" checked><i class="ember-check-box" aria-hidden="true"></i><span class="mini-avatar">${ui.avatarSvg(a)}</span><span>${escape(a.display_name)}</span></label>`).join("")}</div><div class="onboarding-actions"><button id="importCookies" class="button secondary">Import selected</button></div>` : ""}</details><div class="onboarding-actions"><button id="skipCookies" class="button secondary">Not now</button><button id="autoImportContinue" class="button primary">Use browser logins</button></div><div class="onboarding-note">${shield}<span>This copies cookies only—not history, passwords, bookmarks, or autofill. Free accounts can be created automatically later with your company email.</span></div>`, (root) => {
      const sourceButton = root.querySelector("#cookieSource");
      sourceButton.onclick = (event) => {
        ui.openMenuSelect(event.currentTarget, snap.supported_cookie_sources.map((source) => [source, source[0].toUpperCase() + source.slice(1)]), sourceButton.dataset.value, (value) => {
          sourceButton.dataset.value = value;
          sourceButton.querySelector("span").textContent = value[0].toUpperCase() + value.slice(1);
        });
      };
      root.querySelector("#inspectCookies").onclick = () => inspectCookies(sourceButton.dataset.value);
      root.querySelector("#skipCookies").onclick = () => run({ action: "skip_cookie_import" });
      const automatic = () => run({ action: "set_cookie_import_automatic" });
      root.querySelector("#autoImportCookies").onclick = automatic;
      root.querySelector("#autoImportContinue").onclick = automatic;
      root.querySelector("#importCookies")?.addEventListener("click", () => {
        const ids = [...root.querySelectorAll('[name="cookieAgents"]:checked')].map((input) => input.value);
        if (!ids.length) { state.error = "Pick at least one coworker."; state.direction = 0; renderCookies(); return; }
        run({ action: "set_cookie_import", source: inspection.source, agent_ids: ids });
      });
    });
  }

  function powerPreview() {
    return {
      composio: { configured: false, hint: null, source: null },
      apps: [],
      servers: [],
      figmaApp: null,
      figmaMcp: null,
      tasteIncluded: true,
      warnings: [],
    };
  }

  async function loadPowersSnapshot(force = false) {
    if (state.powersSnapshot && !force) return state.powersSnapshot;
    if (preview) {
      state.powersSnapshot = powerPreview();
      return state.powersSnapshot;
    }
    const requests = ["composio_status", "composio_apps", "mcp_list"].map((command) => ui.invoke(command));
    const [composioResult, appsResult, mcpResult] = await Promise.allSettled(requests);
    const value = (result, fallback) => result.status === "fulfilled" ? result.value : fallback;
    const composio = value(composioResult, { configured: false, hint: null, source: null });
    const apps = value(appsResult, { apps: [] }).apps || [];
    const servers = value(mcpResult, { servers: [] }).servers || [];
    const active = (status) => ["active", "connected", "ready"].includes(String(status || "").toLowerCase());
    const mentions = (value, term) => String(value || "").toLowerCase().includes(term);
    const figmaApp = apps.find((app) => mentions(app.name, "figma") && active(app.status)) || null;
    const figmaMcp = servers.find((server) => server.enabled && [server.name, server.description, server.url].some((item) => mentions(item, "figma"))) || null;
    const warnings = [composioResult, appsResult, mcpResult]
      .filter((result) => result.status === "rejected")
      .map((result) => result.reason?.message || String(result.reason));
    state.powersSnapshot = { composio, apps, servers, figmaApp, figmaMcp, tasteIncluded: true, warnings };
    return state.powersSnapshot;
  }

  function instrumentRow(mark, name, kind, description, status, live = false) {
    return `<article class="studio-instrument ${live ? "live" : ""}"><span class="studio-wordmark" aria-hidden="true">${escape(mark)}</span><span><strong>${escape(name)}</strong><small>${escape(kind)} · ${escape(description)}</small></span><em>${live ? "●" : "○"} ${escape(status)}</em></article>`;
  }

  function renderPowers() {
    if (!state.powersSnapshot) {
      shell(`<span class="onboarding-step-label">Optional studio</span><h2>Give your design coworker real instruments.</h2><p class="onboarding-lead">Taste is already part of Phoenix. I’m checking Figma, Composio, and MCP against this machine before showing a status.</p><div class="onboarding-loading"><i class="onboarding-spinner"></i><span>Inspecting live powers</span></div>`, () => {});
      loadPowersSnapshot().then(() => { state.direction = 0; renderPowers(); }).catch((error) => { state.error = error.message; state.powersSnapshot = powerPreview(); state.direction = 0; renderPowers(); });
      return;
    }
    const powers = state.powersSnapshot;
    const figmaReady = Boolean(powers.figmaApp);
    const figmaConfigured = figmaReady || Boolean(powers.figmaMcp);
    const designAgent = (state.snapshot.agents || []).find((agent) => /design/i.test(`${agent.role_title} ${agent.display_name}`));
    const route = designAgent?.agent_id || "";
    const connectedCount = Number(figmaReady) + Number(Boolean(powers.figmaMcp)) + Number(Boolean(powers.composio.configured));
    shell(`<span class="onboarding-step-label">Optional studio</span><h2>Give your design coworker real instruments.</h2><p class="onboarding-lead">Phoenix ships with the judgment layer. Connect the workspace layer now, or leave it untouched and do this later in Settings.</p><div class="studio-score">${instrumentRow("Ta", "Taste", "Design judgment", "Mandatory before every visual build", "Included", true)}${instrumentRow("Fi", "Figma", "Shared design workspace", figmaReady ? "Live through Composio" : powers.figmaMcp ? "MCP endpoint is configured" : "Connect with Composio or your own MCP endpoint", figmaReady ? "Connected" : figmaConfigured ? "Configured" : "Optional", figmaReady)}</div><section class="studio-connect"><header><span><strong>${figmaReady ? "Figma is connected" : figmaConfigured ? "Figma is configured" : "Connect Figma"}</strong><small>${figmaReady ? `Composio reports ${escape(powers.figmaApp.status)}.` : powers.figmaMcp ? `MCP “${escape(powers.figmaMcp.name)}” is enabled; Phoenix will verify it when the agent calls it.` : "Composio is simplest for OAuth. MCP is best when you already operate an endpoint."}</small></span><i class="${figmaReady ? "live" : ""}">${figmaReady ? "live" : "optional"}</i></header>${!powers.composio.configured ? `<form id="onboardComposio" class="studio-key"><label><span>Composio consumer key</span><input name="key" type="password" autocomplete="off" placeholder="ck_…" required></label><button class="button primary" type="submit">Keep key</button></form>` : `<div class="studio-key-kept"><span><strong>Composio key on this machine</strong><small>${escape(powers.composio.hint || "ck_…")} · ${escape(powers.composio.source || "private file")}</small></span><button type="button" id="openComposio" class="button secondary">Manage apps</button></div>`}<div class="studio-connect-actions"><button type="button" id="openFigmaConnect" class="text-action">${figmaReady ? "Manage Figma in Composio" : "Connect Figma in Composio"} ↗</button><button type="button" id="refreshPowers" class="text-action">Verify again</button></div><details class="studio-mcp"><summary>Use a Figma MCP endpoint instead</summary><form id="onboardMcp"><label><span>Remote MCP URL</span><input name="url" type="url" placeholder="https://…/mcp" required></label><label><span>Bearer token <small>optional</small></span><input name="token" type="password" autocomplete="off"></label><button class="button secondary" type="submit">Keep endpoint</button></form><small>Phoenix stores the endpoint in config and routes it to ${escape(designAgent?.display_name || "the design coworker")}. “Configured” becomes “working” only after a real tool call succeeds.</small></details></section>${powers.warnings.length ? `<p class="studio-warning">Some status checks were unavailable: ${escape(powers.warnings[0])}</p>` : ""}<div class="onboarding-actions"><button id="skipPowers" class="button secondary">Not now</button><button id="continuePowers" class="button primary">${connectedCount ? "Keep setup and continue" : "Continue"}</button></div><div class="onboarding-note">${shield}<span>Connections are optional and reversible. Keys stay in Phoenix’s private files and never enter an agent transcript.</span></div>`, (root) => {
      const openDashboard = () => ui.invoke("open_external", { url: "https://app.composio.dev" }).catch(() => { state.error = "Could not open the Composio dashboard."; state.direction = 0; renderPowers(); });
      root.querySelector("#openComposio")?.addEventListener("click", openDashboard);
      root.querySelector("#openFigmaConnect")?.addEventListener("click", openDashboard);
      root.querySelector("#refreshPowers").onclick = async () => { state.powersSnapshot = null; state.direction = 0; renderPowers(); };
      root.querySelector("#onboardComposio")?.addEventListener("submit", async (event) => {
        event.preventDefault();
        const form = event.currentTarget;
        const button = form.querySelector("button");
        button.disabled = true;
        button.textContent = "Saving…";
        try {
          await ui.invoke("composio_key_set", { key: form.elements.key.value });
          state.powersSnapshot = null;
          state.error = "";
          state.direction = 0;
          renderPowers();
        } catch (error) {
          state.error = error.message || String(error);
          state.direction = 0;
          renderPowers();
        }
      });
      root.querySelector("#onboardMcp")?.addEventListener("submit", async (event) => {
        event.preventDefault();
        const form = event.currentTarget;
        const button = form.querySelector("button");
        button.disabled = true;
        button.textContent = "Saving…";
        try {
          await ui.invoke("mcp_upsert", { spec: { name: "figma", kind: "remote", command: null, url: form.elements.url.value, token: form.elements.token.value || null, route: route || null, description: "Figma design workspace", cwd: null } });
          state.powersSnapshot = null;
          state.error = "";
          state.direction = 0;
          renderPowers();
        } catch (error) {
          state.error = error.message || String(error);
          state.direction = 0;
          renderPowers();
        }
      });
      const continueSetup = () => run({ action: "review_powers_setup" });
      root.querySelector("#skipPowers").onclick = continueSetup;
      root.querySelector("#continuePowers").onclick = continueSetup;
    });
  }

  // Optional: reach coworkers from Telegram or Discord. Reuses the Settings
  // channel manager, so a channel added here is the same one listed there.
  function renderChannels() {
    shell(`<span class="onboarding-step-label">Optional · chat apps</span><h2>Talk to your coworkers from your phone.</h2><p class="onboarding-lead">Connect a Telegram bot or a Discord channel. Coworkers send final answers, questions and chosen images there; the full conversation stays in Phoenix.</p><div class="onboarding-channels" id="onboardChannels"></div><div class="onboarding-actions"><button id="skipChannels" class="button secondary">Not now</button><button id="continueChannels" class="button primary">Continue</button></div><div class="onboarding-note">${shield}<span>Bot tokens stay in Phoenix’s private files. You can add, pause or remove chat apps later in Settings → Chat apps.</span></div>`, (root) => {
      const host = root.querySelector("#onboardChannels");
      if (window.PhoenixChannels) window.PhoenixChannels.render(host);
      else host.innerHTML = '<p class="settings-section-note">Chat apps are available in Settings → Chat apps.</p>';
      const next = () => { state.channelsReviewed = true; go(8, 1); };
      root.querySelector("#skipChannels").onclick = next;
      root.querySelector("#continueChannels").onclick = next;
    });
  }

  function renderReady() {
    const snap = state.snapshot;
    const p = ui.state.view?.directory.agents.find((a) => a.agent_id === "phoenix") || snap.agents[0];
    const missing = snap.required_actions || [];
    const cookies = snap.state.cookie_import?.choice === "all_portable" ? `${snap.state.cookie_import.agent_ids?.length || 0} profiles` : "not imported";
    const access = defaultValue("composer.default_permission", "workspace").replaceAll("_", " ");
    const workers = defaultValue("agents.volume_workers_enabled", true) ? `up to ${defaultValue("agents.volume_worker_limit", 8)}` : "disabled";
    shell(`<div class="onboarding-ready"><span class="onboarding-ready-mark">${ui.avatarSvg(p)}</span><span class="onboarding-step-label">Ready</span><h2>${missing.length ? "A few things are still open." : "Your company is ready."}</h2><p class="onboarding-lead">${missing.length ? "Go back or skip remaining steps. Phoenix will not finish until required setup is stored." : "Phoenix can coordinate the whole company, and every coworker keeps a continuous canonical thread."}</p><div class="onboarding-summary"><span><strong>${snap.agents.length} coworker${snap.agents.length === 1 ? "" : "s"}</strong><small>${snap.state.company_choice === "phoenix_only" ? "clean start" : "founding company"}</small></span><span><strong>${snap.provider_ready ? "Provider ready" : "Provider pending"}</strong><small>${escape(snap.state.provider_verification?.model || snap.configured_model || "not verified")}</small></span><span><strong>${["uninitialized","unprotected"].includes(snap.vault_status) ? "Passes pending" : "Passes protected"}</strong><small>${escape(snap.state.default_account_email || (snap.state.account_email_skipped ? "email skipped" : "not set"))}</small></span><span><strong>${escape(access)} access</strong><small>company default</small></span><span><strong>${escape(workers)} workers</strong><small>independent volume tasks</small></span><span><strong>${escape(cookies)}</strong><small>browser cookies</small></span></div><div class="onboarding-actions"><button id="finishOnboarding" class="button primary" ${missing.length ? "disabled" : ""}>Enter Phoenix</button></div></div>`, (root) => {
      root.querySelector("#finishOnboarding").onclick = () => run({ action: "complete" }, true);
    });
  }

  async function inspectCookies(source) {
    if (!source) { state.error = "Choose a browser first."; state.direction = 0; renderCookies(); return; }
    setBusy(true);
    try {
      const reply = await rpc({ action: "inspect_cookie_source", source });
      state.inspection = reply.cookie_source || reply;
      state.error = "";
    } catch (error) {
      state.error = error.message;
    }
    setBusy(false);
    state.direction = 0;
    renderCookies();
  }

  function setBusy(value) { state.busy = value; }

  function runtimeFailure(error) {
    const message = error?.message || String(error || "The Phoenix runtime is not ready.");
    state.runtimeError = message;
    const stage = ensureShell();
    stage.innerHTML = `<section class="onboarding-card in"><span class="onboarding-step-label">Runtime</span><h2>Phoenix could not start its company runtime.</h2><p class="onboarding-lead">Onboarding is still here. Fixing the runtime will not clear or replace anything in <code>.phoenix</code>.</p><div class="onboarding-error runtime-error"><strong>What happened</strong><span>${escape(message)}</span></div><div class="onboarding-actions"><button type="button" id="retryRuntime" class="button primary">Retry runtime</button></div><div class="onboarding-note">${shield}<span>Phoenix requires the desktop and gateway from the same build. The retry checks that pair before loading your existing company.</span></div></section>`;
    setOnboardingOpen(true);
    $("retryRuntime").onclick = async () => {
      if (state.busy) return;
      state.busy = true;
      $("retryRuntime").disabled = true;
      $("retryRuntime").textContent = "Starting…";
      try {
        await ui.invoke("gateway_ensure");
        const [token, status] = await Promise.all([ui.invoke("gateway_token"), ui.invoke("gateway_status")]);
        ui.state.gateway.token = token;
        ui.state.gateway.port = status.ws_port || 17373;
        state.runtimeError = "";
        await load();
      } catch (retryError) {
        state.busy = false;
        runtimeFailure(retryError);
      }
    };
  }

  async function run(command, finishing = false) {
    if (state.busy) return;
    setBusy(true);
    state.error = "";
    try {
      const reply = await rpc(command);
      state.snapshot = unwrap(reply);
      if (finishing && state.snapshot.complete) {
        setOnboardingOpen(false);
        ui.toast("Welcome to Phoenix.");
        setBusy(false);
        return;
      }
      state.direction = 1;
      state.screen = replay ? Math.min(8, currentScreen() + 1) : firstIncomplete();
    } catch (error) {
      state.error = error.message;
      state.direction = 0;
    }
    setBusy(false);
    render();
  }

  async function load(runtimeError="") {
    if (runtimeError) {
      runtimeFailure(runtimeError);
      return;
    }
    try {
      const reply = await rpc({ action: "status" });
      const snap = unwrap(reply);
      state.snapshot = snap;
      if (snap.complete && !preview && !replay) {
        setOnboardingOpen(false);
        return;
      }
      if (detectedCoworkers() && !state.selectedCompany) state.selectedCompany = "founding_company";
      try { await loadSettingsSnapshot(); } catch {}
      state.screen = replay ? 0 : firstIncomplete();
      if (preview && params.get("onboardingStep") === "powers") {
        const now = new Date().toISOString();
        Object.assign(snap.state, { company_choice: "founding_company", default_account_email: "owner@example.com", cookie_import: { choice: "skipped", decided_at: now }, provider_verification: { provider: snap.configured_provider, model: snap.configured_model, verified_at: now }, company_defaults_reviewed_at: now });
        snap.provider_ready = true;
        snap.vault_status = "unlocked";
        snap.agents = mockAgents(true);
        snap.required_actions = ["review_powers_setup"];
        state.screen = 6;
      } else if (preview && /^[0-8]$/.test(params.get("onboardingStep") || "")) {
        state.screen = Number(params.get("onboardingStep"));
      }
      state.direction = 0;
      setOnboardingOpen(true);
      render();
    } catch (error) {
      if (preview) {
        state.snapshot = initialMock();
        const requestedStep = params.get("onboardingStep");
        if (requestedStep === "powers") state.screen = 6;
        else if (/^[0-8]$/.test(requestedStep || "")) state.screen = Number(requestedStep);
        setOnboardingOpen(true);
        render();
      } else runtimeFailure(error);
    }
  }

  // No account setup or fabricated completion in the connected no-credential review.
  if (!shellFixture && !window.__PHOENIX_ISOLATED_BACKEND__?.enabled) {
    addEventListener("phoenix:directory-ready", (event) => { if (preview || ui.TAURI) load(event.detail?.runtimeError || ""); });
    if (ui.state.view && (preview || ui.TAURI)) load();
  }
})();
