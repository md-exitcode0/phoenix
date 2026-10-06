// Animated agent avatars. Two vector marks from avatar-morphs.js — "phoenix"
// (the original logo morph) and "orbit" — drawn in a chosen gradient.
//
// At rest an avatar holds its first pose. While its agent works it plays the
// morph; when the agent stops it keeps going to the end of the current cycle
// and settles back on the first pose, never freezing mid-shape.
//
// Pose geometry lives once in a hidden sprite; each avatar is a tiny <svg>
// with its own gradient and one <use> that points at the current pose. The
// clock is per agent, so a sidebar re-render (which replaces the element)
// picks up exactly where the previous element was.
(() => {
  const MORPHS = window.PHOENIX_MORPHS || {};
  const GRADIENTS = Object.freeze([
    ["01-amber", "Amber", ["#e85600", "#ff8b0b", "#ffd23d"]],
    ["02-coral", "Coral", ["#db3148", "#fb6b50", "#ffc071"]],
    ["03-rose", "Rose", ["#9d2664", "#e3589f", "#ffa5d0"]],
    ["04-violet", "Violet", ["#5029aa", "#9062e8", "#cab1ff"]],
    ["05-iris", "Iris", ["#253fa0", "#6c81e8", "#b8d1ff"]],
    ["06-ocean", "Ocean", ["#006184", "#1699c5", "#83e6e7"]],
    ["07-mint", "Mint", ["#126453", "#32a687", "#acf2bd"]],
    ["08-lime", "Lime", ["#567c0e", "#9bbe3c", "#ebeb74"]],
    ["09-gold", "Gold", ["#9d4a0b", "#d99528", "#ffe49b"]],
    ["10-ember", "Ember", ["#90222a", "#d85818", "#ffd260"]],
  ]);
  const KINDS = Object.keys(MORPHS);
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");
  let uid = 0;

  function ensureSprite() {
    if (document.getElementById("phoenixMorphSprite")) return;
    const defs = KINDS.map((kind) => MORPHS[kind].poses.map((pose, i) =>
      `<g id="phm-${kind}-${i}">${pose.map((d) => `<path d="${d}"/>`).join("")}</g>`).join("")).join("");
    const holder = document.createElement("div");
    holder.innerHTML = `<svg id="phoenixMorphSprite" width="0" height="0" style="position:absolute;width:0;height:0;overflow:hidden" aria-hidden="true"><defs>${defs}</defs></svg>`;
    document.body.prepend(holder.firstElementChild);
  }

  const stopsOf = (gradient, custom) =>
    gradient === "custom" && Array.isArray(custom) && custom.length === 3 ? custom
      : (GRADIENTS.find(([id]) => id === gradient) || GRADIENTS[0])[2];

  // Nearest preset to an agent's existing color, so defaults feel familiar.
  function nearestGradient(color) {
    const rgb = (hex) => { const n = parseInt(String(hex).replace("#", "").padEnd(6, "0").slice(0, 6), 16); return [n >> 16 & 255, n >> 8 & 255, n & 255]; };
    const [r, g, b] = rgb(color || "#e85600");
    let best = GRADIENTS[0][0], bestDistance = Infinity;
    for (const [id, , stops] of GRADIENTS) {
      const [sr, sg, sb] = rgb(stops[1]), distance = (r - sr) ** 2 + (g - sg) ** 2 + (b - sb) ** 2;
      if (distance < bestDistance) { bestDistance = distance; best = id; }
    }
    return best;
  }

  const esc = (value) => String(value).replace(/[&<>"']/g, (ch) => `&#${ch.charCodeAt(0)};`);

  // `agent` ties the avatar to that agent's working state; `demo` plays it
  // continuously (the avatar picker's previews).
  function markup({ kind = "phoenix", gradient = "01-amber", stops = null, agent = "", demo = false, className = "" } = {}) {
    const morph = MORPHS[kind] || MORPHS.phoenix;
    if (!morph) return "";
    ensureSprite();
    const id = `phmg${++uid}`, [x, y, w, h] = morph.viewBox, colors = stopsOf(gradient, stops);
    return `<svg class="morph-avatar ${esc(className)}" viewBox="${x} ${y} ${w} ${h}" data-morph="${esc(kind in MORPHS ? kind : "phoenix")}"${agent ? ` data-agent="${esc(agent)}"` : ""}${demo ? " data-demo" : ""} aria-hidden="true">`
      + `<defs><linearGradient id="${id}" gradientUnits="userSpaceOnUse" x1="${x}" y1="${y}" x2="${x + w}" y2="${y + h}">`
      + colors.map((color, i) => `<stop offset="${i / (colors.length - 1)}" stop-color="${esc(color)}"/>`).join("")
      + `</linearGradient></defs><use href="#phm-${esc(kind in MORPHS ? kind : "phoenix")}-0" fill="url(#${id})"/></svg>`;
  }

  // ── Clocks ───────────────────────────────────────────────────────────────
  // key → { start, stopping }. A clock exists only while that key animates.
  const clocks = new Map();
  let workingAgents = new Set(), raf = 0;

  const cycleOf = (morph) => morph.duration * (morph.mode === "pingpong" ? 2 : 1);

  function poseAt(morph, elapsed) {
    let t = (elapsed % cycleOf(morph)) / morph.duration;
    if (t > 1) t = 2 - t;
    const slots = morph.timeline;
    for (const [pose, from, to] of slots) if (t >= from && t < to) return pose;
    return slots[slots.length - 1][0];
  }

  function clockKey(el) { return el.hasAttribute("data-demo") ? `demo:${el.dataset.morph}` : el.dataset.agent ? `agent:${el.dataset.agent}` : ""; }
  function wantsMotion(el) { return el.hasAttribute("data-demo") || workingAgents.has(el.dataset.agent); }

  function tick(now) {
    raf = 0;
    const avatars = document.querySelectorAll("svg.morph-avatar");
    const seen = new Set();
    for (const el of avatars) {
      const key = clockKey(el), morph = MORPHS[el.dataset.morph];
      if (!key || !morph) continue;
      seen.add(key);
      let clock = clocks.get(key);
      const moving = wantsMotion(el) && !reduceMotion.matches;
      if (moving && !clock) clocks.set(key, clock = { start: now, stopping: false });
      if (clock) {
        if (moving) clock.stopping = false;
        else if (!clock.stopping) { clock.stopping = true; clock.stopAt = clock.start + Math.ceil((now - clock.start) / cycleOf(morph)) * cycleOf(morph); }
      }
      let pose = 0;
      if (clock) {
        if (clock.stopping && now >= clock.stopAt) clocks.delete(key);
        else pose = poseAt(morph, now - clock.start);
      }
      const href = `#phm-${el.dataset.morph}-${pose}`, use = el.lastElementChild;
      if (use && use.getAttribute("href") !== href) use.setAttribute("href", href);
    }
    // A clock whose avatars all left the page stops with them.
    for (const key of clocks.keys()) if (!seen.has(key)) clocks.delete(key);
    if (clocks.size || workingAgents.size || document.querySelector("svg.morph-avatar[data-demo]")) schedule();
  }
  function schedule() { if (!raf) raf = requestAnimationFrame(tick); }

  // Which agents are working: the sidebar marks their rows `.working`.
  function readWorking() {
    const next = new Set([...document.querySelectorAll(".company-row.working[data-id]")].map((row) => row.dataset.id));
    const changed = next.size !== workingAgents.size || [...next].some((id) => !workingAgents.has(id));
    workingAgents = next;
    if (changed || next.size || clocks.size) schedule();
  }
  function start() {
    readWorking();
    new MutationObserver(() => { readWorking(); if (document.querySelector("svg.morph-avatar[data-demo]")) schedule(); })
      .observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["class"] });
  }
  document.readyState === "loading" ? document.addEventListener("DOMContentLoaded", start) : start();

  window.PhoenixMorphAvatar = Object.freeze({ markup, GRADIENTS, KINDS, nearestGradient, stopsOf, labelOf: (kind) => MORPHS[kind]?.name || kind });
})();
