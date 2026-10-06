import * as THREE from 'three';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';
import { GLTFExporter } from 'three/addons/exporters/GLTFExporter.js';
import { createSidekick } from './models.mjs';

export const KINDS = Object.freeze(['ember', 'cinder', 'kiln', 'wisp']);
export const EXPRESSIONS = Object.freeze(['bright', 'joy', 'calm', 'curious', 'mischief', 'sleepy']);

const SELECTOR = 'canvas.phoenix-sidekick[data-sidekick]';
const CACHE_LIMIT = 12;
const MAX_EDGE = 1024;
const FRAME_LIMIT = 8;
const DEFAULT_VIEW = Object.freeze({ yaw: 0.18, pitch: 0.06 });
const ATTRIBUTES = [
  'class', 'style', 'hidden', 'data-sidekick', 'data-color', 'data-expression',
  'data-activity', 'data-sidekick-active', 'data-sidekick-interactive',
  'data-sidekick-rotate', 'data-sidekick-paused', 'data-sidekick-static',
  'data-sidekick-wireframe', 'data-sidekick-framing', 'data-motion',
];
const records = new Map();
const cache = new Map();
const counts = { renders: 0, frames: 0, hits: 0, misses: 0, evictions: 0, errors: 0, rendererCreations: 0, exports: 0 };
let renderer, scene, camera, mount, centering, environmentTarget;
let mutations, intersections, resizes, motionQuery;
let installed = false;
let contextLost = false;
let graphicsError = null;
let reducedMotion = false;
let paused = false;
let fixedTime = null;
let elapsed = 0;
let lastTick = 0;
let frameHandle = 0;
let timerHandle = 0;
let idleHandle = 0;
let scanCursor = 0;
let exportTail = Promise.resolve();

const clamp = (n, min, max) => Math.min(max, Math.max(min, n));
const truthy = (value) => value === 'true' || value === '1';
const displayName = (kind) => kind ? kind[0].toUpperCase() + kind.slice(1) : 'Sidekick';
const currentTime = () => fixedTime ?? elapsed;
const snapshotAttributes = (canvas) => Object.fromEntries(
  ['aria-hidden', 'aria-label', 'role', 'tabindex', 'title'].map((name) => [name, canvas.getAttribute(name)]),
);

function validKind(kind) {
  if (!KINDS.includes(kind)) throw new RangeError(`Unknown sidekick model: ${String(kind)}`);
  return kind;
}

function validColor(color = '#e55732') {
  if (typeof color !== 'string' || !/^#[\da-f]{6}$/i.test(color)) {
    throw new TypeError('Sidekick color must be a six-digit hex color.');
  }
  return color.toLowerCase();
}

function stateOf(record) {
  return { ...record.config, ...record.overrides, yaw: record.yaw, pitch: record.pitch, lookX: record.lookX, lookY: record.lookY };
}

function cancelFrame() {
  if (frameHandle) cancelAnimationFrame(frameHandle);
  if (timerHandle) clearTimeout(timerHandle);
  frameHandle = timerHandle = 0;
  lastTick = 0;
}

function wake() {
  if (!installed || document.hidden || frameHandle || timerHandle) return;
  frameHandle = requestAnimationFrame(tick);
}

function dirtyAll() {
  for (const record of records.values()) record.dirty = true;
  wake();
}

function visible(record) {
  const canvas = record.canvas;
  if (!canvas.isConnected || document.hidden || record.intersecting === false) return false;
  if (canvas.checkVisibility && !canvas.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) return false;
  const bounds = canvas.getBoundingClientRect();
  if (bounds.width < 1 || bounds.height < 1 || bounds.bottom <= 0 || bounds.right <= 0 ||
      bounds.top >= window.innerHeight || bounds.left >= window.innerWidth) return false;
  const style = getComputedStyle(canvas);
  if (style.visibility === 'hidden' || style.visibility === 'collapse' || style.display === 'none') return false;
  if (record.width !== bounds.width || record.height !== bounds.height) {
    record.width = bounds.width;
    record.height = bounds.height;
    record.dirty = true;
  }
  return true;
}

function restoreAttributes(record, attributes = record.original.attributes) {
  for (const [name, value] of Object.entries(attributes)) {
    if (value === null) record.canvas.removeAttribute(name);
    else record.canvas.setAttribute(name, value);
  }
}

function interactiveAccessibility(record) {
  const interactive = stateOf(record).interactive;
  if (interactive) {
    record.canvas.tabIndex = 0;
    record.canvas.setAttribute('aria-hidden', 'false');
    record.canvas.setAttribute('role', 'img');
    const currentLabel = record.canvas.getAttribute('aria-label');
    if (!currentLabel || currentLabel === record.autoLabel) {
      record.autoLabel = `${displayName(stateOf(record).kind)} 3D preview. Drag or use arrow keys to rotate. Home resets the view. Space pauses motion.`;
      record.canvas.setAttribute('aria-label', record.autoLabel);
    }
  }
  const touchAction = interactive ? 'none' : record.original.touchAction;
  const cursor = interactive ? (record.pointer ? 'grabbing' : 'grab') : record.original.cursor;
  if (record.canvas.style.touchAction !== touchAction) record.canvas.style.touchAction = touchAction;
  if (record.canvas.style.cursor !== cursor) record.canvas.style.cursor = cursor;
}

function syncRecord(record) {
  const data = record.canvas.dataset;
  const authoredLabel = record.canvas.getAttribute('aria-label');
  if (authoredLabel && authoredLabel !== record.autoLabel && authoredLabel !== record.failureLabel) {
    record.original.attributes['aria-label'] = authoredLabel;
    if (record.beforeFailure) record.beforeFailure['aria-label'] = authoredLabel;
  }
  const config = {
    kind: data.sidekick,
    color: data.color || '#e55732',
    expression: EXPRESSIONS.includes(data.expression) ? data.expression : 'bright',
    activity: data.activity || (truthy(data.sidekickActive) ? 'working' : 'idle'),
    interactive: truthy(data.sidekickInteractive),
    autoRotate: truthy(data.sidekickRotate),
    paused: truthy(data.sidekickPaused),
    staticPose: truthy(data.sidekickStatic),
    wireframe: truthy(data.sidekickWireframe),
    framing: ['full', 'portrait'].includes(data.sidekickFraming) ? data.sidekickFraming : 'auto',
  };
  if (JSON.stringify(config) !== JSON.stringify(record.config)) {
    const wasInteractive = record.config?.interactive;
    record.config = config;
    record.dirty = true;
    if (wasInteractive && !config.interactive && !record.error) restoreAttributes(record);
    interactiveAccessibility(record);
  }
}

function dimensions(record) {
  const density = Math.min(2, Math.max(1, window.devicePixelRatio || 1));
  const ratio = Math.min(density, MAX_EDGE / Math.max(record.width, record.height));
  const width = Math.max(1, Math.round(record.width * ratio));
  const height = Math.max(1, Math.round(record.height * ratio));
  if (record.canvas.width !== width) record.canvas.width = width;
  if (record.canvas.height !== height) record.canvas.height = height;
  return { width, height };
}

function showFailure(record, message) {
  const state = stateOf(record);
  const label = `${displayName(state.kind)} 3D preview unavailable. ${message}`;
  const isNew = record.error !== message;
  if (!record.error) {
    record.beforeFailure = snapshotAttributes(record.canvas);
    record.beforeFailureText = record.canvas.textContent;
  }
  record.failureLabel = label;
  record.error = message;
  record.dirty = false;
  record.canvas.dataset.sidekickStatus = 'unavailable';
  record.canvas.setAttribute('role', 'img');
  record.canvas.setAttribute('aria-hidden', 'false');
  record.canvas.setAttribute('aria-label', label);
  record.canvas.setAttribute('title', label);
  record.canvas.textContent = label;
  const { width, height } = dimensions(record);
  const context = record.context;
  if (context) {
    context.save();
    context.clearRect(0, 0, width, height);
    const size = Math.min(width, height);
    context.fillStyle = '#82756b';
    context.globalAlpha = 0.14;
    context.beginPath();
    context.arc(width / 2, height / 2, size * 0.4, 0, Math.PI * 2);
    context.fill();
    context.globalAlpha = 1;
    context.fillStyle = '#9a624a';
    context.textAlign = 'center';
    context.textBaseline = 'middle';
    context.font = `600 ${Math.max(12, Math.round(size * 0.34))}px system-ui, sans-serif`;
    context.fillText('!', width / 2, height / 2 - (size >= 160 ? size * 0.09 : 0));
    if (size >= 160) {
      context.font = `${Math.max(12, Math.round(size * 0.042))}px system-ui, sans-serif`;
      context.fillText('3D preview unavailable', width / 2, height / 2 + size * 0.19, width * 0.85);
    }
    context.restore();
  } else if (!record.fallback) {
    record.fallback = document.createElement('span');
    record.fallback.className = 'phoenix-sidekick-fallback';
    record.fallback.setAttribute('role', 'status');
    record.fallback.textContent = record.width >= 96 ? '3D preview unavailable' : '!';
    record.fallback.setAttribute('aria-label', label);
    record.canvas.after(record.fallback);
  }
  if (isNew) {
    counts.errors++;
    record.canvas.dispatchEvent(new CustomEvent('phoenix-sidekick-error', { bubbles: true, detail: { kind: state.kind, message } }));
  }
}

function clearFailure(record) {
  if (record.error) {
    record.error = null;
    record.fallback?.remove();
    record.fallback = null;
    record.canvas.textContent = record.beforeFailureText ?? record.original.text;
    restoreAttributes(record, record.beforeFailure || record.original.attributes);
    record.beforeFailure = null;
    record.beforeFailureText = null;
    record.failureLabel = null;
    interactiveAccessibility(record);
  }
  if (record.canvas.dataset.sidekickStatus !== 'ready') record.canvas.dataset.sidekickStatus = 'ready';
}

function prepareEnvironment() {
  environmentTarget?.dispose();
  const room = new RoomEnvironment();
  const generator = new THREE.PMREMGenerator(renderer);
  try {
    environmentTarget = generator.fromScene(room, 0.04, 0.1, 100, { size: 128 });
    scene.environment = environmentTarget.texture;
    scene.environmentIntensity = 0.50;
  } finally {
    room.dispose();
    generator.dispose();
  }
}

function onContextLost(event) {
  event.preventDefault();
  contextLost = true;
  graphicsError = 'The graphics context was lost. It will recover when graphics are available.';
  cancelFrame();
  for (const record of records.values()) if (visible(record)) showFailure(record, graphicsError);
}

function onContextRestored() {
  try {
    prepareEnvironment();
    contextLost = false;
    graphicsError = null;
    dirtyAll();
  } catch (error) {
    graphicsError = `Graphics recovery failed: ${error.message}`;
    for (const record of records.values()) if (visible(record)) showFailure(record, graphicsError);
  }
}

function ensureRenderer() {
  if (graphicsError || contextLost) return null;
  if (renderer) return renderer;
  try {
    // This detached canvas is the only WebGL context; every public canvas is 2D.
    renderer = new THREE.WebGLRenderer({
      canvas: document.createElement('canvas'), alpha: true, antialias: true,
      premultipliedAlpha: true, preserveDrawingBuffer: false, powerPreference: 'low-power',
    });
    counts.rendererCreations++;
    renderer.setPixelRatio(1);
    renderer.setSize(1, 1, false);
    renderer.setClearColor(0x000000, 0);
    renderer.outputColorSpace = THREE.SRGBColorSpace;
    renderer.toneMapping = THREE.ACESFilmicToneMapping;
    renderer.toneMappingExposure = 0.92;
    scene = new THREE.Scene();
    camera = new THREE.OrthographicCamera(-2, 2, 2, -2, 0.01, 100);
    camera.position.set(0, 0, 12);
    camera.lookAt(0, 0, 0);
    mount = new THREE.Group();
    centering = new THREE.Group();
    mount.add(centering);
    scene.add(mount);
    scene.add(new THREE.HemisphereLight(0xfff7e9, 0x667381, 0.55));
    const key = new THREE.DirectionalLight(0xffead5, 2.0);
    key.position.set(-3.5, 5, 6);
    const fill = new THREE.DirectionalLight(0xc6dfff, 0.7);
    fill.position.set(4, 2, 3);
    const rim = new THREE.DirectionalLight(0xffefe4, 2.0);
    rim.position.set(2, 4, -4);
    scene.add(key, fill, rim);
    prepareEnvironment();
    renderer.domElement.addEventListener('webglcontextlost', onContextLost, false);
    renderer.domElement.addEventListener('webglcontextrestored', onContextRestored, false);
    return renderer;
  } catch (error) {
    const message = `WebGL is unavailable: ${error.message}`;
    releaseGraphics();
    graphicsError = message;
    return null;
  }
}

function inspectModel(model) {
  const geometrySet = new Set();
  const materials = new Set();
  let meshes = 0, triangles = 0, vertices = 0;
  model.object.traverse((object) => {
    if (!object.isMesh) return;
    meshes++;
    const geometry = object.geometry;
    geometrySet.add(geometry);
    vertices += geometry.attributes.position?.count || 0;
    triangles += (geometry.index?.count || geometry.attributes.position?.count || 0) / 3;
    for (const material of Array.isArray(object.material) ? object.material : [object.material]) materials.add(material);
  });
  return { materials, stats: { meshes, geometries: geometrySet.size, vertices, triangles: Math.round(triangles), materials: materials.size } };
}

function acquireModel(kind, color, quality) {
  const key = `${validKind(kind)}:${validColor(color)}:${quality}`;
  if (cache.has(key)) {
    const entry = cache.get(key);
    cache.delete(key);
    cache.set(key, entry);
    counts.hits++;
    return entry;
  }
  const model = createSidekick(kind, { color, quality });
  try {
    if (!model?.object?.isObject3D || typeof model.update !== 'function' || typeof model.dispose !== 'function') {
      throw new TypeError('The procedural sidekick model is incomplete.');
    }
    model.update(0, { expression: 'bright', lookX: 0, lookY: 0, activity: 'idle', reducedMotion: true });
    const box = new THREE.Box3().setFromObject(model.object);
    if (box.isEmpty() || ![...box.min.toArray(), ...box.max.toArray()].every(Number.isFinite)) {
      throw new Error('The sidekick has no finite mesh bounds.');
    }
    // Small avatars use the same model with a compact character-specific crop.
    // Ember needs more than its face in-frame: at 24–48 px its crest, shoulders,
    // wing edges and rising tail are the cues that keep the phoenix silhouette
    // distinct from a round owl/bird head.
    let portrait = null;
    const head = kind === 'wisp' ? null : model.object.getObjectByName('head');
    if (head) {
      const headBox = new THREE.Box3().setFromObject(head);
      if (!headBox.isEmpty()) {
        headBox.min.y -= (headBox.max.y - headBox.min.y) * 0.20;
        portrait = { center: headBox.getCenter(new THREE.Vector3()), size: headBox.getSize(new THREE.Vector3()) };
      }
    }
    if (kind === 'ember') {
      const emberBox = box.clone();
      const height = emberBox.max.y - emberBox.min.y;
      // Crop only the feet/lower-most tail arc. Keep the full lateral spread,
      // crest and most of the body, while limiting rear-tail depth so it does
      // not force the whole bird to become tiny in a square avatar.
      emberBox.min.y = emberBox.min.y + height * 0.15;
      emberBox.min.z = Math.max(emberBox.min.z, emberBox.max.z - 1.62);
      const emberSize = emberBox.getSize(new THREE.Vector3());
      emberSize.x *= 1.04;
      emberSize.y *= 1.03;
      emberSize.z *= 0.94;
      portrait = { center: emberBox.getCenter(new THREE.Vector3()), size: emberSize };
    }
    const entry = { key, kind, color, quality, model, portrait, center: box.getCenter(new THREE.Vector3()), size: box.getSize(new THREE.Vector3()), ...inspectModel(model) };
    cache.set(key, entry);
    counts.misses++;
    while (cache.size > CACHE_LIMIT) {
      const oldest = cache.keys().next().value;
      cache.get(oldest).model.dispose();
      cache.delete(oldest);
      counts.evictions++;
    }
    return entry;
  } catch (error) {
    model?.dispose?.();
    throw error;
  }
}

function drawRecord(record, seconds) {
  if (!record.context) return showFailure(record, 'A canvas drawing context is unavailable.');
  const state = stateOf(record);
  try {
    validKind(state.kind);
    validColor(state.color);
    if (!ensureRenderer()) return showFailure(record, graphicsError || 'Graphics are temporarily unavailable.');
    const quality = Math.max(record.width, record.height) >= 96 ? 'studio' : 'avatar';
    const entry = acquireModel(state.kind, state.color, quality);
    const { width, height } = dimensions(record);
    // Grow the one drawing buffer, never resize it between differently sized avatars.
    const source = renderer.domElement;
    if (source.width < width || source.height < height) {
      renderer.setSize(Math.max(source.width, width), Math.max(source.height, height), false);
    }
    renderer.setViewport(0, 0, width, height);
    renderer.setScissor(0, 0, width, height);
    renderer.setScissorTest(true);
    if (state.paused && record.frozenTime === null) record.frozenTime = record.lastTime ?? seconds;
    if (!state.paused) record.frozenTime = null;
    const poseTime = fixedTime !== null ? seconds : record.frozenTime ?? seconds;
    entry.model.update(poseTime, {
      expression: state.expression, lookX: state.lookX, lookY: state.lookY,
      activity: state.activity, reducedMotion,
    });
    const spinTime = fixedTime !== null ? poseTime : Math.max(0, poseTime - record.spinStarted);
    const spin = state.autoRotate && !reducedMotion ? spinTime * 0.23 : 0;
    mount.rotation.set(state.pitch, state.yaw + spin, 0, 'YXZ');
    const usePortrait = entry.portrait && state.framing !== 'full' &&
      (state.framing === 'portrait' || Math.max(record.width, record.height) <= 64);
    const frame = usePortrait ? entry.portrait : entry;
    record.renderedFraming = usePortrait ? 'portrait' : 'full';
    centering.position.copy(frame.center).multiplyScalar(-1);
    centering.add(entry.model.object);
    const horizontal = Math.hypot(frame.size.x, frame.size.z);
    const halfWidth = horizontal * 0.56;
    const halfHeight = (Math.abs(Math.cos(state.pitch)) * frame.size.y + Math.abs(Math.sin(state.pitch)) * horizontal) * 0.57;
    const viewHeight = Math.max(halfHeight, halfWidth / (width / height), 0.1);
    camera.left = -viewHeight * width / height;
    camera.right = -camera.left;
    camera.top = viewHeight;
    camera.bottom = -viewHeight;
    camera.updateProjectionMatrix();
    for (const material of entry.materials) if ('wireframe' in material) material.wireframe = state.wireframe;
    try {
      renderer.render(scene, camera);
      // WebGL viewport coordinates start at the bottom; 2D source rectangles start at the top.
      // Copy synchronously, before the browser can discard the WebGL drawing buffer.
      record.context.clearRect(0, 0, width, height);
      record.context.drawImage(source, 0, source.height - height, width, height, 0, 0, width, height);
    } finally {
      centering.remove(entry.model.object);
    }
    record.dirty = false;
    record.lastDraw = performance.now();
    record.modelStats = entry.stats;
    record.lastTime = poseTime;
    counts.renders++;
    clearFailure(record);
    record.canvas.dispatchEvent(new CustomEvent('phoenix-sidekick-render', { detail: { kind: state.kind, quality, time: seconds, stats: entry.stats } }));
  } catch (error) {
    showFailure(record, error.message || 'The model could not be rendered.');
  }
}

function animates(record) {
  const state = stateOf(record);
  if (paused || reducedMotion || fixedTime !== null || state.paused || record.error || graphicsError || contextLost) return false;
  if (state.staticPose && !record.hovered && !record.focused) return false;
  return Math.min(record.width, record.height) >= 96 || record.hovered || record.focused ||
    state.autoRotate || !['idle', 'paused', 'dormant'].includes(state.activity);
}

function fps(record) {
  return Math.min(record.width, record.height) < 96 ? 12 : stateOf(record).interactive ? 30 : 20;
}

function tick(now) {
  frameHandle = timerHandle = 0;
  if (!installed || document.hidden) { lastTick = 0; return; }
  if (lastTick && !paused && !reducedMotion && fixedTime === null) elapsed += Math.min(0.1, (now - lastTick) / 1000);
  lastTick = now;
  counts.frames++;
  const live = [];
  for (const record of records.values()) {
    if (!record.canvas.isConnected) { removeRecord(record); continue; }
    if (visible(record)) live.push(record);
  }
  let rendered = 0;
  let nextDelay = Infinity;
  // Rotate the starting index so a large roster cannot starve its last avatars.
  for (let offset = 0; offset < live.length; offset++) {
    const record = live[(scanCursor + offset) % live.length];
    const animate = animates(record);
    const interval = 1000 / fps(record);
    const due = record.dirty || (animate && now - record.lastDraw >= interval - 1);
    if (due && rendered < FRAME_LIMIT) {
      drawRecord(record, currentTime());
      rendered++;
    }
    if (record.dirty) nextDelay = Math.min(nextDelay, 16);
    if (animates(record)) nextDelay = Math.min(nextDelay, Math.max(8, interval - (performance.now() - record.lastDraw)));
  }
  if (live.length) scanCursor = (scanCursor + Math.max(1, rendered)) % live.length;
  if (Number.isFinite(nextDelay)) {
    timerHandle = setTimeout(() => { timerHandle = 0; wake(); }, Math.max(8, nextDelay));
  } else lastTick = 0;
}

function addListener(record, target, name, handler, options) {
  target.addEventListener(name, handler, options);
  record.listeners.push(() => target.removeEventListener(name, handler, options));
}

function bindInput(record) {
  const canvas = record.canvas;
  addListener(record, canvas, 'pointerenter', () => { record.hovered = true; record.dirty = true; wake(); });
  addListener(record, canvas, 'pointerleave', () => {
    record.hovered = false;
    if (!record.pointer) record.lookX = record.lookY = 0;
    record.dirty = true;
    wake();
  });
  addListener(record, canvas, 'pointerdown', (event) => {
    if (!stateOf(record).interactive || event.button !== 0) return;
    record.pointer = { id: event.pointerId, x: event.clientX, y: event.clientY };
    record.overrides.autoRotate = false;
    try { canvas.setPointerCapture(event.pointerId); } catch { /* Detached canvases can lose capture. */ }
    canvas.focus({ preventScroll: true });
    interactiveAccessibility(record);
    event.preventDefault();
  });
  addListener(record, canvas, 'pointermove', (event) => {
    const bounds = canvas.getBoundingClientRect();
    record.lookX = clamp((event.clientX - bounds.left) / Math.max(1, bounds.width) * 2 - 1, -1, 1);
    record.lookY = clamp(1 - (event.clientY - bounds.top) / Math.max(1, bounds.height) * 2, -1, 1);
    if (record.pointer?.id === event.pointerId) {
      record.yaw += (event.clientX - record.pointer.x) * 0.012;
      record.pitch = clamp(record.pitch + (event.clientY - record.pointer.y) * 0.009, -0.6, 0.6);
      record.pointer.x = event.clientX;
      record.pointer.y = event.clientY;
    }
    record.dirty = true;
    wake();
  });
  const release = (event) => {
    if (record.pointer?.id !== event.pointerId) return;
    record.pointer = null;
    if (canvas.hasPointerCapture?.(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
    if (!record.hovered) record.lookX = record.lookY = 0;
    record.dirty = true;
    interactiveAccessibility(record);
    wake();
  };
  addListener(record, canvas, 'pointerup', release);
  addListener(record, canvas, 'pointercancel', release);
  addListener(record, canvas, 'lostpointercapture', release);
  addListener(record, canvas, 'focus', () => { record.focused = true; record.dirty = true; wake(); });
  addListener(record, canvas, 'blur', () => { record.focused = false; record.dirty = true; wake(); });
  addListener(record, canvas, 'keydown', (event) => {
    if (!stateOf(record).interactive) return;
    const step = event.shiftKey ? 0.4 : 0.14;
    if (event.key === 'ArrowLeft') record.yaw -= step;
    else if (event.key === 'ArrowRight') record.yaw += step;
    else if (event.key === 'ArrowUp') record.pitch = clamp(record.pitch - step, -0.6, 0.6);
    else if (event.key === 'ArrowDown') record.pitch = clamp(record.pitch + step, -0.6, 0.6);
    else if (event.key === 'Home') { record.yaw = DEFAULT_VIEW.yaw; record.pitch = DEFAULT_VIEW.pitch; }
    else if (event.code === 'Space') record.overrides.paused = !stateOf(record).paused;
    else return;
    if (event.code !== 'Space') record.overrides.autoRotate = false;
    record.dirty = true;
    event.preventDefault();
    wake();
  });
}

export function makeRecord(canvas) {
  if (!canvas?.matches?.(SELECTOR)) throw new TypeError(`Expected ${SELECTOR}.`);
  if (records.has(canvas)) return records.get(canvas);
  ensureHooks();
  if (idleHandle) { clearTimeout(idleHandle); idleHandle = 0; }
  const record = {
    canvas, context: canvas.getContext('2d', { alpha: true }), config: {}, overrides: {},
    original: { attributes: snapshotAttributes(canvas), text: canvas.textContent, touchAction: canvas.style.touchAction, cursor: canvas.style.cursor },
    width: 0, height: 0, yaw: DEFAULT_VIEW.yaw, pitch: DEFAULT_VIEW.pitch, lookX: 0, lookY: 0, pointer: null,
    hovered: false, focused: false, intersecting: null, dirty: true, error: null,
    lastDraw: -Infinity, lastTime: null, frozenTime: null, spinStarted: currentTime(), listeners: [], fallback: null, modelStats: null,
    autoLabel: null, failureLabel: null, beforeFailure: null, beforeFailureText: null, renderedFraming: null,
  };
  records.set(canvas, record);
  syncRecord(record);
  bindInput(record);
  intersections?.observe(canvas);
  resizes?.observe(canvas);
  wake();
  return record;
}

function removeRecord(record) {
  for (const remove of record.listeners) remove();
  intersections?.unobserve(record.canvas);
  resizes?.unobserve(record.canvas);
  record.fallback?.remove();
  restoreAttributes(record);
  record.canvas.textContent = record.original.text;
  record.canvas.style.touchAction = record.original.touchAction;
  record.canvas.style.cursor = record.original.cursor;
  delete record.canvas.dataset.sidekickStatus;
  records.delete(record.canvas);
  if (!records.size && installed && !idleHandle) idleHandle = setTimeout(() => {
    idleHandle = 0;
    if (!records.size) releaseGraphics();
  }, 12000);
}

function discover(root) {
  if (root.matches?.(SELECTOR)) makeRecord(root);
  for (const canvas of root.querySelectorAll?.(SELECTOR) || []) makeRecord(canvas);
}

function onVisibilityChange() {
  if (document.hidden) cancelFrame();
  else dirtyAll();
}

function onMotionChange(event) {
  const next = !!(event?.matches ?? motionQuery?.matches) || document.documentElement.dataset.motion === 'minimal';
  if (next === reducedMotion) return;
  reducedMotion = next;
  lastTick = 0;
  dirtyAll();
  window.dispatchEvent(new CustomEvent('phoenix-sidekicks-motionchange', { detail: { reducedMotion } }));
}

function onResize() { dirtyAll(); }

function ensureHooks() {
  if (installed) return;
  if (typeof document === 'undefined') throw new Error('Sidekick rendering requires a document.');
  installed = true;
  motionQuery = window.matchMedia?.('(prefers-reduced-motion: reduce)');
  reducedMotion = !!motionQuery?.matches || document.documentElement.dataset.motion === 'minimal';
  motionQuery?.addEventListener('change', onMotionChange);
  document.addEventListener('visibilitychange', onVisibilityChange);
  window.addEventListener('resize', onResize, { passive: true });
  if (typeof IntersectionObserver !== 'undefined') intersections = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      const record = records.get(entry.target);
      if (!record) continue;
      record.intersecting = entry.isIntersecting;
      if (entry.isIntersecting) record.dirty = true;
    }
    wake();
  });
  if (typeof ResizeObserver !== 'undefined') resizes = new ResizeObserver((entries) => {
    for (const entry of entries) {
      const record = records.get(entry.target);
      if (record) record.dirty = true;
    }
    wake();
  });
  mutations = new MutationObserver((changes) => {
    let relevant = false;
    for (const change of changes) {
      if (change.type === 'childList') {
        for (const node of change.addedNodes) if (node.nodeType === 1) discover(node);
        if (change.removedNodes.length) relevant = true;
      } else {
        if (change.attributeName === 'data-motion' && change.target === document.documentElement) onMotionChange();
        if (change.target.matches?.(SELECTOR)) makeRecord(change.target);
        for (const record of records.values()) {
          if (record.canvas === change.target) {
            if (!record.canvas.matches(SELECTOR)) removeRecord(record);
            else syncRecord(record);
            relevant = true;
          } else if (change.target.contains(record.canvas)) {
            record.dirty = true;
            relevant = true;
          }
        }
      }
    }
    if (relevant) {
      for (const record of records.values()) if (!record.canvas.isConnected) removeRecord(record);
      wake();
    }
  });
  mutations.observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ATTRIBUTES });
}

export function refresh(root = document) {
  ensureHooks();
  // An explicit refresh retries failed context creation; mutation scans never spin on failures.
  if (!renderer && graphicsError) graphicsError = null;
  discover(root);
  for (const record of records.values()) {
    if (!record.canvas.isConnected || !record.canvas.matches(SELECTOR)) removeRecord(record);
    else { syncRecord(record); record.dirty = true; }
  }
  wake();
  return records.size;
}

export function setState(canvas, options = {}) {
  const allowed = new Set(['kind', 'color', 'expression', 'activity', 'autoRotate', 'paused', 'staticPose', 'wireframe', 'framing', 'yaw', 'pitch', 'lookX', 'lookY']);
  for (const key of Object.keys(options)) if (!allowed.has(key)) throw new TypeError(`Unknown sidekick state: ${key}`);
  if ('kind' in options) validKind(options.kind);
  if ('color' in options) validColor(options.color);
  if ('expression' in options && !EXPRESSIONS.includes(options.expression)) throw new RangeError('Unknown sidekick expression.');
  if ('framing' in options && !['auto', 'full', 'portrait'].includes(options.framing)) throw new RangeError('Framing must be auto, full, or portrait.');
  if ('activity' in options && typeof options.activity !== 'string') throw new TypeError('Activity must be a string.');
  for (const key of ['autoRotate', 'paused', 'staticPose', 'wireframe']) {
    if (key in options && typeof options[key] !== 'boolean') throw new TypeError(`${key} must be a boolean.`);
  }
  for (const key of ['yaw', 'pitch', 'lookX', 'lookY']) {
    if (key in options && !Number.isFinite(options[key])) throw new TypeError(`${key} must be finite.`);
  }
  const record = makeRecord(canvas);
  for (const key of ['yaw', 'pitch', 'lookX', 'lookY']) {
    if (key in options) record[key] = key === 'pitch' ? clamp(options[key], -0.6, 0.6) : key.startsWith('look') ? clamp(options[key], -1, 1) : options[key];
  }
  if (options.autoRotate === true && !stateOf(record).autoRotate) record.spinStarted = currentTime();
  for (const [key, value] of Object.entries(options)) if (!['yaw', 'pitch', 'lookX', 'lookY'].includes(key)) record.overrides[key] = value;
  record.dirty = true;
  wake();
  return getState(canvas);
}

export function getState(canvas) {
  const record = records.get(canvas);
  return record ? { ...stateOf(record), error: record.error, lastTime: record.lastTime, renderedFraming: record.renderedFraming, stats: record.modelStats } : null;
}

export function resetRotation(canvas) {
  for (const record of canvas ? [makeRecord(canvas)] : records.values()) {
    record.yaw = DEFAULT_VIEW.yaw;
    record.pitch = DEFAULT_VIEW.pitch;
    record.lookX = record.lookY = 0;
    record.overrides.autoRotate = false;
    record.dirty = true;
  }
  wake();
}

export function setPaused(value = true) {
  if (typeof value !== 'boolean') throw new TypeError('Paused must be a boolean.');
  paused = value;
  lastTick = 0;
  dirtyAll();
  return paused;
}

export function setTime(seconds = null) {
  if (seconds !== null && (!Number.isFinite(seconds) || seconds < 0)) throw new RangeError('Time must be a nonnegative number or null.');
  fixedTime = seconds;
  lastTick = 0;
  dirtyAll();
  return fixedTime;
}

// Persistent fixed time makes screenshots reproducible even after resize/mutation callbacks.
// setTime(null) returns to the live clock. Rendering itself always remains visibility-gated.
export function renderAt(seconds, { canvas } = {}) {
  refresh();
  setTime(seconds);
  cancelFrame();
  for (const record of canvas ? [makeRecord(canvas)] : records.values()) if (visible(record)) drawRecord(record, currentTime());
  return stats();
}

export async function exportGLB(kind, options = {}) {
  validKind(kind);
  const color = validColor(options.color);
  const expression = options.expression || 'bright';
  if (!EXPRESSIONS.includes(expression)) throw new RangeError('Unknown sidekick expression.');
  const seconds = options.time ?? 0;
  if (!Number.isFinite(seconds) || seconds < 0) throw new RangeError('Export time must be nonnegative.');
  // Export owns its model and is serialized: the live cache can safely update or evict meanwhile.
  const operation = exportTail.then(async () => {
    const model = createSidekick(kind, { color, quality: 'studio' });
    try {
      model.update(seconds, { expression, lookX: 0, lookY: 0, activity: options.activity || 'idle', reducedMotion: false });
      const exportScene = new THREE.Scene();
      exportScene.name = `Phoenix ${displayName(kind)}`;
      exportScene.userData = { generator: 'Phoenix procedural sidekicks', model: kind, expression, poseTime: seconds };
      exportScene.add(model.object);
      const result = await new GLTFExporter().parseAsync(exportScene, { binary: true, onlyVisible: true, trs: true });
      if (!(result instanceof ArrayBuffer)) throw new Error('The GLB exporter did not produce binary model data.');
      counts.exports++;
      return result;
    } finally { model.dispose(); }
  });
  exportTail = operation.catch(() => {});
  return operation;
}

export function stats() {
  return {
    version: '1.0.0', threeRevision: THREE.REVISION, installed,
    rendererCount: renderer ? 1 : 0, rendererCreations: counts.rendererCreations,
    trackedCanvases: records.size,
    visibleCanvases: typeof document === 'undefined' ? 0 : [...records.values()].filter(visible).length,
    cacheSize: cache.size, cacheLimit: CACHE_LIMIT, maxRenderEdge: MAX_EDGE,
    cacheHits: counts.hits, cacheMisses: counts.misses, cacheEvictions: counts.evictions,
    renderCount: counts.renders, frameCount: counts.frames, errorCount: counts.errors, exportCount: counts.exports,
    paused, fixedTime, elapsed, reducedMotion, contextLost, graphicsError,
    scheduled: !!(frameHandle || timerHandle),
    drawingBuffer: renderer ? { width: renderer.domElement.width, height: renderer.domElement.height } : null,
    gpuMemory: renderer ? { ...renderer.info.memory } : null,
    models: [...cache.values()].map(({ kind, color, quality, stats: modelStats }) => ({ kind, color, quality, ...modelStats })),
    canvases: [...records.values()].map((record) => ({ kind: stateOf(record).kind, width: record.width, height: record.height, lastTime: record.lastTime, framing: record.renderedFraming, status: record.canvas.dataset.sidekickStatus || 'pending' })),
  };
}

function releaseGraphics() {
  centering?.clear();
  for (const { model } of cache.values()) model.dispose();
  cache.clear();
  environmentTarget?.dispose();
  environmentTarget = null;
  if (scene) scene.environment = null;
  scene?.clear();
  if (renderer) {
    renderer.domElement.removeEventListener('webglcontextlost', onContextLost);
    renderer.domElement.removeEventListener('webglcontextrestored', onContextRestored);
    renderer.dispose();
    renderer.forceContextLoss();
  }
  renderer = scene = camera = mount = centering = null;
  contextLost = false;
  graphicsError = null;
}

export function dispose() {
  installed = false;
  cancelFrame();
  if (idleHandle) clearTimeout(idleHandle);
  idleHandle = 0;
  mutations?.disconnect();
  intersections?.disconnect();
  resizes?.disconnect();
  mutations = intersections = resizes = null;
  if (typeof document !== 'undefined') {
    document.removeEventListener('DOMContentLoaded', boot);
    document.removeEventListener('visibilitychange', onVisibilityChange);
    window.removeEventListener('resize', onResize);
  }
  motionQuery?.removeEventListener('change', onMotionChange);
  motionQuery = null;
  for (const record of records.values()) removeRecord(record);
  releaseGraphics();
}

export const PhoenixSidekicks = Object.freeze({
  kinds: KINDS, expressions: EXPRESSIONS, refresh, dispose, stats, exportGLB, makeRecord,
  setState, getState, setPaused, setTime, renderAt, resetRotation,
});

function boot() { refresh(); }

if (typeof window !== 'undefined' && typeof document !== 'undefined') {
  window.PhoenixSidekicks?.dispose?.();
  window.PhoenixSidekicks = PhoenixSidekicks;
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, { once: true });
  else boot();
}
