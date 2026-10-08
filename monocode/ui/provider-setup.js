"use strict";

(() => {
  const ui = window.PhoenixUI;
  if (!ui) return;
  const preview = !ui.TAURI || ui.SIDEBAR_PREVIEW;
  const currentBackend = window.__PHOENIX_CURRENT_BACKEND__?.enabled === true;
  const isolated = window.__PHOENIX_ISOLATED_BACKEND__?.enabled === true;
  let authPlan = null;
  const esc = ui.escapeHtml;
  const state = { session: 0, label: "", savedProfileId: "", savedMethod: "", catalog: null, initialize: false, onConnected: null, provider: "", model: "", method: "", replaceProfileId: "", busy: false, deviceAuth: null, listening: false };

  const closeIcon = '<svg viewBox="0 0 20 20"><path d="m5 5 10 10M15 5 5 15"/></svg>';
  const mockCatalog = {
    providers: [
      { id: "openai", name: "OpenAI API", recommended: "gpt-5.6", auth_methods: [{type:"api",label:"API key",env_var:"OPENAI_API_KEY",llm_supported:true}], models: [{id:"gpt-5.6",name:"GPT-5.6",context_window:1000000}], profiles: [] },
      { id: "openai-codex", name: "OpenAI Codex", recommended: "gpt-5.6-sol", auth_methods: [{type:"oauth",label:"Sign in with ChatGPT",llm_supported:true}], models: [{id:"gpt-5.6-sol",name:"GPT-5.6 Sol",context_window:400000}], profiles: [] },
      { id: "github-copilot", name: "GitHub Copilot", recommended: "gpt-5.6", auth_methods: [{type:"device_code",label:"Sign in with GitHub",llm_supported:true}], models: [{id:"gpt-5.6",name:"GPT-5.6",context_window:400000}], profiles: [] },
      { id: "google-gemini-cli", name: "Gemini CLI OAuth", recommended: "google/gemini-3.1-pro-preview", auth_methods: [{type:"oauth",label:"Google OAuth",llm_supported:true}], models: [{id:"google/gemini-3.1-pro-preview",name:"Gemini 3.1 Pro",context_window:1048576}], profiles: [] },
      { id: "anthropic", name: "Anthropic", recommended: "claude-opus-4.6", auth_methods: [{type:"api",label:"API key",llm_supported:true}], models: [{id:"claude-opus-4.6",name:"Claude Opus 4.6",context_window:1000000}], profiles: [] },
      { id: "ollama", name: "Ollama · local", recommended: "qwen3.5", auth_methods: [{type:"none",label:"No login",llm_supported:true}], models: [{id:"qwen3.5",name:"Qwen 3.5",context_window:262144}], profiles: [] },
    ],
    oauth_login_providers: ["openai-codex", "xai", "grok-cli", "google-gemini-cli", "chutes", "github-copilot", "minimax-portal"],
  };

  async function invoke(command, args = {}) {
    if (preview) {
      if (command === "providers_catalog") return mockCatalog;
      if (command === "auth_probe") return { ok: true, model: args.model, note: "Preview credential verified." };
      if (command === "oauth_login" && args.authMethod?.startsWith("device_code")) {
        setTimeout(() => handleDeviceAuth({ provider: args.provider, verification_url: "https://github.com/login/device", user_code: "PHNX-2026" }), 180);
        await new Promise((resolve) => setTimeout(resolve, 900));
      }
      return "saved in preview";
    }
    return ui.invoke(command, args);
  }

  function provider() {
    return state.catalog?.providers?.find((entry) => entry.id === state.provider) || state.catalog?.providers?.[0];
  }

  function methods(entry) {
    const oauth = new Set(state.catalog?.oauth_login_providers || []);
    const result = [];
    if (state.initialize && entry?.profiles?.length && !state.replaceProfileId) result.push({ type: "existing", label: "Existing account" });
    for (const method of entry?.auth_methods || []) {
      if (method.llm_supported === false) continue;
      if (method.type === "oauth" && !oauth.has(entry.id)) continue;
      if (["api", "oauth", "device_code", "device_code_cn", "none"].includes(method.type) && !result.some((item) => item.type === method.type)) {
        result.push({ type: method.type, label: method.label || method.type });
      }
    }
    return result;
  }

  function providerChoiceHint(entry) {
    const choices = methods(entry).filter((method) => method.type !== "existing");
    if (!choices.length) return "No sign-in method available";
    return choices.map((method) => method.label).join(" or ");
  }

  function nextProfileId(entry) {
    const ids = new Set((entry.profiles || []).map((profile) => profile.id));
    const base = `${entry.id}:default`;
    if (!ids.has(base)) return base;
    for (let slot = 2; slot < 1000; slot += 1) {
      if (!ids.has(`${entry.id}:${slot}`)) return `${entry.id}:${slot}`;
    }
    return `${entry.id}:${Date.now()}`;
  }

  function isDeviceMethod() {
    return state.method === "device_code" || state.method === "device_code_cn";
  }

  function handleDeviceAuth(payload) {
    if (!payload || payload.provider !== state.provider || !isDeviceMethod()) return;
    state.deviceAuth = payload;
    const card = document.getElementById("providerDeviceAuth");
    if (!card) return;
    card.hidden = false;
    card.innerHTML = `<span>Enter this one-time code in the browser Phoenix opened</span><strong>${esc(payload.user_code)}</strong><small>${esc(payload.verification_url)}</small><button type="button" class="button secondary" data-copy-device-code>Copy code</button>`;
    card.querySelector("[data-copy-device-code]").onclick = async () => {
      try {
        await navigator.clipboard.writeText(payload.user_code);
        ui.toast("Verification code copied.");
      } catch {
        ui.toast("Could not copy automatically. Select the code above.", true);
      }
    };
    const status = document.getElementById("providerSetupStatus");
    if (status) status.querySelector("span").textContent = "Waiting for you to approve the sign-in in your browser…";
  }

  async function ensureDeviceAuthListener() {
    if (state.listening || preview || !window.__TAURI__?.event?.listen) return;
    state.listening = true;
    try {
      await window.__TAURI__.event.listen("provider-device-auth", (event) => handleDeviceAuth(event.payload));
    } catch (error) {
      state.listening = false;
      console.warn("Phoenix could not subscribe to provider device auth events", error);
    }
  }

  function render() {
    const entry = provider();
    if (!entry) return;
    state.provider = entry.id;
    const available = methods(entry);
    if (!available.some((item) => item.type === state.method)) state.method = available[0]?.type || "";
    const models = entry.models || [];
    if (state.initialize && !models.some((model) => model.id === state.model)) state.model = entry.recommended || models[0]?.id || "";
    const modelRow = models.find((model) => model.id === state.model) || models[0];
    const modelLabel = modelRow ? `${modelRow.name}${modelRow.context_window ? ` · ${Math.round(modelRow.context_window / 1000)}K` : ""}` : "Choose a model";
    ui.showModal(`<section class="modal provider-setup-modal" role="dialog" aria-modal="true" aria-labelledby="providerSetupTitle">
      <header class="modal-header"><span><strong id="providerSetupTitle">${isolated ? "Review subscription sign-in" : state.initialize ? "Connect Phoenix" : state.replaceProfileId ? "Sign in again" : "Add a provider account"}</strong><small>${isolated ? "OpenAI Codex through your ChatGPT subscription. Authorization is paused; no API credits." : state.initialize ? "Choose the intelligence behind your company. This creates the first config without a terminal or replacing existing data." : state.replaceProfileId ? `Reconnect ${esc(state.label || entry.name)}.` : "Sign in or add an API key."}</small></span><button class="modal-close" aria-label="Close">${closeIcon}</button></header>
      <form id="providerSetupForm" class="modal-body">

        <div class="field provider-setup-field"><span>Provider</span><button type="button" class="menu-select" id="providerSetupProvider" aria-label="Provider" ${state.replaceProfileId?"disabled":""}><span>${esc(entry.name)}</span><svg viewBox="0 0 20 20"><path d="m6 8 4 4 4-4"/></svg></button></div>
        ${state.initialize?`<div class="field provider-setup-field"><span>${isolated ? 'Subscription model' : 'Initial company model'}</span><button type="button" class="menu-select" id="providerSetupModel" aria-label="${isolated ? 'Subscription model' : 'Initial company model'}"><span>${esc(modelLabel)}</span><svg viewBox="0 0 20 20"><path d="m6 8 4 4 4-4"/></svg></button></div>`:""}
        <div class="provider-methods">${available.map((method) => `<button type="button" data-provider-method="${esc(method.type)}" class="${method.type === state.method ? "active" : ""}">${esc(method.label)}</button>`).join("")}</div>
        ${!isolated && !["existing", "none"].includes(state.method) ? `<label class="field"><span>Account name</span><input id="providerSetupName" aria-label="Account name" aria-describedby="providerNameHelp" value="${esc(state.label)}" maxlength="80" autocomplete="off" placeholder="e.g. Personal or Work" required><small id="providerNameHelp">A name you’ll recognize when choosing an account.</small></label>` : ""}
        ${authFields(entry)}
        <div id="providerSetupStatus" class="provider-setup-status${isolated ? ' review' : ''}" aria-live="polite"><i></i><span>${statusCopy(entry)}</span></div>
      </form>
      <footer class="modal-footer"><button class="button secondary" type="button" data-provider-cancel>Cancel</button><button id="providerSetupSubmit" class="button primary" type="submit" form="providerSetupForm" ${!state.method ? "disabled" : ""}>${isolated ? "Check readiness" : state.method === "oauth" || isDeviceMethod() ? "Open sign-in" : state.method === "existing" ? "Use account" : "Save account"}</button></footer>
    </section>`);
    const layer = document.getElementById("modalLayer");
    layer.querySelector("[data-provider-cancel]").onclick = ui.closeModal;
    layer.querySelector("#providerSetupProvider").onclick = state.replaceProfileId ? null : (event) => {
      event.preventDefault();
      event.stopPropagation();
      ui.openMenuSelect(event.currentTarget, state.catalog.providers.map((item) => [item.id, item.name, "", "", providerChoiceHint(item)]), state.provider, (value) => {
        state.provider = value; state.label = ""; state.model = ""; state.method = ""; state.deviceAuth = null; render();
      },{search:true,searchPlaceholder:"Search providers",searchLabel:"Search model providers",providerIcons:true,className:"provider-picker-popover",minWidth:360});
    };
    const modelButton=layer.querySelector("#providerSetupModel");if(modelButton)modelButton.onclick = (event) => {
      ui.openMenuSelect(event.currentTarget, models.map((model) => [model.id, `${model.name}${model.context_window ? ` · ${Math.round(model.context_window / 1000)}K` : ""}`]), state.model, (value) => {
        state.model = value; render();
      },{search:true,searchPlaceholder:`Search ${entry.name} models`,minWidth:380});
    };
    const account = layer.querySelector("#providerSetupProfile");
    if (account?.tagName === "BUTTON") {
      account.onclick = (event) => {
      ui.openMenuSelect(event.currentTarget, entry.profiles.map((profile) => [profile.id, `${profile.display || profile.id} · ${profile.method || "stored"}`]), account.dataset.value, (value) => {
          account.dataset.value = value;
          account.querySelector("span").textContent = entry.profiles.find((profile) => profile.id === value)?.display || value;
        },{search:entry.profiles.length>5,searchPlaceholder:"Search accounts",minWidth:340});
      };
    }
    layer.querySelectorAll("[data-provider-method]").forEach((button) => button.onclick = () => { state.method = button.dataset.providerMethod; state.deviceAuth = null; render(); });
    const nameInput = layer.querySelector("#providerSetupName");
    if (nameInput) nameInput.oninput = () => { state.label = nameInput.value; };
    layer.querySelector("#providerSetupForm").onsubmit = connect;
    focusDialog(layer);
  }

  function authFields(entry) {
    if (currentBackend) return `<div class="provider-oauth-note" data-auth-review><p><strong>GPT-6.1 Sol / xhigh</strong><br>Current Phoenix reports ${Number(authPlan?.existingAccounts)||0} existing Codex account(s). This review reads account metadata without copying credentials.</p><p>New sign-in, persistent authentication, storage choices and a model test are awaiting your decision. Check readiness does not start OAuth or device authorization.</p><p>Existing Phoenix remains the backend. Its accounts and data stay unchanged.</p></div>`;
    if (isolated) return `<div class="provider-oauth-note" data-auth-review><p><strong>GPT-6.1 Sol / xhigh</strong><br>No account is connected. Checking readiness does not start sign-in.</p><p>Requested access: ${state.method==='device_code'?'Codex device authorization. The provider will show its access request after approval.':'OpenID identity, profile, email and offline refresh access for Codex.'}</p><p>After confirmation: secure ${state.method==='device_code'?'device-code':'OAuth PKCE'} sign-in, then one bounded subscription test. Production accounts stay untouched.</p><p>Storage: owner-only JSON in the isolated runtime (0600), <strong>not vault-encrypted</strong>. No tokens are stored now.</p></div>`;
    if (state.method === "existing") {
      const first = entry.profiles[0];
      return `<div class="field provider-setup-field"><span>Account</span><button type="button" class="menu-select" id="providerSetupProfile" aria-label="Account" data-value="${esc(first?.id || "")}"><span>${esc(`${first?.display || first?.id || "Account"} · ${first?.method || "stored"}`)}</span><svg viewBox="0 0 20 20"><path d="m6 8 4 4 4-4"/></svg></button></div>`;
    }
    if (state.method === "api") {
      const env = entry.auth_methods?.find((method) => method.type === "api")?.env_var || entry.env_vars?.[0] || "API key";
      return `<label class="field"><span>${esc(env)}</span><input id="providerSetupSecret" type="password" autocomplete="off" spellcheck="false" required placeholder="Paste key"></label>`;
    }
    if (state.method === "oauth") {
      const customClient = entry.id === "google-gemini-cli" || entry.id === "chutes";
      return `${customClient ? `<label class="field"><span>OAuth client ID</span><input id="providerSetupClientId" autocomplete="off" spellcheck="false" required placeholder="Paste the provider app client ID"></label><label class="field"><span>OAuth client secret <small>optional for public clients</small></span><input id="providerSetupClientSecret" type="password" autocomplete="off" spellcheck="false" placeholder="Paste only if your OAuth app requires it"></label>` : ""}<div class="provider-oauth-note">Continue in your browser to sign in.</div>`;
    }
    if (isDeviceMethod()) return `<div class="provider-oauth-note">Phoenix opens the provider’s secure device page. A one-time verification code will appear here; the resulting credential never enters this webview.</div><div id="providerDeviceAuth" class="provider-device-auth" ${state.deviceAuth ? "" : "hidden"}>${state.deviceAuth ? `<span>Enter this one-time code in the browser Phoenix opened</span><strong>${esc(state.deviceAuth.user_code)}</strong><small>${esc(state.deviceAuth.verification_url)}</small><button type="button" class="button secondary" data-copy-device-code>Copy code</button>` : ""}</div>`;
    if (state.method === "none") return `<div class="provider-oauth-note">No credential is stored. Start the local provider service before using its models.</div>`;
    return `<div class="provider-oauth-note bad">This provider has no native model-login method Phoenix can safely use.</div>`;
  }

  function statusCopy(entry) {
    if (isolated) return 'Ready for review. Authorization, token storage and execution are paused.';
    if (state.method === "oauth") return `Sign in to ${esc(entry.name)}; this window will finish automatically.`;
    if (isDeviceMethod()) return `Open ${esc(entry.name)} sign-in; Phoenix will show the verification code here.`;
    if (state.method === "existing") return state.initialize ? "Use this account for Phoenix." : "This account is already connected.";
    if (state.method === "none") return "No sign-in is needed for this provider.";
    return "Your key is stored privately on this device.";
  }

  function setBusy(message, failed = false) {
    state.busy = !failed;
    const status = document.getElementById("providerSetupStatus");
    const submit = document.getElementById("providerSetupSubmit");
    document.getElementById("providerSetupForm")?.querySelectorAll("input, button").forEach(control => {
      if (!failed) { control.dataset.wasDisabled = String(control.disabled); control.disabled = true; }
      else if (control.dataset.wasDisabled !== undefined) { control.disabled = control.dataset.wasDisabled === "true"; delete control.dataset.wasDisabled; }
    });
    if (status) { status.classList.toggle("busy", !failed); status.classList.toggle("failed", failed); status.querySelector("span").textContent = message; }
    if (submit) submit.disabled = !failed;
  }

  async function connect(event) {
    event.preventDefault();
    if (state.busy) return;
    const entry = provider();
    if (isolated) {
      const generation=state.session,form=document.getElementById('providerSetupForm'),current=()=>generation===state.session&&form?.isConnected;
      setBusy(currentBackend?'Checking the existing Phoenix subscription route...':'Checking the isolated subscription route...');
      try {
        const plan=await invoke('auth_setup',{action:'prepare',provider:'openai-codex',model:'gpt-6.1-sol',effort:'xhigh',method:state.method});
        if(!current()){state.busy=false;return;}authPlan=plan;
        setBusy('Authorization required. No sign-in, credential write or model request was started.',true);
        document.getElementById('providerSetupStatus')?.classList.remove('failed');
        document.getElementById('providerSetupSubmit').textContent='Check readiness again';
      } catch(error) { if(current()){setBusy('Readiness check failed. Keep the existing Phoenix backend open and retry.',true);showErrorDetails(error);}else state.busy=false; }
      return;
    }
    const session = state.session, form = document.getElementById("providerSetupForm"), callback = state.onConnected;
    const current = () => state.session === session && (!form || form.isConnected);
    const method = state.method, initialize = state.initialize;
    const label = (document.getElementById("providerSetupName")?.value || state.label || "").trim();
    if (!["existing", "none"].includes(method) && !validAccountName(label)) {
      setBusy("Enter an account name using 1–80 characters.", true); return;
    }
    const model = state.initialize ? (state.model || entry.recommended) : null;
    const profileEl = document.getElementById("providerSetupProfile");
    let profileId = method === "existing" ? (profileEl?.dataset.value || "").trim() : (state.savedProfileId || state.replaceProfileId || nextProfileId(entry));
    const alreadySaved = !!state.savedProfileId;
    let storedMethod = state.savedMethod || method;
    setBusy(state.method === "oauth" || isDeviceMethod() ? "Waiting for secure provider sign-in…" : "Saving the account…");
    try {
      if (method === "api" && !alreadySaved) {
        const input = document.getElementById("providerSetupSecret");
        const key = input.value;
        input.value = "";
        await invoke("auth_set_key", { profileId, provider: entry.id, key });
        storedMethod = "api";
      } else if (["oauth", "device_code", "device_code_cn"].includes(method) && !alreadySaved) {
        const clientIdInput = document.getElementById("providerSetupClientId");
        const clientSecretInput = document.getElementById("providerSetupClientSecret");
        const clientId = clientIdInput?.value?.trim() || null;
        const clientSecret = clientSecretInput?.value || null;
        if (clientSecretInput) clientSecretInput.value = "";
        await invoke("oauth_login", { provider: entry.id, profileId, authMethod: method, clientId, clientSecret });
        storedMethod = method.startsWith("device_code") ? "token" : "oauth";
      } else if (method === "existing") {
        const existing = entry.profiles.find((item) => item.id === profileId);
        storedMethod = existing?.method === "token" ? "token" : existing?.method === "oauth" ? "oauth" : "api";
      } else if (method === "none") {
        profileId = null;
        storedMethod = "none";
      }
      if (!["existing", "none"].includes(method)) {
        if (current()) { state.savedProfileId = profileId; state.savedMethod = storedMethod; }
        await invoke("auth_rename", { profileId, label });
      }
      if (initialize) {
        await invoke("provider_initialize_config", { provider: entry.id, model, authMethod: storedMethod, profileId });
      }
      if (current()) ui.closeModal();
      ui.toast(`${entry.name} is connected.`);
      await callback?.({ provider: entry.id, model, profileId, authMethod: storedMethod });
    } catch (error) {
      if (current()) {
        setBusy(state.savedProfileId ? "Account connected. Could not finish saving; retry to continue." : "Could not connect this account. Please try again.", true);
        if (state.savedProfileId) {
          form?.querySelectorAll("#providerSetupSecret, #providerSetupClientId, #providerSetupClientSecret").forEach(input => input.required = false);
          form?.querySelectorAll("button").forEach(button => button.disabled = true);
          document.getElementById("providerSetupSubmit").textContent = "Finish saving";
        }
        showErrorDetails(error);
      }
    }
  }

  async function open(options = {}) {
    seedPreviewCatalog(options.catalog);
    const session = ++state.session;
    state.label = "";
    state.savedProfileId = "";
    state.savedMethod = "";
    state.initialize = !!options.initialize;
    state.onConnected = options.onConnected || null;
    state.provider = options.provider || "";
    state.replaceProfileId = options.profileId || "";
    state.model = "";
    state.method = "";
    state.busy = false;
    state.deviceAuth = null;
    try {
      await ensureDeviceAuthListener();
      const catalog = await invoke("providers_catalog");
      if (state.session !== session) return;
      state.catalog = isolated ? {...catalog, providers:catalog.providers.filter(p=>p.id==='openai-codex').map(p=>({...p,recommended:'gpt-6.1-sol',models:p.models.filter(m=>m.id==='gpt-6.1-sol')}))} : catalog;
      if (isolated) { state.provider='openai-codex'; state.initialize=true; state.model='gpt-6.1-sol'; authPlan=await invoke('auth_setup',{action:'status',provider:'openai-codex',model:'gpt-6.1-sol',effort:'xhigh'}); if(state.session!==session)return; }
      if (!state.provider) state.provider = state.catalog.providers?.find((entry) => entry.profiles?.length)?.id || state.catalog.providers?.[0]?.id || "";
      if(state.replaceProfileId){const entry=provider(),account=entry?.profiles?.find((profile)=>profile.id===state.replaceProfileId),available=methods(entry),stored=String(account?.method||"");let reauth=null;if(stored==="api")reauth=available.find((method)=>method.type==="api");else if(stored==="none")reauth=available.find((method)=>method.type==="none");else reauth=available.find((method)=>!["existing","api","none"].includes(method.type));state.method=reauth?.type||available.find((method)=>method.type!=="existing")?.type||"";}
      const account = provider()?.profiles?.find(profile => profile.id === state.replaceProfileId);
      state.label = account?.display || account?.display_name || "";
      render();
    } catch (error) {
      if (state.session !== session) return;
      ui.toast(`Provider catalog unavailable: ${error?.message || error}`, true);
    }
  }


  function focusDialog(layer) {
    const dialog = layer.querySelector('[role="dialog"]');
    (dialog.querySelector('input:not([disabled])') || dialog.querySelector('button:not([disabled])'))?.focus();
    dialog.addEventListener('keydown', event => {
      if (event.key !== 'Tab') return;
      const controls = [...dialog.querySelectorAll('button:not([disabled]), input:not([disabled]), summary, [tabindex="0"]')].filter(node => node.getClientRects().length);
      const first = controls[0], last = controls.at(-1);
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    });
  }

  function connectionFailureMessage(error) {
    const text = String(error?.message || error || "");
    if (/AUTHORIZATION_REQUIRED/.test(text)) return "Connection test paused pending your authorization. The existing account is unchanged.";
    if (/429|usage.limit|quota|rate.limit/i.test(text)) return "Usage limit reached. Your sign-in is still saved.";
    if (/401|invalid.grant|token.expired|refresh.token|unauthorized|oauth token refresh returned http \d+:.*sign in again|saved login rejected/i.test(text)) return "Sign-in needs attention. Try signing in again.";
    if (/timeout|timed out|network|unavailable|502|503|504/i.test(text)) return "Provider unavailable. Try again shortly.";
    return "Account saved, but it cannot answer right now.";
  }

  function validAccountName(label) {
    return !!label && [...label].length <= 80 && !/[\x00-\x1f\x7f-\x9f]/.test(label);
  }

  function showErrorDetails(error, root = document.getElementById("providerSetupForm")) {
    if (!root) return;
    root.querySelector(".provider-error-details")?.remove();
    const details = document.createElement("details");
    details.className = "provider-error-details";
    const summary = document.createElement("summary");
    summary.textContent = "? Error details";
    const body = document.createElement("pre");
    body.textContent = String(error?.message || error);
    details.append(summary, body);
    const status = root.querySelector('.provider-manage-status, .provider-setup-status');
    if (status) status.after(details); else root.append(details);
  }

  function seedPreviewCatalog(catalog) {
    if (!preview || !catalog) return;
    for (const entry of mockCatalog.providers) {
      const match = catalog.providers?.find(p => p.id === entry.id);
      if (match) entry.profiles = structuredClone(match.profiles || []);
    }
  }

  async function manage(options) {
    seedPreviewCatalog(options.catalog);
    const session = ++state.session;
    const catalog = await invoke("providers_catalog");
    if (session !== state.session) return;
    const entry = catalog.providers?.find(p => p.id === options.provider);
    const account = entry?.profiles?.find(p => p.id === options.profileId);
    if (!account) { ui.toast("Account no longer exists. Refresh your providers.", true); return; }
    const label = account.display || account.display_name || account.id;
    ui.showModal(`<section class="modal provider-setup-modal" role="dialog" aria-modal="true" aria-labelledby="providerManageTitle">
      <header class="modal-header"><span><strong id="providerManageTitle">Manage account</strong><small>${esc(entry.name)}</small></span><button class="modal-close" aria-label="Close">${closeIcon}</button></header>
      <form id="providerManageForm" class="modal-body">
        <label class="field"><span>Account name</span><input name="label" aria-label="Account name" value="${esc(label)}" required maxlength="80" autocomplete="off" aria-describedby="providerManageNameError"><small id="providerManageNameError" role="alert" hidden></small></label>
        <div class="provider-manage-actions"><button class="button secondary" type="button" data-reconnect>${account.method === "api" ? "Replace API key" : "Sign in again"}</button><button class="button secondary" type="button" data-verify>Test connection</button></div>
        <div class="provider-manage-status" role="status">Account saved. Test connection checks whether it can answer now.</div>
        <details class="provider-remove"><summary>Remove account</summary><p>This removes the saved login from Phoenix. Any route using this account will need another available account.</p><button class="button danger" type="button" data-remove>Remove ${esc(label)}</button></details>
      </form>
      <footer class="modal-footer"><button class="button secondary" type="button" data-done>Done</button><button class="button primary" type="submit" form="providerManageForm">Save name</button></footer>
    </section>`);
    const form = document.getElementById("providerManageForm"), layer = document.getElementById("modalLayer");
    const status = form.querySelector("[role=status]");
    const current = () => state.session === session && form.isConnected;
    focusDialog(layer);
    let busy = false;
    const run = async (message, action, failureMessage = () => "Could not complete that action. Your account is still saved.") => {
      if (busy) return;
      busy = true; status.textContent = message;
      const controls = [...layer.querySelectorAll("input, button")].filter(c => !c.matches(".modal-close, [data-done]"));
      controls.forEach(c => c.disabled = true);
      form.querySelector(".provider-error-details")?.remove();
      try { await action(); } catch (error) {
        if (current()) { status.textContent = failureMessage(error); showErrorDetails(error, form); }
      } finally { busy = false; if (current()) controls.forEach(c => c.disabled = false); }
    };
    layer.querySelector("[data-done]").onclick = ui.closeModal;
    form.querySelector("[data-reconnect]").onclick = () => open(options);
    const nameInput = form.elements.label, nameError = form.querySelector('#providerManageNameError');
    nameInput.oninput = () => { nameInput.removeAttribute('aria-invalid'); nameError.hidden = true; nameError.textContent = ''; };
    form.onsubmit = event => { event.preventDefault(); const label = nameInput.value.trim();
      if (!validAccountName(label)) {
        nameInput.setAttribute('aria-invalid', 'true');
        nameError.textContent = 'Enter an account name using 1–80 characters.';
        nameError.hidden = false; nameInput.focus(); return;
      }
      return run("Saving name…", async () => {
      await invoke("auth_rename", { profileId: account.id, label });
      if (current()) { status.textContent = "Name saved."; form.querySelector("[data-remove]").textContent = `Remove ${label}`; }
      await options.onConnected?.();
    }); };
    form.querySelector("[data-verify]").onclick = () => run("Checking connection…", async () => {
      const result = await invoke("auth_probe", { profileId: account.id, model: null, effort: "medium" });
      if (!current()) return;
      if (result?.skipped === true) status.textContent = "Connection check is not available for this account. Your sign-in is still saved.";
      else if (result?.ok === true) status.textContent = "Connection verified. This account can answer.";
      else if (result?.ok === false) { status.textContent = connectionFailureMessage(result.error || result.note); showErrorDetails(result.error || result.note || "Provider rejected the request.", form); }
      else { status.textContent = "Could not verify this connection. Try again."; showErrorDetails("The connection check returned an unrecognized response.", form); }
    }, connectionFailureMessage);
    form.querySelector("[data-remove]").onclick = () => run("Removing account…", async () => {
      await invoke("auth_remove", { profileId: account.id });
      if (current()) ui.closeModal();
      dispatchEvent(new CustomEvent("phoenix:provider-accounts-changed",{detail:{removedProfileId:account.id}}));
      await options.onConnected?.();ui.toast(`${label} removed.`);
    });
  }

  window.PhoenixProviderSetup = Object.freeze({ open, manage });
})();
