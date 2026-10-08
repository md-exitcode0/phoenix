// Shared component behaviour. Switch: squish while pressed, shake when a
// disabled switch is pressed. Works for every switch markup in components.css.
(() => {
  const SWITCH = ".ph-switch, .ember-toggle, .ember-check-label, input.onboarding-switch";
  const disabledOf = (el) => el.matches?.(".ember-check-label") ? el.querySelector(".ember-check-input")?.disabled : el.disabled;
  let pressed = null;
  const release = () => { pressed?.classList.remove("pressed"); pressed = null; };
  document.addEventListener("pointerdown", (event) => {
    const el = event.target.closest?.(SWITCH);
    if (!el) return;
    if (disabledOf(el)) {
      const target = el.matches(".ember-check-label") ? el.querySelector(".ember-check-input") || el : el;
      target.classList.remove("switch-shake"); void target.offsetWidth; target.classList.add("switch-shake");
      setTimeout(() => target.classList.remove("switch-shake"), 900);
      return;
    }
    pressed = el; el.classList.add("pressed");
  }, true);
  for (const type of ["pointerup", "pointercancel", "blur"]) window.addEventListener(type, release, true);
  document.addEventListener("pointerleave", (event) => { if (event.target === pressed) release(); }, true);
})();

// Dropdowns: any native <select> left in the page (Settings upgrades its own)
// becomes the shared menu-select trigger + popover, so every dropdown opens
// with the same Select motion. The <select> stays in the DOM, hidden, as the
// source of truth: forms, FormData and existing change listeners keep working.
(() => {
  const CHEVRON = '<svg class="select-chevron" viewBox="0 0 20 20" aria-hidden="true"><path d="m6 8 4 4 4-4"/></svg>';
  const valueSetter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value");
  const labelOf = (option) => (option?.label || option?.textContent || "").trim();

  function sync(select) {
    const button = select._phSelect;
    if (!button) return;
    button.disabled = select.disabled;
    button.querySelector("span").textContent = labelOf(select.selectedOptions[0]) || select.dataset.placeholder || "Choose option";
    button.classList.toggle("is-placeholder", !select.selectedOptions[0]);
  }

  function enhance(select) {
    if (select._phSelect || select.multiple || select.dataset.keepNative || !window.PhoenixUI?.openMenuSelect) return;
    // Settings converts its own selects; enhancing them too leaves two pickers.
    if (!select.isConnected || select.closest("#settingsView, #settingsContent, .settings-view")) return;
    const button = document.createElement("button");
    button.type = "button";
    button.className = "menu-select ph-select-trigger";
    button.setAttribute("aria-haspopup", "listbox");
    button.setAttribute("aria-label", select.getAttribute("aria-label") || select.closest("label")?.firstChild?.textContent?.trim() || "Choose option");
    button.innerHTML = `<span></span>${CHEVRON}`;
    select._phSelect = button;
    select.classList.add("ph-select-native");
    select.tabIndex = -1;
    select.setAttribute("aria-hidden", "true");
    select.after(button);
    // Programmatic `select.value = …` fires no event; keep the label honest.
    Object.defineProperty(select, "value", { configurable: true, get() { return valueSetter.get.call(this); }, set(v) { valueSetter.set.call(this, v); sync(this); } });
    new MutationObserver(() => sync(select)).observe(select, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled", "data-placeholder"] });
    select.addEventListener("change", () => sync(select));
    button.addEventListener("click", (event) => {
      event.preventDefault(); event.stopPropagation();
      if (select.disabled) return;
      const items = [...select.options].filter((option) => !option.disabled || option.selected).map((option) => [option.value, labelOf(option)]);
      window.PhoenixUI.openMenuSelect(button, items, select.value, (value) => {
        if (String(value) === select.value) return;
        valueSetter.set.call(select, value); sync(select);
        select.dispatchEvent(new Event("input", { bubbles: true }));
        select.dispatchEvent(new Event("change", { bubbles: true }));
      }, { search: items.length > 12 });
    });
    sync(select);
  }

  const scan = (root) => root.querySelectorAll?.("select").forEach(enhance);
  const start = () => {
    scan(document);
    new MutationObserver((records) => {
      for (const record of records) for (const node of record.addedNodes) {
        if (node.nodeType !== 1) continue;
        node.matches("select") ? enhance(node) : scan(node);
      }
    }).observe(document.body, { childList: true, subtree: true });
  };
  document.readyState === "loading" ? document.addEventListener("DOMContentLoaded", start) : start();
})();

// Tabs (beUI Tabs, pill variant) for every segmented picker in Settings.
// One ink pill slides to the chosen tab; each label has an inverted copy
// clipped to the pill, so text flips colour exactly where the pill covers it.
// Settings re-renders pickers on every click, so the pill's last position is
// remembered per picker and the new pill glides from there.
(() => {
  const SELECTOR = ".visual-choices, .settings-theme, .model-hub-tabs";
  const ACTIVE = '[aria-pressed="true"], .selected, .active, [aria-checked="true"]';
  const last = new Map();
  const reduce = matchMedia("(prefers-reduced-motion: reduce)");
  const keyOf = (list) => list.id || list.getAttribute("aria-label") || list.closest("[data-setting-key]")?.dataset.settingKey || [...list.querySelectorAll("button")].map((b) => b.textContent.trim()).join("|");

  function pinLabels(list) {
    list.querySelectorAll(".ph-tab-label.pinned").forEach((label) => {
      const source = label._source; if (!source) return;
      Object.assign(label.style, { left: `${source.offsetLeft}px`, top: `${source.offsetTop}px`, width: `${source.offsetWidth}px`, height: `${source.offsetHeight}px` });
    });
  }
  function syncClips(list, pill) {
    const box = pill.getBoundingClientRect();
    list.querySelectorAll(".ph-tab-label").forEach((label) => {
      const bounds = label.getBoundingClientRect();
      const left = Math.max(0, Math.min(bounds.width, box.left - bounds.left));
      const right = Math.max(0, Math.min(bounds.width, bounds.right - box.right));
      label.style.clipPath = left + right >= bounds.width ? "inset(0 100% 0 0)" : `inset(0 ${right}px 0 ${left}px)`;
    });
  }

  function place(list) {
    const pill = list.querySelector(":scope > .ph-tab-pill"), active = list.querySelector(`:scope > button:is(${ACTIVE})`);
    if (!pill) return;
    pinLabels(list);
    if (!active) { pill.style.opacity = "0"; syncClips(list, pill); return; }
    const key = keyOf(list), target = { x: active.offsetLeft, y: active.offsetTop, w: active.offsetWidth, h: active.offsetHeight };
    const from = last.get(key);
    last.set(key, target);
    const apply = (rect) => { pill.style.transform = `translate(${rect.x}px, ${rect.y}px)`; pill.style.width = `${rect.w}px`; pill.style.height = `${rect.h}px`; };
    pill.style.opacity = "1";
    if (!from || reduce.matches || (from.x === target.x && from.w === target.w)) { pill.style.transition = "none"; apply(target); syncClips(list, pill); return; }
    pill.style.transition = "none"; apply(from); pill.getBoundingClientRect();
    pill.style.transition = "";
    apply(target);
    const until = performance.now() + 700;
    const frame = () => { syncClips(list, pill); if (performance.now() < until && list.isConnected) requestAnimationFrame(frame); };
    requestAnimationFrame(frame);
  }

  function enhance(list) {
    if (list._phTabs) { place(list); return; }
    list._phTabs = true;
    list.classList.add("ph-tabs");
    list.setAttribute("role", "tablist");
    const pill = document.createElement("span"); pill.className = "ph-tab-pill"; pill.setAttribute("aria-hidden", "true");
    list.prepend(pill);
    list.querySelectorAll(":scope > button").forEach((button) => {
      button.classList.add("ph-tab");
      button.setAttribute("role", "tab");
      // Buttons wrapping one element get an exact clone pinned over it, so the
      // inverted copy keeps that element's own layout (dots, icons, gaps).
      const only = button.children.length === 1 && ![...button.childNodes].some((node) => node.nodeType === 3 && node.textContent.trim()) ? button.firstElementChild : null;
      let label;
      if (only) { label = only.cloneNode(true); label.classList.add("ph-tab-label", "pinned"); label._source = only; }
      else { label = document.createElement("span"); label.className = "ph-tab-label"; label.innerHTML = button.innerHTML; }
      label.setAttribute("aria-hidden", "true"); label.inert = true;
      button.append(label);
    });
    // The owning code flips aria-pressed/.selected in place on some pickers.
    new MutationObserver(() => {
      list.querySelectorAll(":scope > button").forEach((button) => button.setAttribute("aria-selected", String(button.matches(ACTIVE))));
      place(list);
    }).observe(list, { subtree: true, attributes: true, attributeFilter: ["class", "aria-pressed", "aria-checked"] });
    list.querySelectorAll(":scope > button").forEach((button) => button.setAttribute("aria-selected", String(button.matches(ACTIVE))));
    // A visible rerender needs a valid first frame even when the compositor
    // defers RAF. Hidden pickers still wait until layout can measure them.
    if (list.getClientRects().length) place(list);
    else requestAnimationFrame(() => place(list));
  }

  const scan = (root) => root.querySelectorAll?.(SELECTOR).forEach(enhance);
  const start = () => {
    scan(document);
    new MutationObserver((records) => {
      for (const record of records) for (const node of record.addedNodes) {
        if (node.nodeType !== 1) continue;
        node.matches(SELECTOR) ? enhance(node) : scan(node);
      }
    }).observe(document.body, { childList: true, subtree: true });
    addEventListener("resize", () => document.querySelectorAll(".ph-tabs").forEach((list) => { last.delete(keyOf(list)); place(list); }));
  };
  document.readyState === "loading" ? document.addEventListener("DOMContentLoaded", start) : start();
})();

// Sidebar glide (the reference SidebarNav's GlideMenu): one highlight per row
// group that glides to whichever row the pointer is over, instead of each row
// lighting up on its own. It fades in where it first lands and out on leave.
(() => {
  const ROW = ".company-row";
  const ease = "cubic-bezier(.16,1,.3,1)";
  let current = null;
  function glideFor(group) {
    let glide = group.querySelector(":scope > .row-glide");
    if (!glide) { glide = document.createElement("span"); glide.className = "row-glide"; glide.setAttribute("aria-hidden", "true"); group.prepend(glide); }
    return glide;
  }
  function moveTo(row) {
    const group = row.parentElement; if (!group) return;
    const glide = glideFor(group), fresh = !group.classList.contains("glide-active");
    if (current && current !== group) hide(current);
    current = group; group.classList.add("glide-active");
    const box = { x: row.offsetLeft, y: row.offsetTop, w: row.offsetWidth, h: row.offsetHeight };
    glide.style.transition = fresh ? "opacity .15s ease" : `transform .28s ${ease}, width .28s ${ease}, height .28s ${ease}, opacity .15s ease`;
    glide.style.width = `${box.w}px`; glide.style.height = `${box.h}px`;
    glide.style.transform = `translate(${box.x}px, ${box.y}px)`;
    glide.style.opacity = "1";
  }
  function hide(group) { group.classList.remove("glide-active"); const glide = group.querySelector(":scope > .row-glide"); if (glide) glide.style.opacity = "0"; if (current === group) current = null; }
  document.addEventListener("pointerover", (event) => {
    if (event.pointerType === "touch") return;
    const row = event.target.closest?.(ROW);
    if (row) moveTo(row);
    else if (current && !event.target.closest?.(".company-section, .sidebar-list, #companyList")) hide(current);
  });
  document.addEventListener("pointerout", (event) => {
    if (!current) return;
    const to = event.relatedTarget;
    if (!to || !current.contains(to)) { if (!to?.closest?.(ROW)) hide(current); }
  });
})();

// Tooltips: native `title` tooltips are drawn by the OS and land in the
// wrong place (and get cut off) once the interface is zoomed. Show the same
// text in an in-page tooltip that zooms with everything else.
(() => {
  let tip = null, owner = null, timer = 0;
  const hide = () => { clearTimeout(timer); timer = 0; tip?.classList.remove("visible"); owner = null; };
  const place = (el, text) => {
    if (!tip) { tip = document.createElement("div"); tip.className = "ph-tooltip"; tip.setAttribute("role", "tooltip"); document.body.append(tip); }
    tip.textContent = text;
    tip.classList.add("visible");
    const r = el.getBoundingClientRect(), t = tip.getBoundingClientRect(), gap = 8;
    let top = r.bottom + gap, left = r.left + r.width / 2 - t.width / 2;
    if (top + t.height > innerHeight - 4) top = r.top - t.height - gap;
    left = Math.max(6, Math.min(left, innerWidth - t.width - 6));
    tip.style.transform = `translate(${Math.round(left)}px, ${Math.round(Math.max(4, top))}px)`;
  };
  document.addEventListener("pointerover", (event) => {
    const el = event.target.closest?.("[title],[data-ph-tip]");
    if (!el || el === owner) return;
    if (el.hasAttribute("title")) {
      const text = el.getAttribute("title").trim();
      el.removeAttribute("title");
      if (!text) return;
      el.dataset.phTip = text;
      if (!el.hasAttribute("aria-label") && !el.textContent.trim()) el.setAttribute("aria-label", text);
    }
    hide(); owner = el;
    timer = setTimeout(() => { if (owner === el && el.isConnected) place(el, el.dataset.phTip); }, 450);
  }, true);
  document.addEventListener("pointerout", (event) => { if (owner && !owner.contains(event.relatedTarget)) hide(); }, true);
  addEventListener("scroll", hide, true);
  addEventListener("pointerdown", hide, true);
  addEventListener("keydown", hide, true);
})();
