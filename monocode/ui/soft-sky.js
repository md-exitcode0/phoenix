/* Phoenix "soft sky": a living, time-of-day sky behind the MonoCode skin.
 *
 * Colours follow the local clock (night → pre-dawn → dawn → day → golden
 * hour → dusk → night), interpolated continuously and written as CSS
 * variables once a minute. Each theme keeps its own palette so text and
 * glass stay readable: Dark is a deep sky with light ink, Light a pale sky
 * with dark ink, and both move through the same day.
 *
 * Motion (drifting haze, twinkling stars, a horizon glow) is pure CSS
 * transform/opacity animation in soft-sky.css; this file never runs per
 * frame. It pauses the animations while the window is hidden.
 *
 * Preview hook: ?sky_time=06:30 freezes the sky at that local time.
 * Console: PhoenixSky.set("18:30"), PhoenixSky.set(null) to follow the clock.
 */
(function () {
  "use strict";
  const root = document.documentElement;

  // Keyframes: [minute of day, top, mid, low, horizon glow rgba, cloud tint,
  //             cloud alpha (far, near), star alpha]
  const DARK = [
    [0,    "#0b0e1f", "#131a35", "#1e2546", "rgb(96 110 200 / 12%)",  "#aab4ff", 0.10, 0.07, 1],
    [270,  "#0c1022", "#151c3a", "#222a4f", "rgb(110 116 210 / 14%)", "#aab4ff", 0.10, 0.07, 1],
    [330,  "#11163a", "#222957", "#463d6c", "rgb(176 132 206 / 20%)", "#c7b6ff", 0.12, 0.09, .7],
    [390,  "#1a2450", "#3f3f72", "#8a6078", "rgb(255 168 138 / 30%)", "#ffc6b4", 0.16, 0.13, .22],
    [450,  "#253a68", "#4b5686", "#8c7286", "rgb(255 196 156 / 28%)", "#ffe0cc", 0.17, 0.14, 0],
    [570,  "#213a64", "#2f4f80", "#4b6a96", "rgb(214 230 255 / 16%)", "#e4ecff", 0.17, 0.14, 0],
    [930,  "#213a64", "#2f4f80", "#4d6a94", "rgb(220 232 255 / 16%)", "#e4ecff", 0.17, 0.14, 0],
    [1050, "#24385f", "#3d4d78", "#7a6c82", "rgb(255 200 146 / 22%)", "#ffe2c4", 0.17, 0.14, 0],
    [1110, "#27325e", "#4d4776", "#94645c", "rgb(255 164 104 / 34%)", "#ffcf9e", 0.19, 0.14, 0],
    [1170, "#1d1f48", "#383368", "#6a4a7e", "rgb(214 124 176 / 28%)", "#e8b6e0", 0.14, 0.12, .25],
    [1230, "#141838", "#242a54", "#3b3768", "rgb(150 140 224 / 18%)", "#bcb8ff", 0.12, 0.09, .7],
    [1320, "#0d1124", "#161d3c", "#232a50", "rgb(110 116 210 / 14%)", "#aab4ff", 0.10, 0.07, .95],
    [1440, "#0b0e1f", "#131a35", "#1e2546", "rgb(96 110 200 / 12%)",  "#aab4ff", 0.10, 0.07, 1],
  ];
  const LIGHT = [
    [0,    "#b6bcda", "#c6c9e2", "#d4d2e8", "rgb(255 255 255 / 30%)", "#ffffff", .30, .22, .55],
    [270,  "#b8bddb", "#c8cae3", "#d8d3e8", "rgb(255 255 255 / 32%)", "#ffffff", .30, .22, .5],
    [330,  "#c0c2df", "#d2cbe3", "#e6d2de", "rgb(255 236 240 / 40%)", "#fff4f6", .34, .26, .3],
    [390,  "#cbd1ec", "#e6d5e1", "#f6d6c6", "rgb(255 214 186 / 55%)", "#fff1e8", .40, .30, 0],
    [480,  "#c6daf1", "#dce5f3", "#f1e8e4", "rgb(255 246 236 / 50%)", "#ffffff", .44, .32, 0],
    [660,  "#bbd5f1", "#d5e4f5", "#ecf0f6", "rgb(255 255 255 / 55%)", "#ffffff", .46, .34, 0],
    [930,  "#bdd5f0", "#d7e3f4", "#eeeff4", "rgb(255 255 255 / 50%)", "#ffffff", .46, .34, 0],
    [1050, "#c3d4ee", "#dde0f0", "#f4e5da", "rgb(255 232 204 / 50%)", "#fff8ee", .44, .32, 0],
    [1110, "#d0d0e9", "#eedad2", "#fbd8b2", "rgb(255 206 150 / 60%)", "#fff1de", .42, .30, 0],
    [1170, "#c6c3e4", "#dbcde5", "#eed0d8", "rgb(246 200 222 / 50%)", "#fff4fa", .38, .28, .15],
    [1230, "#bcbfdf", "#cec9e4", "#ddd3e8", "rgb(236 230 255 / 40%)", "#ffffff", .32, .24, .4],
    [1320, "#b7bcda", "#c7c9e2", "#d5d2e8", "rgb(255 255 255 / 32%)", "#ffffff", .30, .22, .5],
    [1440, "#b6bcda", "#c6c9e2", "#d4d2e8", "rgb(255 255 255 / 30%)", "#ffffff", .30, .22, .55],
  ];

  const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const rgba = (s) => { const m = s.match(/rgb\((\d+) (\d+) (\d+) \/ ([\d.]+)%\)/); return [+m[1], +m[2], +m[3], +m[4] / 100]; };
  const lerp = (a, b, t) => a + (b - a) * t;
  const ease = (t) => t * t * (3 - 2 * t);
  const toHex = (c) => "#" + c.map((v) => Math.round(v).toString(16).padStart(2, "0")).join("");

  function sample(palette, minute) {
    let i = 0;
    while (i < palette.length - 2 && palette[i + 1][0] <= minute) i++;
    const a = palette[i], b = palette[i + 1], t = ease((minute - a[0]) / (b[0] - a[0] || 1));
    const mixHex = (k) => toHex(hex(a[k]).map((v, j) => lerp(v, hex(b[k])[j], t)));
    const ga = rgba(a[4]), gb = rgba(b[4]), g = ga.map((v, j) => lerp(v, gb[j], t));
    return {
      top: mixHex(1), mid: mixHex(2), low: mixHex(3),
      glow: `rgb(${Math.round(g[0])} ${Math.round(g[1])} ${Math.round(g[2])} / ${(g[3] * 100).toFixed(1)}%)`,
      cloud: mixHex(5), cloudFar: lerp(a[6], b[6], t), cloudNear: lerp(a[7], b[7], t), stars: lerp(a[8], b[8], t),
    };
  }

  // The horizon glow follows the sun east → west by day, a faint moon by night.
  function sunX(minute) {
    if (minute >= 360 && minute <= 1200) return 12 + ((minute - 360) / 840) * 76;
    return 72;
  }
  function phase(minute) {
    if (minute < 300 || minute >= 1290) return "night";
    if (minute < 360) return "predawn";
    if (minute < 480) return "dawn";
    if (minute < 1020) return "day";
    if (minute < 1140) return "golden";
    return "dusk";
  }

  function parseTime(value) {
    const m = String(value || "").match(/^(\d{1,2}):?(\d{2})$/);
    if (!m) return null;
    const h = +m[1], min = +m[2];
    return h < 24 && min < 60 ? h * 60 + min : null;
  }
  let forced = parseTime(new URLSearchParams(location.search).get("sky_time"));
  const clockMinute = () => { const d = new Date(); return d.getHours() * 60 + d.getMinutes() + d.getSeconds() / 60; };

  function ensureLayer() {
    if (document.getElementById("softSky") || !document.body) return;
    const sky = document.createElement("div");
    sky.id = "softSky";
    sky.setAttribute("aria-hidden", "true");
    sky.innerHTML = '<i class="sky-base"></i><i class="sky-glow"></i>'
      + '<span class="sky-stars"><i class="sky-stars-a"></i><i class="sky-stars-b"></i><i class="sky-stars-c"></i></span>'
      + '<i class="sky-clouds sky-clouds-far"></i><i class="sky-clouds sky-clouds-near"></i>';
    document.body.prepend(sky);
  }

  let last = null;
  function paint(smooth) {
    if (root.dataset.skin !== "monocode") return;
    ensureLayer();
    const minute = forced ?? clockMinute(), theme = root.dataset.theme === "dark" ? "dark" : "light";
    const s = sample(theme === "dark" ? DARK : LIGHT, minute), key = JSON.stringify(s) + theme;
    if (key === last) return;
    // A large jump (window shown after hours, theme change, forced time)
    // eases over ~1.5s; the per-minute steps are too small to see.
    if (smooth) { root.dataset.skyEase = "true"; clearTimeout(paint.easeTimer); paint.easeTimer = setTimeout(() => delete root.dataset.skyEase, 1800); }
    last = key;
    const st = root.style;
    st.setProperty("--sky-top", s.top); st.setProperty("--sky-mid", s.mid); st.setProperty("--sky-low", s.low);
    st.setProperty("--sky-glow", s.glow); st.setProperty("--sky-cloud", s.cloud);
    st.setProperty("--sky-cloud-far", s.cloudFar.toFixed(3)); st.setProperty("--sky-cloud-near", s.cloudNear.toFixed(3));
    st.setProperty("--sky-stars", s.stars.toFixed(3)); st.setProperty("--sky-sun-x", sunX(minute).toFixed(1) + "%");
    root.dataset.skyPhase = phase(minute);
    // Stars stop animating entirely while they are invisible.
    root.dataset.skyStars = s.stars > 0.02 ? "on" : "off";
  }

  let timer = 0;
  function schedule() {
    clearTimeout(timer);
    if (document.visibilityState === "hidden") return;
    // Tick on the minute boundary so the sky and the clock agree.
    timer = setTimeout(() => { paint(false); schedule(); }, 60000 - (Date.now() % 60000) + 50);
  }
  document.addEventListener("visibilitychange", () => {
    const hidden = document.visibilityState === "hidden";
    root.dataset.skyPaused = String(hidden);
    if (!hidden) { paint(true); schedule(); } else clearTimeout(timer);
  });
  addEventListener("phoenix:theme-changed", () => { last = null; paint(false); });
  new MutationObserver(() => { last = null; paint(false); }).observe(root, { attributes: true, attributeFilter: ["data-theme", "data-skin"] });

  window.PhoenixSky = {
    set(time) { forced = time == null ? null : parseTime(time); last = null; paint(true); return forced; },
    sample: (theme, time) => sample(theme === "dark" ? DARK : LIGHT, parseTime(time) ?? clockMinute()),
    phase: () => root.dataset.skyPhase,
  };

  function start() { paint(false); schedule(); }
  if (document.body) start(); else document.addEventListener("DOMContentLoaded", start, { once: true });
})();
