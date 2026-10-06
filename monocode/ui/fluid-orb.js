"use strict";

/* Fluid send orb.
 *
 * While the agent works the button is a sphere of slowly mixing liquid. It
 * remains free-flowing until the pointer approaches the stop action. It then
 * gathers into a square. A drafted prompt still gathers into the send arrow.
 *
 * Perf notes for the WebKitGTK webview: the drawing surface is ~34 CSS px, so
 * even a 3-octave fbm is a few thousand fragments per frame. The loop only runs
 * while the orb is actually mixing (working, visible, page not hidden) and is
 * torn down the moment it settles, so an idle composer costs nothing. One
 * context is created for the app and reused.
 */
(() => {
  const VERT = `
attribute vec2 a_pos;
void main(){ gl_Position = vec4(a_pos, 0.0, 1.0); }
`;

  const FRAG = `
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform vec2 u_resolution;
uniform float u_time;
uniform float u_focus;   // 0 = free liquid, 1 = gathered into an action
uniform float u_shape;   // 0 = stop square, 1 = send arrow
uniform float u_seed;
uniform vec3 u_color;
uniform vec3 u_deep;

float hash(vec2 p){ return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123); }

float noise(vec2 p){
  vec2 i = floor(p);
  vec2 f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(
    mix(hash(i + vec2(0.0, 0.0)), hash(i + vec2(1.0, 0.0)), u.x),
    mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x),
    u.y);
}

float fbm(vec2 p){
  float v = 0.0;
  float a = 0.6;
  for (int i = 0; i < 3; i++){
    v += a * noise(p);
    p *= 2.0;
    a *= 0.5;
  }
  return v;
}

float segment(vec2 p, vec2 a, vec2 b, float r){
  vec2 pa = p - a, ba = b - a;
  float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
  return length(pa - ba * h) - r;
}

// Up arrow, matching the resting send glyph.
float arrowMask(vec2 p){
  float d = segment(p, vec2(0.0, -0.52), vec2(0.0, 0.46), 0.115);
  d = min(d, segment(p, vec2(-0.34, 0.10), vec2(0.0, 0.48), 0.115));
  d = min(d, segment(p, vec2(0.34, 0.10), vec2(0.0, 0.48), 0.115));
  return 1.0 - smoothstep(-0.03, 0.07, d);
}

float stopMask(vec2 p){
  vec2 q = abs(p) - vec2(0.25);
  float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - 0.055;
  return 1.0 - smoothstep(-0.03, 0.07, d);
}

void main(){
  vec2 uv = gl_FragCoord.xy / u_resolution.xy;
  vec2 c = (uv - 0.5) * 2.0;
  float t = u_time * 0.22;

  // Incommensurate frequencies so the currents never fall into a visible
  // repeating cycle — the liquid wanders instead of orbiting.
  vec2 drift = vec2(
    sin(t + u_seed) + 0.6 * sin(t * 1.73 + 1.3) + 0.35 * sin(t * 0.41 + u_seed * 2.1),
    cos(t * 0.83) + 0.6 * cos(t * 1.31 + 2.1) + 0.35 * cos(t * 0.37 + u_seed * 1.7));

  // As focus rises the sampling contracts toward the middle, so the currents
  // visibly pour inward rather than cross-fading in place.
  float gather = 1.0 - 0.55 * u_focus;
  vec2 p = vec2(c.x * 0.9, c.y * 0.5) * gather + drift * 0.7 * (1.0 - 0.7 * u_focus);

  vec2 q = vec2(fbm(p + drift), fbm(p + vec2(3.2, 1.5) - drift));
  float f = fbm(p + 1.2 * q);

  float g = clamp(0.5 - c.y * 0.5, 0.0, 1.0);
  float anchor = smoothstep(-1.0, -0.4, c.y);
  float shade = clamp(g + (f - 0.5) * 0.9 * anchor * (1.0 - 0.65 * u_focus), 0.0, 1.0);

  float actionShape = mix(stopMask(c), arrowMask(c), u_shape);
  shade = mix(shade, actionShape, u_focus * 0.92);

  vec3 pale = mix(u_color, vec3(1.0), 0.82);
  vec3 col = pale;
  col = mix(col, u_color, smoothstep(0.26, 0.54, shade));
  col = mix(col, u_deep, smoothstep(0.58, 0.9, shade));

  // GLSL does not define smoothstep when edge0 is greater than edge1. The
  // former reversed call happened to work on some drivers but rendered a
  // completely transparent canvas on NVIDIA. Keep the interval ordered and
  // invert the result explicitly.
  float edge = 1.0 - smoothstep(0.47, 0.5, length(uv - 0.5));
  gl_FragColor = vec4(col * edge, edge);
}
`;

  function compile(gl, type, source) {
    const shader = gl.createShader(type);
    if (!shader) return null;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      gl.deleteShader(shader);
      return null;
    }
    return shader;
  }

  function readColor(name, fallback) {
    const raw = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    const probe = document.createElement("span");
    probe.style.color = raw || fallback;
    document.body.appendChild(probe);
    const resolved = getComputedStyle(probe).color;
    probe.remove();
    const match = resolved.match(/-?[\d.]+/g);
    if (!match || match.length < 3) return [0.94, 0.36, 0.16];
    return [Number(match[0]) / 255, Number(match[1]) / 255, Number(match[2]) / 255];
  }

  function motionAllowed() {
    const mode = document.documentElement.dataset.motion || "";
    if (mode === "minimal") return false;
    return !window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  }

  class FluidOrb {
    constructor(canvas) {
      this.canvas = canvas;
      this.focus = 0;
      this.targetFocus = 0;
      this.shape = 0;
      this.targetShape = 0;
      this.running = false;
      this.frame = 0;
      this.start = 0;
      this.elapsed = 0;
      this.seed = Math.random() * 20;
      this.sized = "";
      this.gl = null;
      this.failed = false;
      this.paletteChanged = () => this.syncColor();
      addEventListener("phoenix:activity-color-changed", this.paletteChanged);
      addEventListener("pagehide", () => { removeEventListener("phoenix:activity-color-changed", this.paletteChanged); this.stop(); }, {once:true});
    }

    setup() {
      if (this.gl || this.failed) return Boolean(this.gl);
      const gl = this.canvas.getContext("webgl", { antialias: true, alpha: true, depth: false, stencil: false });
      if (!gl) { this.failed = true; return false; }
      const program = gl.createProgram();
      const vert = compile(gl, gl.VERTEX_SHADER, VERT);
      const frag = compile(gl, gl.FRAGMENT_SHADER, FRAG);
      if (!program || !vert || !frag) { this.failed = true; return false; }
      gl.attachShader(program, vert);
      gl.attachShader(program, frag);
      gl.linkProgram(program);
      if (!gl.getProgramParameter(program, gl.LINK_STATUS)) { this.failed = true; return false; }
      gl.useProgram(program);
      const buffer = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]), gl.STATIC_DRAW);
      const aPos = gl.getAttribLocation(program, "a_pos");
      gl.enableVertexAttribArray(aPos);
      gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 0, 0);
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      this.gl = gl;
      this.program = program;
      this.uResolution = gl.getUniformLocation(program, "u_resolution");
      this.uTime = gl.getUniformLocation(program, "u_time");
      this.uFocus = gl.getUniformLocation(program, "u_focus");
      this.uShape = gl.getUniformLocation(program, "u_shape");
      this.uSeed = gl.getUniformLocation(program, "u_seed");
      gl.uniform1f(this.uSeed, this.seed);
      this.syncColor();
      return true;
    }

    syncColor() {
      const gl = this.gl;
      if (!gl) return;
      const palette = getComputedStyle(document.documentElement).getPropertyValue("--activity-color").trim();
      const base = readColor(palette ? "--activity-color" : "--orb-color", "#ef5a1a");
      gl.uniform3f(gl.getUniformLocation(this.program, "u_color"), base[0], base[1], base[2]);
      gl.uniform3f(
        gl.getUniformLocation(this.program, "u_deep"),
        base[0] * 0.52, base[1] * 0.42, base[2] * 0.46);
    }

    resize() {
      const rect = this.canvas.getBoundingClientRect();
      if (!rect.width || !rect.height) return false;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const px = Math.max(8, Math.round(rect.width * dpr));
      const py = Math.max(8, Math.round(rect.height * dpr));
      const key = `${px}x${py}`;
      if (key !== this.sized) {
        this.canvas.width = px;
        this.canvas.height = py;
        this.gl.viewport(0, 0, px, py);
        this.gl.uniform2f(this.uResolution, px, py);
        this.sized = key;
      }
      return true;
    }

    /** target: "off" | "liquid" (free flow) | "stop" | "arrow" */
    set(target) {
      this.targetFocus = target === "stop" || target === "arrow" ? 1 : 0;
      this.targetShape = target === "arrow" ? 1 : 0;
      if (target === "off") { this.stop(); this.canvas.hidden = true; return false; }
      this.canvas.hidden = false;
      if (!this.setup()) { this.canvas.hidden = true; return false; }
      // Paint synchronously before waiting for the first animation frame. A
      // throttled or just-restored window must still show a usable control.
      this.focus += (this.targetFocus - this.focus) * 0.16;
      this.shape += (this.targetShape - this.shape) * 0.16;
      this.drawOnce();
      this.play();
      return true;
    }

    play() {
      if (this.running || this.failed) return !this.failed;
      if (!this.setup()) return false;
      if (!motionAllowed()) {
        this.focus = this.targetFocus;
        this.shape = this.targetShape;
        this.drawOnce();
        return true;
      }
      this.running = true;
      this.start = performance.now() - this.elapsed * 1000;
      const tick = (now) => {
        if (!this.running) return;
        this.elapsed = (now - this.start) / 1000;
        this.focus += (this.targetFocus - this.focus) * 0.16;
        this.shape += (this.targetShape - this.shape) * 0.16;
        this.render(this.elapsed);
        this.frame = requestAnimationFrame(tick);
      };
      this.frame = requestAnimationFrame(tick);
      return true;
    }

    drawOnce() {
      if (!this.setup()) return;
      this.render(this.elapsed || 0);
    }

    render(seconds) {
      const gl = this.gl;
      if (!gl || !this.resize()) return;
      gl.uniform1f(this.uTime, seconds);
      gl.uniform1f(this.uFocus, this.focus);
      gl.uniform1f(this.uShape, this.shape);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    }

    stop() {
      this.running = false;
      cancelAnimationFrame(this.frame);
      this.frame = 0;
    }
  }

  FluidOrb.prototype.elapsed = 0;

  window.PhoenixFluidOrb = FluidOrb;
})();
