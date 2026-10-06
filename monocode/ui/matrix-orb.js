// Dot-matrix orb: a round field of dots that breathes (idle), ripples
// (listening) or carries orbiting heat (thinking). Port of the user's React
// MatrixOrb to plain canvas for the vanilla UI.
(() => {
  const TAU = Math.PI * 2;
  const STATES = ["idle", "listening", "thinking"];
  const SCALE = { idle: 0.88, listening: 1, thinking: 0.92 };
  const STIFFNESS = 180, DAMPING = 26, ATTACK = 0.22, RELEASE = 0.08, BLEND = 0.16;
  const ORBITERS = [
    { radius: 0.62, speed: 2.2, phase: 0, spread: 0.42 },
    { radius: 0.4, speed: -1.7, phase: 2.1, spread: 0.36 },
    { radius: 0.8, speed: 1.15, phase: 4, spread: 0.34 },
  ];

  // No Math.abs: its corners read as a snap at every trough.
  function envelope(t) {
    const slow = 0.5 + 0.5 * Math.sin(t * 0.62 + 0.4);
    const fast = 0.5 + 0.5 * Math.sin(t * 1.9 + 1.1);
    return 0.22 + 0.78 * (0.45 + 0.55 * slow) * fast;
  }

  function intensityOf(state, d, nx, ny, t, amplitude) {
    if (state === "listening") {
      const ripple = 0.5 + 0.5 * Math.sin(d * 4.2 - t * 3);
      return 0.32 + amplitude * (0.34 + 0.38 * ripple);
    }
    if (state === "thinking") {
      let heat = 0;
      for (const o of ORBITERS) {
        const a = t * o.speed + o.phase;
        const dx = nx - Math.cos(a) * o.radius, dy = ny - Math.sin(a) * o.radius;
        heat += Math.exp(-(dx * dx + dy * dy) / (o.spread * o.spread));
      }
      return 0.26 + 0.8 * Math.min(1, heat);
    }
    return 0.62 + 0.12 * Math.sin(t * 1.05 - d * 2.4);
  }

  class MatrixOrb {
    constructor(canvas, { size = 72, color = "#F75001", dots = 11, state = "idle", level } = {}) {
      this.canvas = canvas; this.size = size; this.color = color; this.dots = dots;
      this.state = state; this.level = level;
      this.weights = { idle: 0, listening: 0, thinking: 0 }; this.weights[state] = 1;
      this.t = 0; this.amplitude = 0; this.scale = SCALE[state]; this.velocity = 0; this.raf = 0;
      this.reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
      this.resize();
      // Only animate while on screen; a hidden panel costs nothing.
      this.visible = false;
      this.observer = new IntersectionObserver((entries) => {
        this.visible = entries.some((entry) => entry.isIntersecting);
        this.visible ? this.start() : this.stop();
      });
      this.observer.observe(canvas);
    }
    setState(state, level) { if (STATES.includes(state)) this.state = state; this.level = level; if (this.reduce) this.drawStatic(); }
    resize() {
      const dpr = Math.min(window.devicePixelRatio || 1, 4), buffer = Math.round(this.size * dpr);
      this.dpr = dpr; this.canvas.width = this.canvas.height = buffer;
      this.canvas.style.width = this.canvas.style.height = `${this.size}px`;
      this.ctx = this.canvas.getContext("2d");
      // Scale by buffer/size, not dpr, so the transform stays exact when it rounds.
      this.ctx.setTransform(buffer / this.size, 0, 0, buffer / this.size, 0, 0);
    }
    levelAt(t) {
      const v = this.level;
      return v === undefined || !Number.isFinite(v) ? envelope(t) : Math.min(1, Math.max(0, v));
    }
    draw(t, amplitude, scale) {
      const { ctx, size } = this, grid = Math.max(3, Math.round(this.dots)), half = (grid - 1) / 2;
      const spacing = (size * 0.74) / (grid - 1), maxRadius = spacing * 0.6, center = size / 2;
      ctx.clearRect(0, 0, size, size); ctx.fillStyle = this.color;
      for (let iy = 0; iy < grid; iy++) for (let ix = 0; ix < grid; ix++) {
        const nx = (ix - half) / half, ny = (iy - half) / half, d = Math.hypot(nx, ny);
        // 1.12, not the square's 1.41 corner, is what makes the outline round.
        if (d > 1.12) continue;
        let blended = 0;
        for (const s of STATES) if (this.weights[s] >= 0.001) blended += this.weights[s] * intensityOf(s, d, nx, ny, t, amplitude);
        const radius = maxRadius * Math.exp(-d * d * 1.7) * Math.min(1, Math.max(0, blended)) * scale;
        // Anything under half a device pixel renders as haze, not a dot.
        if (radius * this.dpr < 0.5) continue;
        ctx.beginPath(); ctx.arc(center + (ix - half) * spacing * scale, center + (iy - half) * spacing * scale, radius, 0, TAU); ctx.fill();
      }
    }
    drawStatic() { for (const s of STATES) this.weights[s] = s === this.state ? 1 : 0; this.draw(0, this.levelAt(0), SCALE[this.state]); }
    start() {
      if (this.reduce) { this.drawStatic(); return; }
      if (this.raf) return;
      let last = performance.now();
      const frame = (now) => {
        const dt = Math.min((now - last) / 1000, 0.05); last = now; this.t += dt;
        const target = this.levelAt(this.t), rate = target > this.amplitude ? ATTACK : RELEASE;
        this.amplitude += (target - this.amplitude) * (1 - Math.pow(1 - rate, dt * 60));
        // Per-state weights, so an interrupted change blends from what is on screen.
        const step = 1 - Math.pow(1 - BLEND, dt * 60);
        for (const s of STATES) this.weights[s] += ((s === this.state ? 1 : 0) - this.weights[s]) * step;
        this.velocity += (-STIFFNESS * (this.scale - SCALE[this.state]) - DAMPING * this.velocity) * dt;
        this.scale += this.velocity * dt;
        this.draw(this.t, this.amplitude, this.scale);
        this.raf = this.visible ? requestAnimationFrame(frame) : 0;
      };
      this.raf = requestAnimationFrame(frame);
    }
    stop() { if (this.raf) cancelAnimationFrame(this.raf); this.raf = 0; }
  }

  window.PhoenixMatrixOrb = MatrixOrb;
  // Mount on every [data-matrix-orb] canvas already in the page.
  const mount = () => document.querySelectorAll("canvas[data-matrix-orb]").forEach((canvas) => {
    if (canvas._matrixOrb) return;
    canvas._matrixOrb = new MatrixOrb(canvas, { size: Number(canvas.dataset.size) || 72, state: canvas.dataset.state || "idle" });
  });
  document.readyState === "loading" ? document.addEventListener("DOMContentLoaded", mount) : mount();
})();
