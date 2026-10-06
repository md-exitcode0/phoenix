"use strict";

(() => {
  const ui = window.PhoenixUI;
  if (!ui) return;
  const $ = (id) => document.getElementById(id);
  const escape = (value) => String(value ?? "").replace(/[&<>"']/g,(char)=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[char]));
  const preview = !ui.TAURI || ui.SIDEBAR_PREVIEW;
  const terminalRuntime = window.PhoenixTerminal;
  const state = { tabs: [], activeKey: "", conversationKey: selectedConversationKey(), terminalViews: new Map(), nextKey: 1, listening: false, resizeTimer: 0, resizeFrame: 0, resizePromise: null, closeTimer: 0, orphanOutput: new Map() };

  function selectedConversationKey(item=ui.state.selected){return item?.kind&&item?.id?`${item.kind}:${item.id}`:"agent:phoenix";}
  function terminalView(key=state.conversationKey){if(!state.terminalViews.has(key))state.terminalViews.set(key,{open:false,activeKey:""});return state.terminalViews.get(key);}
  function terminalTabs(key=state.conversationKey){return state.tabs.filter((tab)=>tab.conversationKey===key);}
  function publishTerminalTabs(){window.dispatchEvent(new CustomEvent("phoenix:terminal-tabs",{detail:{tabs:state.tabs.map((tab)=>({key:tab.key,conversationKey:tab.conversationKey,cwd:tab.cwd,id:tab.id,exited:Boolean(tab.exited)}))}}));}
  function rememberTerminalView(){const view=terminalView();view.open=termOpen();view.activeKey=activeTab()?.key||"";}

  function workspace() {
    return window.PhoenixConversation?.workspace?.() || localStorage.getItem("phoenix-workspace:last") || "";
  }
  function workspaceName(path) {
    const parts = String(path || "").replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean);
    return parts[parts.length - 1] || path || "";
  }
  function railHidden() { return document.body.classList.contains("rail-hidden"); }
  function termOpen() { return document.body.classList.contains("term-open"); }
  function browserOpen() { return Boolean($("browserOverlay") && !$("browserOverlay").hidden); }
  function sidebarCollapsed() { return document.body.classList.contains("sidebar-collapsed"); }
  function settingsOpen() { return Boolean($("settingsView") && !$("settingsView").hidden); }

  function measureCols() {
    const tab = activeTab();
    if (tab?.terminal && tab.fit) {
      try {
        const rect = $("termScreen")?.getBoundingClientRect();
        if (rect?.width > 40 && rect?.height > 40) tab.fit.fit();
        return { cols: Math.max(20, tab.terminal.cols), rows: Math.max(8, tab.terminal.rows) };
      } catch {}
    }
    const screen = $("termScreen");
    if (!screen) return { cols: 80, rows: 16 };
    const probe = document.createElement("span");
    probe.textContent = "0";
    probe.style.cssText = "position:absolute;visibility:hidden;font:12.5px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace";
    screen.appendChild(probe);
    const w = probe.getBoundingClientRect().width || 7.4;
    const h = probe.getBoundingClientRect().height || 18;
    probe.remove();
    const rect = screen.getBoundingClientRect();
    return { cols: Math.max(20, Math.floor(rect.width / w) - 1), rows: Math.max(8, Math.floor(rect.height / h) - 1) };
  }

  function activeTab() { return state.tabs.find((tab) => tab.key === state.activeKey && tab.conversationKey===state.conversationKey) || null; }
  function terminalLabel(tab) { const duplicates=terminalTabs(tab.conversationKey).filter((candidate)=>candidate.cwd===tab.cwd),index=duplicates.indexOf(tab);return `${workspaceName(tab.cwd)||"Terminal"}${index>0?` ${index+1}`:""}`; }
  function terminalColors() {
    const dark=document.documentElement.dataset.theme==="dark";
    return dark?{
      background:"#181818",foreground:"#d4d4d4",cursor:"#d7d7d7",cursorAccent:"#181818",selectionBackground:"rgba(255,255,255,.18)",
      black:"#181818",red:"#f14c4c",green:"#40c977",yellow:"#cca700",blue:"#339cff",magenta:"#c586c0",cyan:"#4ec9b0",white:"#d4d4d4",
      brightBlack:"#808080",brightRed:"#ff6b6b",brightGreen:"#40c977",brightYellow:"#dcdcaa",brightBlue:"#339cff",brightMagenta:"#d7a0d2",brightCyan:"#69d8c0",brightWhite:"#ffffff",
    }:{
      background:"#ffffff",foreground:"#242424",cursor:"#242424",cursorAccent:"#ffffff",selectionBackground:"rgba(0,94,184,.18)",
      black:"#242424",red:"#c42b1c",green:"#00a240",yellow:"#8a6d00",blue:"#339cff",magenta:"#8b3f9f",cyan:"#087f8c",white:"#e5e5e5",
      brightBlack:"#6e6e6e",brightRed:"#d73a2f",brightGreen:"#00a240",brightYellow:"#9a7700",brightBlue:"#339cff",brightMagenta:"#a454b3",brightCyan:"#0f919f",brightWhite:"#ffffff",
    };
  }
  function syncTerminalTheme(){const theme=terminalColors();state.tabs.forEach((tab)=>{if(tab.terminal)tab.terminal.options.theme=theme;});}
  function mountTerminal(tab) {
    const host=$("termScreen");if(!host)return;
    const container=document.createElement("div");container.className="term-session";container.dataset.termSession=tab.key;host.append(container);tab.container=container;
    if(!terminalRuntime?.Terminal||!terminalRuntime?.FitAddon)return;
    const terminal=new terminalRuntime.Terminal({
      allowTransparency:false,
      cursorBlink:true,
      cursorStyle:"block",
      fontFamily:"ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace",
      fontSize:12.5,
      lineHeight:1.18,
      minimumContrastRatio:4.5,
      scrollback:6000,
      smoothScrollDuration:0,
      theme:terminalColors(),
    });
    const fit=new terminalRuntime.FitAddon();terminal.loadAddon(fit);terminal.open(container);
    terminal.attachCustomKeyEventHandler((event)=>!((event.ctrlKey||event.metaKey)&&(event.key.toLowerCase()==="j"||event.key==="`"||event.code==="Backquote")));
    tab.terminal=terminal;tab.fit=fit;tab.inputDisposable=terminal.onData((data)=>writeTab(tab,data));
  }
  function focusTerminal(){const tab=activeTab();if(tab?.terminal)tab.terminal.focus();else $("termScreen")?.focus();}
  function renderTabs() {
    const host=$("termTabs");if(!host)return;
    host.innerHTML=terminalTabs().map((tab)=>`<button type="button" class="term-tab" role="tab" data-term-tab="${escape(tab.key)}" aria-selected="${String(tab.key===state.activeKey)}" tabindex="${tab.key===state.activeKey?"0":"-1"}" title="${escape(tab.cwd)}"><span class="terminal-glyph" aria-hidden="true"><svg viewBox="0 0 20 20"><rect x="2.8" y="3.3" width="14.4" height="13.4" rx="2.7"/><path d="m5.8 7 2.7 2.7-2.7 2.7M10.5 12.4h3.6"/></svg></span><strong>${escape(terminalLabel(tab))}</strong><i data-term-tab-close aria-label="Close ${escape(terminalLabel(tab))}">×</i></button>`).join("");
    publishTerminalTabs();
  }
  function paint() {
    const screen = $("termScreen");
    if (!screen) return;
    const active=activeTab();
    state.tabs.forEach((tab)=>{if(tab.container)tab.container.hidden=tab!==active;});
    if(active?.terminal){requestAnimationFrame(()=>{if(active!==activeTab())return;measureCols();active.terminal.refresh(0,Math.max(0,active.terminal.rows-1));});return;}
    if(active?.container){active.container.textContent=active.buf||"";active.container.scrollTop=active.container.scrollHeight;}
  }

  function ingest(tab,chunk) {
    if(!tab)return;
    if(tab.terminal){tab.terminal.write(String(chunk||""));return;}
    let text = String(chunk || "");
    text = text.replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "");
    if(/\x1b\[(?:2J|3J)/.test(text))tab.buf="";
    text = text.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "");
    text = text.replace(/\x1b[PX^_][\s\S]*?\x1b\\/g, "");
    text = text.replace(/\x1b./g, "");
    for (const ch of text) {
      if (ch === "\r") {
        const last = tab.buf.lastIndexOf("\n");
        tab.buf = tab.buf.slice(0, last + 1);
      } else if (ch === "\b") {
        if (tab.buf.length && !tab.buf.endsWith("\n")) tab.buf = tab.buf.slice(0, -1);
      } else if (ch === "\u0007") {
        continue;
      } else {
        tab.buf += ch;
      }
    }
    if (tab.buf.length > 120000) tab.buf = tab.buf.slice(-80000);
    if(tab.key===state.activeKey&&tab.conversationKey===state.conversationKey)paint();
  }

  async function listen() {
    if (state.listening || preview || !window.__TAURI__?.event?.listen) return;
    state.listening = true;
    await window.__TAURI__.event.listen("term-data", (event) => {
      const id=event.payload?.id,tab=state.tabs.find((candidate)=>candidate.id===id);if(tab)ingest(tab,event.payload.data);else state.orphanOutput.set(id,`${state.orphanOutput.get(id)||""}${event.payload?.data||""}`);
    });
    await window.__TAURI__.event.listen("term-exit", (event) => {
      const id=event.payload?.id,tab=state.tabs.find((candidate)=>candidate.id===id);if(!tab)return;tab.id=0;tab.exited=true;ingest(tab,"\r\n\x1b[2m[process exited]\x1b[0m\r\n");renderTabs();ui.invoke("term_close",{id}).catch(()=>{});
    });
  }

  async function startShell() {
    const cwd = workspace();
    const tab={key:`term-${state.nextKey++}`,conversationKey:state.conversationKey,id:0,cwd:cwd||"~",buf:"",exited:false,terminal:null,fit:null,container:null,inputDisposable:null};state.tabs.push(tab);state.activeKey=tab.key;terminalView().activeKey=tab.key;mountTerminal(tab);renderTabs();paint();
    if (preview) {
      tab.id=-state.nextKey;ingest(tab,`\x1b[2mPhoenix terminal preview · ${cwd || "~"}\x1b[0m\r\n$ `);return tab;
    }
    await listen();
    const size = measureCols();
    try {
      tab.id = await ui.invoke("term_open", { cwd: cwd || null, cols: size.cols, rows: size.rows });const pending=state.orphanOutput.get(tab.id);if(pending){state.orphanOutput.delete(tab.id);ingest(tab,pending);}
    } catch (error) {
      tab.exited=true;ingest(tab,`\r\n\x1b[31mCould not start the terminal.\x1b[0m ${error.message || error}\r\n`);
    }
    renderTabs();return tab;
  }

  async function closeShell(key) {
    const index=state.tabs.findIndex((tab)=>tab.key===key);if(index<0)return;const tab=state.tabs[index],localIndex=terminalTabs(tab.conversationKey).findIndex((candidate)=>candidate.key===key);state.tabs.splice(index,1);
    if(tab.id)state.orphanOutput.delete(tab.id);if(!preview&&tab.id)try{await ui.invoke("term_close",{id:tab.id});}catch{}
    tab.inputDisposable?.dispose?.();tab.terminal?.dispose?.();tab.container?.remove?.();
    if(state.activeKey===key){const current=terminalTabs(),replacement=current[Math.min(Math.max(0,localIndex),current.length-1)];state.activeKey=replacement?.key||"";terminalView().activeKey=state.activeKey;}renderTabs();paint();
    if(!terminalTabs().length)toggleTerminal(false);
  }

  async function toggleTerminal(force) {
    const panel = $("termPanel");
    if (!panel) return;
    const open = force ?? !termOpen();
    clearTimeout(state.closeTimer);state.closeTimer=0;
    if(open){
      panel.hidden=false;
      panel.setAttribute("aria-hidden","false");
      // Commit the collapsed first frame before opening the grid row. Without
      // this read, browsers can coalesce both states and the panel simply pops.
      panel.getBoundingClientRect();
      requestAnimationFrame(()=>document.body.classList.add("term-open"));
    }else{
      document.body.classList.remove("term-open");
      panel.setAttribute("aria-hidden","true");
      state.closeTimer=window.setTimeout(()=>{if(!termOpen())panel.hidden=true;state.closeTimer=0;},340);
    }
    $("terminalToggle")?.setAttribute("aria-expanded",String(open));
    terminalView().open=Boolean(open);
    if (open) {
      if(!terminalTabs().length)await startShell();
      focusTerminal();
    }
  }

  function switchTerminalConversation(item){
    const nextKey=selectedConversationKey(item);if(nextKey===state.conversationKey)return;
    rememberTerminalView();state.conversationKey=nextKey;const view=terminalView(),tabs=terminalTabs();state.activeKey=tabs.some((tab)=>tab.key===view.activeKey)?view.activeKey:(tabs[0]?.key||"");view.activeKey=state.activeKey;renderTabs();paint();toggleTerminal(view.open);
  }

  function toggleRail() {
    document.body.classList.toggle("rail-hidden");
    localStorage.setItem("phoenix-rail-hidden", String(railHidden()));
    window.PhoenixConversation?.refreshPromptRail?.();
  }

  function toggleBrowser() {
    if (browserOpen()) window.PhoenixConversation?.closeBrowser?.();
    else {
      const item = ui.state.selected;
      window.PhoenixConversation?.openBrowser?.(item?.kind === "agent" ? item.id : "phoenix", "browse", "about:blank");
    }
  }

  async function toggleFullscreen() {
    const win = window.__TAURI__?.window?.getCurrentWindow?.();
    if (!win?.setFullscreen) {
      await ui.invoke?.("plugin:window|toggle_maximize")?.catch?.(() => {});
      return;
    }
    try {
      const on = await win.isFullscreen();
      await win.setFullscreen(!on);
    } catch {
      await win.toggleMaximize?.();
    }
  }

  function onboardingOpen() {
    return document.body.classList.contains("onboarding-open");
  }

  function openViewMenu() {
    if (onboardingOpen()) return;
    const button = $("viewMenuButton");
    const mark = (on) => on ? ui.checkIcon : "";
    const row = (action, label, keys, on) =>
      `<button type="button" data-view="${action}" class="${on ? "on" : ""}"><span>${mark(on)}${label}</span>${keys ? `<kbd>${keys}</kbd>` : ""}</button>`;
    const pop = ui.openPopover(button, `${row("rail", "Company rail", "Ctrl+\\", !sidebarCollapsed())}${row("term", "Terminal", "Ctrl+J", termOpen())}${row("browser", "Agent browser", "", browserOpen())}${row("settings", "Settings", "Ctrl+,", settingsOpen())}<div class="popover-separator"></div>${row("full", "Full screen", "F11", false)}`, "view-menu", { align: "start" });
    button.setAttribute("aria-expanded", "true");
    pop.onclick = (event) => {
      const action = event.target.closest("[data-view]")?.dataset.view;
      if (!action) return;
      ui.closeLayers();
      button.setAttribute("aria-expanded", "false");
      runView(action);
    };
  }

  function runView(action) {
    if (onboardingOpen()) return;
    if (action === "rail") {
      const collapsed = document.body.classList.contains("sidebar-collapsed");
      if (collapsed) $("sidebarWake")?.click();
      else $("sidebarToggle")?.click();
    }
    if (action === "term") toggleTerminal();
    if (action === "browser") toggleBrowser();
    if (action === "settings") {
      if (settingsOpen()) window.PhoenixSettings?.close?.();
      else window.dispatchEvent(new CustomEvent("phoenix:open-settings"));
    }
    if (action === "full") toggleFullscreen();
  }

  function bindKeys(event) {
    if (onboardingOpen()) return;
    const meta = event.ctrlKey || event.metaKey;
    if (meta && (event.key.toLowerCase() === "j" || event.key === "`" || event.code === "Backquote")) {
      event.preventDefault();
      toggleTerminal();
      return;
    }
    if (event.key === "F11") {
      event.preventDefault();
      toggleFullscreen();
    }
  }

  function bindTermInput(event) {
    if ($("termPanel")?.hidden || activeTab()?.terminal || document.activeElement !== $("termScreen")) return;
    if ((event.ctrlKey || event.metaKey) && (event.key.toLowerCase() === "j" || event.key === "`" || event.code === "Backquote")) return;
    if (event.ctrlKey && event.key === "c") {
      event.preventDefault();
      write("\u0003");
      return;
    }
    if (event.ctrlKey && event.key === "d") {
      event.preventDefault();
      write("\u0004");
      return;
    }
    if (event.ctrlKey && event.key === "l") {
      event.preventDefault();
      const tab=activeTab();if(tab)tab.buf="";
      paint();
      write("\f");
      return;
    }
    if (event.key === "Enter") { event.preventDefault(); write("\r"); return; }
    if (event.key === "Backspace") { event.preventDefault(); write("\u007f"); return; }
    if (event.key === "Tab") { event.preventDefault(); write("\t"); return; }
    if (event.key === "ArrowUp") { event.preventDefault(); write("\x1b[A"); return; }
    if (event.key === "ArrowDown") { event.preventDefault(); write("\x1b[B"); return; }
    if (event.key === "ArrowRight") { event.preventDefault(); write("\x1b[C"); return; }
    if (event.key === "ArrowLeft") { event.preventDefault(); write("\x1b[D"); return; }
    if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      write(event.key);
    }
  }

  async function write(data) {
    const tab=activeTab();if(!tab||tab.exited)return;
    return writeTab(tab,data);
  }

  async function writeTab(tab,data) {
    if(!tab||tab.exited)return;
    if (preview) {
      if (data === "\r") ingest(tab,"\r\n$ ");
      else if (data === "\u007f") ingest(tab,"\b \b");
      else ingest(tab,data.replace(/\n/g,"\r\n"));
      return;
    }
    if (!tab.id) return;
    try { await ui.invoke("term_write", { id: tab.id, data }); } catch {}
  }

  function bindResize() {
    const handle = $("termResize");
    if (!handle) return;
    handle.addEventListener("pointerdown", (event) => {
      if(event.button!==0)return;
      event.preventDefault();
      const startY = event.clientY;
      const startH = $("termPanel").getBoundingClientRect().height;
      let latestY=startY,frame=0;
      handle.setPointerCapture(event.pointerId);
      handle.classList.add("dragging");
      document.body.classList.add("term-resizing");
      const apply = () => {
        frame=0;
        const height = Math.max(140, Math.min(innerHeight * 0.62, startH + (startY - latestY)));
        document.documentElement.style.setProperty("--term-h", `${Math.round(height)}px`);
        handle.setAttribute("aria-valuenow",String(Math.round(height)));
        window.PhoenixConversation?.refreshPanelBounds?.();
      };
      const move = (next) => {
        latestY=next.clientY;
        if(!frame)frame=requestAnimationFrame(apply);
      };
      const finish = async () => {
        if(frame){cancelAnimationFrame(frame);apply();}
        handle.removeEventListener("pointermove", move);
        handle.removeEventListener("pointerup", finish);
        handle.removeEventListener("pointercancel", finish);
        handle.classList.remove("dragging");
        document.body.classList.remove("term-resizing");
        try{handle.releasePointerCapture(event.pointerId);}catch{}
        localStorage.setItem("phoenix-terminal-height",String(Math.round($("termPanel").getBoundingClientRect().height)));
        await resizeShells();
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", finish, { once: true });
      handle.addEventListener("pointercancel", finish, { once: true });
    });
  }
  async function resizeShells(){
    if(preview||document.body.classList.contains("term-resizing"))return;
    const size=measureCols(),tabs=terminalTabs().filter((tab)=>tab.id);
    if(!tabs.length)return;
    const resize=Promise.all(tabs.map((tab)=>ui.invoke("term_resize",{id:tab.id,cols:size.cols,rows:size.rows}).catch(()=>{})));
    state.resizePromise=resize;
    await resize;
    if(state.resizePromise===resize)state.resizePromise=null;
  }

  $("viewMenuButton")?.addEventListener("click", (event) => {
    if (onboardingOpen()) return;
    event.stopPropagation();
    if ($("viewMenuButton").getAttribute("aria-expanded") === "true") {
      ui.closeLayers();
      $("viewMenuButton").setAttribute("aria-expanded", "false");
      return;
    }
    openViewMenu();
  });
  $("termClose")?.addEventListener("click", () => toggleTerminal(false));
  $("termNewTab")?.addEventListener("click", async()=>{await startShell();focusTerminal();});
  $("termTabs")?.addEventListener("click",(event)=>{const button=event.target.closest("[data-term-tab]");if(!button)return;if(event.target.closest("[data-term-tab-close]")){closeShell(button.dataset.termTab);return;}state.activeKey=button.dataset.termTab;terminalView().activeKey=state.activeKey;renderTabs();paint();focusTerminal();resizeShells();});
  $("termTabs")?.addEventListener("keydown",(event)=>{if(!["ArrowLeft","ArrowRight","Home","End"].includes(event.key))return;const tabs=[...$("termTabs").querySelectorAll("[data-term-tab]")],current=Math.max(0,tabs.findIndex((tab)=>tab.dataset.termTab===state.activeKey)),next=event.key==="Home"?0:event.key==="End"?tabs.length-1:(current+(event.key==="ArrowLeft"?-1:1)+tabs.length)%tabs.length,key=tabs[next]?.dataset.termTab;if(!key)return;event.preventDefault();state.activeKey=key;terminalView().activeKey=state.activeKey;renderTabs();paint();$("termTabs").querySelector(`[data-term-tab="${CSS.escape(key)}"]`)?.focus();});
  $("termScreen")?.addEventListener("paste",(event)=>{if(activeTab()?.terminal)return;const text=event.clipboardData?.getData("text");if(text){event.preventDefault();write(text);}});
  $("terminalToggle")?.addEventListener("click", () => toggleTerminal());
  addEventListener("keydown", bindKeys);
  addEventListener("keydown", bindTermInput);
  addEventListener("phoenix:settings-visibility", () => $("viewMenuButton")?.setAttribute("aria-expanded", "false"));
  addEventListener("phoenix:theme-changed",syncTerminalTheme);
  addEventListener("phoenix:conversation-selected",(event)=>switchTerminalConversation(event.detail?.item));
  bindResize();
  if(globalThis.ResizeObserver&&$("termScreen")){const observer=new ResizeObserver(()=>{if(document.body.classList.contains("term-resizing"))return;clearTimeout(state.resizeTimer);state.resizeTimer=setTimeout(resizeShells,60);});observer.observe($("termScreen"));}
  const storedTerminalHeight=Number(localStorage.getItem("phoenix-terminal-height"));if(Number.isFinite(storedTerminalHeight)&&storedTerminalHeight>=140)document.documentElement.style.setProperty("--term-h",`${Math.min(storedTerminalHeight,innerHeight*.62)}px`);
  // v2 restores prompt markers for installs whose old View-menu experiment
  // left them permanently hidden. After this one-time migration, the user's
  // explicit View → Prompt markers choice remains authoritative.
  // v3: the rail became beUI's PreviewRail; show it again once.
  if (localStorage.getItem("phoenix-prompt-marker-pref-version") !== "3") {
    localStorage.setItem("phoenix-rail-hidden", "false");
    localStorage.setItem("phoenix-prompt-marker-pref-version", "3");
  }
  if (localStorage.getItem("phoenix-rail-hidden") === "true") document.body.classList.add("rail-hidden");

  window.PhoenixView = Object.freeze({ toggleTerminal, toggleRail, toggleBrowser, toggleFullscreen });
  if (new URLSearchParams(location.search).get("shot") === "view") {
    setTimeout(() => $("viewMenuButton")?.click(), 80);
  }
  if (new URLSearchParams(location.search).get("shot") === "term") {
    setTimeout(() => toggleTerminal(true), 80);
  }
  if (new URLSearchParams(location.search).get("shot") === "perf") {
    setTimeout(runNativeScrollProbe, 700);
  }

  async function runNativeScrollProbe() {
    const results = [];
    const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    async function measure(name, surface, rowSelector) {
      if (!surface) return;
      const additions = [], template = surface.querySelector(rowSelector);
      if (template) {
        while (surface.scrollHeight < surface.clientHeight * 5 && additions.length < 180) {
          const clone = template.cloneNode(true);
          clone.removeAttribute("id");
          clone.querySelectorAll("[id]").forEach((node) => node.removeAttribute("id"));
          clone.setAttribute("aria-hidden", "true");
          clone.style.pointerEvents = "none";
          surface.appendChild(clone);
          additions.push(clone);
        }
      }
      const startTop = surface.scrollTop, gaps = [], duration = 1300;
      let frames = 0, previous = performance.now(), started = previous;
      await new Promise((resolve) => {
        const tick = (now) => {
          const elapsed = now - started, max = Math.max(1, surface.scrollHeight - surface.clientHeight);
          gaps.push(now - previous); previous = now; frames += 1;
          const phase = Math.min(1, elapsed / duration);
          surface.scrollTop = max * (phase < .5 ? phase * 2 : (1 - phase) * 2);
          if (elapsed < duration) requestAnimationFrame(tick); else resolve();
        };
        requestAnimationFrame(tick);
      });
      surface.scrollTop = startTop;
      additions.forEach((node) => node.remove());
      const elapsed = gaps.reduce((total, gap) => total + gap, 0);
      results.push({ name, fps:elapsed > 0 ? frames * 1000 / elapsed : 0, worst:Math.max(...gaps), dropped:gaps.filter((gap) => gap > 25).length, frames });
      await pause(90);
    }
    await measure("sidebar", $("sidebarList"), ".company-row");
    await measure("conversation", $("conversationFeed"), ".message-row");
    window.dispatchEvent(new CustomEvent("phoenix:open-settings", { detail:{ section:"General" } }));
    await pause(800);
    await measure("settings", $("settingsMain"), ".setting-row");
    const fps = results.length ? Math.min(...results.map((result) => result.fps)) : 0;
    const worst = results.length ? Math.max(...results.map((result) => result.worst)) : 0;
    const dropped = results.reduce((total, result) => total + result.dropped, 0);
    const report = `PERF fps=${fps.toFixed(1)} worst_ms=${worst.toFixed(1)} dropped=${dropped} surfaces=${results.map((result) => `${result.name}:${result.fps.toFixed(1)}`).join(",")}`;
    try { await ui.invoke("perf_report", { report }); } catch { console.info(report); }
    document.documentElement.dataset.perfReport = report;
    // The standalone WebKit/Chromium probe cannot call Tauri's perf_report
    // command. Mirroring the result into the title gives the native harness a
    // zero-dependency completion signal without affecting normal launches.
    document.title = report;
  }
})();
