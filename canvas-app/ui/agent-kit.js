// Shared pieces for the agent components (beUI agents kit, vanilla port):
// the Lucide icons they use and a unified-diff parser for file diffs.
(() => {
  const PATHS = {
    check: '<path d="M20 6 9 17l-5-5"/>',
    x: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
    chevronDown: '<path d="m6 9 6 6 6-6"/>',
    loader: '<path d="M21 12a9 9 0 1 1-6.219-8.56"/>',
    shieldCheck: '<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/><path d="m9 12 2 2 4-4"/>',
    circleAlert: '<circle cx="12" cy="12" r="10"/><path d="M12 8v4"/><path d="M12 16h.01"/>',
    terminal: '<path d="m7 11 2-2-2-2"/><path d="M11 13h4"/><rect width="18" height="18" x="3" y="3" rx="2" ry="2"/>',
    braces: '<path d="M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5c0 1.1.9 2 2 2h1"/><path d="M16 21h1a2 2 0 0 0 2-2v-5c0-1.1.9-2 2-2a2 2 0 0 1-2-2V5a2 2 0 0 0-2-2h-1"/>',
    wrench: '<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"/>',
    circleCheck: '<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>',
    circleX: '<circle cx="12" cy="12" r="10"/><path d="m15 9-6 6"/><path d="m9 9 6 6"/>',
    ban: '<circle cx="12" cy="12" r="10"/><path d="m4.9 4.9 14.2 14.2"/>',
    copy: '<rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>',
    rotateCcw: '<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/>',
    fileCode: '<path d="M4 22h14a2 2 0 0 0 2-2V7l-5-5H6a2 2 0 0 0-2 2v4"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="m5 12-3 3 3 3"/><path d="m9 18 3-3-3-3"/>',
    circleHelp: '<circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><path d="M12 17h.01"/>',
    messageText: '<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/><path d="M13 8H7"/><path d="M17 12H7"/>',
    arrowLeft: '<path d="m12 19-7-7 7-7"/><path d="M19 12H5"/>',
    arrowRight: '<path d="M5 12h14"/><path d="m12 5 7 7-7 7"/>',
    archive: '<rect width="20" height="5" x="2" y="3" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"/><path d="M10 12h4"/>',
    trash: '<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/>',
    cornerDownLeft: '<polyline points="9 10 4 15 9 20"/><path d="M20 4v7a4 4 0 0 1-4 4H4"/>',
    maximize: '<polyline points="15 3 21 3 21 9"/><polyline points="9 21 3 21 3 15"/><line x1="21" y1="3" x2="14" y2="10"/><line x1="3" y1="21" x2="10" y2="14"/>',
    minimize: '<polyline points="4 14 10 14 10 20"/><polyline points="20 10 14 10 14 4"/><line x1="14" y1="10" x2="21" y2="3"/><line x1="3" y1="21" x2="10" y2="14"/>',
    folder: '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>',
    folderOpen: '<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"/>',
    file: '<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/>',
    chevronRight: '<path d="m9 18 6-6-6-6"/>',
    listTodo: '<rect x="3" y="5" width="6" height="6" rx="1"/><path d="m3 17 2 2 4-4"/><path d="M13 6h8"/><path d="M13 12h8"/><path d="M13 18h8"/>',
  };

  function icon(name, className = "") {
    return `<svg class="lu ${className}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${PATHS[name] || ""}</svg>`;
  }

  // Unified diff → rows {type, oldLine, newLine, content}. Hunk headers set
  // the line counters; without them numbers stay blank rather than invented.
  function parseDiff(text) {
    const rows = [];
    let oldLine = null, newLine = null;
    for (const raw of String(text || "").replace(/\n$/, "").split("\n")) {
      const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
      if (hunk) { oldLine = Number(hunk[1]); newLine = Number(hunk[2]); continue; }
      if (/^(?:\+\+\+|---) /.test(raw) || /^diff --git|^index [0-9a-f]/.test(raw)) continue;
      if (raw.startsWith("+")) { rows.push({ type: "added", oldLine: null, newLine, content: raw.slice(1) }); if (newLine != null) newLine++; }
      else if (raw.startsWith("-")) { rows.push({ type: "removed", oldLine, newLine: null, content: raw.slice(1) }); if (oldLine != null) oldLine++; }
      else { rows.push({ type: "context", oldLine, newLine, content: raw.startsWith(" ") ? raw.slice(1) : raw }); if (oldLine != null) oldLine++; if (newLine != null) newLine++; }
    }
    return rows;
  }

  // beUI Loader variants in plain CSS (agent-components.css animates them).
  function loader(variant = "spinner", size = 16, label = "Loading") {
    const style = `--ld:${size}px`;
    let inner = "";
    if (variant === "helix") inner = Array.from({ length: 7 }, (_, r) => `<i style="--row:${r}"></i><i class="b" style="--row:${r}"></i>`).join("");
    else if (variant === "dot-matrix") inner = Array.from({ length: 9 }, (_, i) => `<i style="--d:${(i % 3) + Math.floor(i / 3)}"></i>`).join("");
    else if (variant === "bars") inner = "<i></i><i></i><i></i><i></i>";
    else if (variant === "dots") inner = "<i></i><i></i><i></i>";
    else if (variant === "comet") inner = "<b>" + Array.from({ length: 6 }, (_, i) => `<i style="--t:${i}"></i>`).join("") + "</b>";
    else inner = '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10" opacity=".2"/><path d="M12 2a10 10 0 0 1 10 10"/></svg>';
    return `<span class="ld ld-${variant}" style="${style}" role="status" aria-label="${label}">${inner}</span>`;
  }

  // ── Springs ──────────────────────────────────────────────────────────
  // A small damped-spring integrator standing in for motion's springs.
  function spring(from, to, { stiffness = 180, damping = 20, mass = 1 } = {}, onUpdate, onDone) {
    let x = from, v = 0, last = performance.now(), frame = 0, stopped = false;
    const tick = (now) => {
      if (stopped) return;
      const dt = Math.min(0.032, (now - last) / 1000); last = now;
      const force = -stiffness * (x - to) - damping * v;
      v += (force / mass) * dt; x += v * dt;
      if (Math.abs(x - to) < 0.0015 && Math.abs(v) < 0.01) { onUpdate(to); onDone?.(); return; }
      onUpdate(x); frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return { stop() { stopped = true; cancelAnimationFrame(frame); }, get value() { return x; } };
  }
  const reduceMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

  // ── Goo popover (beUI Popover) ───────────────────────────────────────
  // The panel oozes out of its trigger: a goo-filtered blob morphs from the
  // trigger's pill to the panel rect while the same clip reveals the content.
  let openGoo = null;
  function gooPopover(trigger, content, { side = "top", align = "start", gap = 14, radius = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--radius")) + 6 || 16, blur = 8, onClose } = {}) {
    // A second click on the same trigger closes it: triggers toggle.
    if (openGoo && openGoo.trigger === trigger) { openGoo.close(); return null; }
    openGoo?.close(true);
    const id = `goo${Math.random().toString(36).slice(2, 8)}`;
    const layer = document.createElement("div");
    layer.className = "goo-layer";
    layer.innerHTML = `<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs><filter id="${id}" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur in="SourceGraphic" stdDeviation="${blur}" result="b"/><feColorMatrix in="b" mode="matrix" values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 22 -10" result="g"/><feComposite in="SourceGraphic" in2="g" operator="atop"/></filter></defs></svg><div class="goo-body" style="filter:url(#${id})"><div class="goo-pill"></div><div class="goo-blob"></div></div><div class="goo-clip"><div class="goo-panel" role="dialog"></div></div>`;
    const panel = layer.querySelector(".goo-panel"); panel.append(content);
    document.body.append(layer);
    const body = layer.querySelector(".goo-body"), pill = layer.querySelector(".goo-pill"), blob = layer.querySelector(".goo-blob"), clip = layer.querySelector(".goo-clip");
    let geo = null, progress = 0, anim = null, closed = false, outsideTimer = null;
    const lerp = (a, b, t) => a + (b - a) * t;
    function measure() {
      const t = trigger.getBoundingClientRect(), cW = panel.offsetWidth, cH = panel.offsetHeight;
      const py = side === "bottom" ? t.height + gap : -(gap + cH), px = align === "start" ? 0 : align === "end" ? t.width - cW : (t.width - cW) / 2;
      const left = Math.min(0, px), top = Math.min(0, py), W = Math.max(t.width, px + cW) - left, H = Math.max(t.height, py + cH) - top;
      geo = { W, H, left, top, tr: { x: -left, y: -top, w: t.width, h: t.height, r: Math.min(t.height / 2, radius) }, pn: { x: px - left, y: py - top, w: cW, h: cH, r: radius } };
      layer.style.transform = `translate3d(${t.left + left}px, ${t.top + top}px, 0)`;
      for (const el of [body, clip]) { el.style.width = `${W}px`; el.style.height = `${H}px`; }
      Object.assign(pill.style, { left: `${geo.tr.x}px`, top: `${geo.tr.y}px`, width: `${geo.tr.w}px`, height: `${geo.tr.h}px`, borderRadius: `${geo.tr.r}px` });
      panel.style.left = `${geo.pn.x}px`; panel.style.top = `${geo.pn.y}px`;
      // Punch the real trigger out of the goo so its own label stays visible.
      // The hole is 2px smaller than the trigger: the goo overlaps the
      // trigger's edge, so the two antialiased borders never leave a slit.
      const path = (r) => { const q = Math.max(0, Math.min(r.r, r.w / 2, r.h / 2)), n = (v) => v.toFixed(2); return `M${n(r.x + q)} ${n(r.y)}H${n(r.x + r.w - q)}A${n(q)} ${n(q)} 0 0 1 ${n(r.x + r.w)} ${n(r.y + q)}V${n(r.y + r.h - q)}A${n(q)} ${n(q)} 0 0 1 ${n(r.x + r.w - q)} ${n(r.y + r.h)}H${n(r.x + q)}A${n(q)} ${n(q)} 0 0 1 ${n(r.x)} ${n(r.y + r.h - q)}V${n(r.y + q)}A${n(q)} ${n(q)} 0 0 1 ${n(r.x + q)} ${n(r.y)}Z`; };
      const hole = { x: geo.tr.x + 2, y: geo.tr.y + 2, w: geo.tr.w - 4, h: geo.tr.h - 4, r: Math.max(0, geo.tr.r - 2) };
      body.style.clipPath = `path(evenodd, "${path({ x: 0, y: 0, w: W, h: H, r: 0 })} ${path(hole)}")`;
      render(progress);
    }
    function render(p) {
      if (!geo) return;
      const { tr, pn, W, H } = geo, r = { x: lerp(tr.x, pn.x, p), y: lerp(tr.y, pn.y, p), w: lerp(tr.w, pn.w, p), h: lerp(tr.h, pn.h, p), rr: lerp(tr.r, pn.r, p) };
      const inset = `inset(${r.y}px ${W - r.x - r.w}px ${H - r.y - r.h}px ${r.x}px round ${r.rr}px)`;
      blob.style.clipPath = inset; clip.style.clipPath = inset;
    }
    function animateTo(target, done) {
      anim?.stop();
      if (reduceMotion()) { progress = target; render(target); done?.(); return; }
      anim = spring(progress, target, target ? { stiffness: 190, damping: 22 } : { stiffness: 380, damping: 32 }, (value) => { progress = value; render(Math.max(0, value)); }, done);
    }
    const outside = (event) => { if (!panel.contains(event.target) && !trigger.contains(event.target)) api.close(); };
    const key = (event) => { if (event.key === "Escape") { event.preventDefault(); api.close(); trigger.focus?.(); } };
    const api = {
      panel, layer, trigger,
      close(immediate = false) {
        if (closed) return; closed = true; if (openGoo === api) openGoo = null;
        clearTimeout(outsideTimer);
        document.removeEventListener("pointerdown", outside, true); document.removeEventListener("keydown", key, true); removeEventListener("resize", measure);
        trigger.setAttribute("aria-expanded", "false"); trigger.classList.remove("goo-open"); panel.inert = true; clip.style.pointerEvents = "none";
        let done = false;
        const finish = () => { if (done) return; done = true; anim?.stop(); layer.remove(); onClose?.(); };
        // Springs run on rAF, which stops while the window is hidden; never
        // leave a half-closed layer behind.
        if (immediate) finish(); else { animateTo(0, finish); setTimeout(finish, 700); }
      },
      remeasure: measure,
    };
    trigger.setAttribute("aria-expanded", "true"); trigger.classList.add("goo-open");
    measure(); animateTo(1);
    requestAnimationFrame(measure);
    // Escape must work in the opening event turn. Only the outside pointer
    // listener waits, so the press that opens the popup cannot dismiss it.
    document.addEventListener("keydown", key, true);
    outsideTimer = setTimeout(() => { if (!closed) document.addEventListener("pointerdown", outside, true); }, 0);
    addEventListener("resize", measure);
    openGoo = api;
    return api;
  }

  // ── Inline slider (beUI InlineSlider) ────────────────────────────────
  // Label inside the left edge, readout inside the right, ten evenly spaced
  // snap stops (or one per option), an inset fill, and a thumb whose caps
  // part around the text as it passes. Drags follow the pointer exactly and
  // settle on the nearest stop with a spring on release.
  function inlineSlider({ label, stops, index = 0, format = (stop) => String(stop.label ?? stop.value), onInput, onCommit, ariaLabel, typed = null }) {
    const START = 8, END_INSET = 12, TEXT_INSET = 20;
    const root = document.createElement("div");
    root.className = "inline-slider";
    root.innerHTML = `<div class="is-fill-window" aria-hidden="true"><div class="is-fill"></div></div><div class="is-text" aria-hidden="true"><span class="is-label"></span><span class="is-readout"></span></div><div class="is-ticks" aria-hidden="true"></div><div class="is-thumb" aria-hidden="true"><span class="is-cap top"></span><span class="is-stem"></span><span class="is-cap bottom"></span></div><button type="button" class="is-handle" role="slider"></button>`;
    const fill = root.querySelector(".is-fill"), labelEl = root.querySelector(".is-label"), readout = root.querySelector(".is-readout"), ticksEl = root.querySelector(".is-ticks"), thumb = root.querySelector(".is-thumb"), handle = root.querySelector(".is-handle");
    const [capTop, stem, capBottom] = thumb.children;
    labelEl.textContent = label;
    handle.setAttribute("aria-label", ariaLabel || label); handle.setAttribute("aria-valuemin", "0"); handle.setAttribute("aria-valuemax", String(stops.length - 1));
    let current = Math.max(0, Math.min(stops.length - 1, index)), width = 0, x = 0, anim = null, gesture = null;
    const endX = () => Math.max(START, width - END_INSET);
    const stopX = (i) => stops.length === 1 ? START : START + (i / (stops.length - 1)) * (endX() - START);
    const nearest = (px) => stops.length === 1 ? 0 : Math.max(0, Math.min(stops.length - 1, Math.round((px - START) / Math.max(1, endX() - START) * (stops.length - 1))));
    function paintThumb(px) {
      x = px; thumb.style.transform = `translateX(${px}px)`;
      // The fill runs flush to the track's edges: no 2px strip either side.
      const right = px >= endX() ? width : px + 8;
      fill.style.transform = `translateX(${right - width}px)`;
      const lw = labelEl.offsetWidth, rw = readout.offsetWidth;
      const overlap = (a, b) => Math.max(0, Math.min(1, (px + 4 - a) / 6, (b - px) / 6));
      const split = Math.max(overlap(TEXT_INSET, TEXT_INSET + lw), overlap(width - TEXT_INSET - rw, width - TEXT_INSET));
      stem.style.opacity = String(1 - split); capTop.style.transform = `translateY(${-split}px)`; capBottom.style.transform = `translateY(${split}px)`;
    }
    function paintValue(i, live = false) {
      readout.textContent = format(stops[i], live);
      handle.setAttribute("aria-valuenow", String(i)); handle.setAttribute("aria-valuetext", readout.textContent);
      root.dataset.index = String(i); root.dataset.fraction = String(stops.length > 1 ? i / (stops.length - 1) : 1);
      root.dispatchEvent(new CustomEvent("is-change", { detail: { index: i, stop: stops[i] } }));
    }
    function paintTicks() {
      // A fine scale (context, every 1K) is continuous: no tick marks.
      if (stops.length > 24) { ticksEl.innerHTML = ""; return; }
      const lw = labelEl.offsetWidth, rw = readout.offsetWidth, hit = (px, a, b) => px + 2 >= a && px - 2 <= b;
      ticksEl.innerHTML = stops.map((_, i) => stopX(i)).filter((px) => !hit(px, TEXT_INSET, TEXT_INSET + lw) && !hit(px, width - TEXT_INSET - rw, width - TEXT_INSET)).map((px) => `<i style="left:${px}px"></i>`).join("");
    }
    function settle(i) {
      anim?.stop();
      const target = stopX(i);
      if (reduceMotion()) { paintThumb(target); return; }
      anim = spring(x, target, { stiffness: 300, damping: 30 }, paintThumb);
    }
    function commit(i) { const changed = i !== current; current = i; paintValue(i); settle(i); if (changed) onCommit?.(stops[i], i); }
    function layout() { width = root.getBoundingClientRect().width; if (!width) return; paintValue(current); paintTicks(); paintThumb(stopX(current)); }
    // With `typed`, clicking the number lets the user type a value; `typed`
    // maps the text to a stop index (or null to reject it).
    function editReadout() {
      const input = document.createElement("input");
      input.className = "is-edit"; input.type = "text"; input.value = readout.textContent; input.setAttribute("aria-label", `${ariaLabel || label} value`);
      root.append(input); root.classList.add("editing"); input.focus(); input.select();
      let done = false;
      const close = (apply) => {
        if (done) return; done = true;
        const i = apply ? typed(input.value) : null;
        input.remove(); root.classList.remove("editing");
        if (Number.isInteger(i)) commit(Math.max(0, Math.min(stops.length - 1, i)));
        handle.focus({ preventScroll: true });
      };
      input.addEventListener("keydown", (event) => { event.stopPropagation(); if (event.key === "Enter") { event.preventDefault(); close(true); } else if (event.key === "Escape") { event.preventDefault(); close(false); } });
      input.addEventListener("pointerdown", (event) => event.stopPropagation());
      input.addEventListener("blur", () => close(true));
    }
    root.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || gesture) return;
      if (typed) {
        const box = readout.getBoundingClientRect();
        if (event.clientX >= box.left - 6 && event.clientX <= box.right + 6 && event.clientY >= box.top - 6 && event.clientY <= box.bottom + 6) { event.preventDefault(); editReadout(); return; }
      }
      const rect = root.getBoundingClientRect(); event.preventDefault();
      const px = event.clientX - rect.left, offset = Math.abs(px - x - 2) <= 12 ? px - x : 2;
      gesture = { id: event.pointerId, left: rect.left, offset }; anim?.stop();
      root.classList.add("dragging"); root.setPointerCapture(event.pointerId); handle.focus({ preventScroll: true });
      // Grabbing the thumb drags it; a track click waits for release and
      // then glides to the nearest stop.
      gesture.grabbed = offset !== 2; gesture.clickX = px - 2; gesture.startX = event.clientX;
    });
    root.addEventListener("pointermove", (event) => {
      if (!gesture || gesture.id !== event.pointerId) return;
      if (!gesture.grabbed && Math.abs(event.clientX - gesture.startX) < 3) return;
      if (!gesture.grabbed) { gesture.grabbed = true; gesture.offset = 2; }
      const px = Math.min(endX(), Math.max(START, event.clientX - gesture.left - gesture.offset));
      gesture.moved = true; paintThumb(px);
      const i = nearest(px); if (String(i) !== root.dataset.index) { paintValue(i, true); onInput?.(stops[i], i); }
    });
    const end = (event) => {
      if (!gesture || gesture.id !== event.pointerId) return;
      const done = gesture; gesture = null; root.classList.remove("dragging");
      commit(nearest(done.moved ? x : done.clickX));
    };
    root.addEventListener("pointerup", end); root.addEventListener("pointercancel", end); root.addEventListener("lostpointercapture", end);
    handle.addEventListener("keydown", (event) => {
      const next = { ArrowRight: current + 1, ArrowUp: current + 1, ArrowLeft: current - 1, ArrowDown: current - 1, Home: 0, End: stops.length - 1, PageUp: stops.length - 1, PageDown: 0 }[event.key];
      if (next === undefined) return; event.preventDefault(); commit(Math.max(0, Math.min(stops.length - 1, next)));
    });
    new ResizeObserver(layout).observe(root);
    requestAnimationFrame(layout);
    return { root, set(i) { current = i; layout(); } };
  }

  window.PhoenixAgentKit = Object.freeze({ icon, parseDiff, loader, spring, gooPopover, inlineSlider });
})();
