/** Transparent Blender sprite playback. No external runtime or global styles. */
export const COMPANION_STATES = Object.freeze(['idle', 'browsing', 'coding', 'waiting', 'success', 'error']);
const caches = new WeakMap();

function validateManifest(manifest, win) {
  if (!manifest?.id || !manifest.variants?.standard) throw new TypeError('A character manifest is required');
  const clips = new Map();
  for (const [variant, states] of Object.entries(manifest.variants)) {
    for (const state of COMPANION_STATES) {
      const clip = states[state];
      if (!clip || !Number.isFinite(clip.duration) || clip.duration <= 0 || !clip.frames?.length || !Number.isInteger(clip.columns) || clip.columns < 1 || !Number.isInteger(clip.cellSize) || clip.cellSize < 1) throw new TypeError(`Invalid ${variant}/${state} clip`);
      const url = new URL(clip.url, win.location.href);
      if (url.origin !== win.location.origin) throw new TypeError('Companion assets must have the same origin as the app');
      let previous = -1;
      for (const frame of clip.frames) {
        if (!Number.isFinite(frame.at) || frame.at < 0 || frame.at <= previous || frame.at > clip.duration || !Number.isInteger(frame.cell) || frame.cell < 0) throw new TypeError(`Invalid frame in ${variant}/${state}`);
        previous = frame.at;
      }
      if (clip.frames[0].at !== 0 || typeof clip.loop !== 'boolean') throw new TypeError('Clips must start at zero and declare loop behavior');
      clips.set(`${variant}/${state}`, Object.freeze({ ...clip, url: url.href, frames: clip.frames.map(f => Object.freeze({ ...f })) }));
    }
  }
  return { id: String(manifest.id), label: String(manifest.label ?? manifest.id), clips };
}
function acquire(win, url) {
  let cache = caches.get(win);
  if (!cache) { cache = new Map(); caches.set(win, cache); }
  let entry = cache.get(url);
  if (!entry) {
    const image = new win.Image();
    entry = { image, refs: 0 };
    entry.promise = new Promise((resolve, reject) => {
      image.onload = () => { image.onload = image.onerror = null; resolve(image); };
      image.onerror = () => { image.onload = image.onerror = null; reject(new Error('Companion asset failed to load')); };
    });
    cache.set(url, entry); image.src = url;
  }
  entry.refs++;
  let released = false;
  return { promise: entry.promise, release() {
    if (released) return; released = true;
    if (--entry.refs === 0) { cache.delete(url); entry.image.src = ''; }
  } };
}
const checked = (value, values, name) => { if (!values.includes(value)) throw new RangeError(`Invalid companion ${name}`); return value; };
const checkedSize = value => { if (!Number.isFinite(value) || value < 16 || value > 256) throw new RangeError('Companion size must be 16–256 CSS pixels'); return value; };

/** Create a detached companion. Its manifest contains only actual rendered poses. */
export function renderCompanion(options = {}) {
  const doc = options.document ?? globalThis.document;
  if (!doc?.defaultView) throw new TypeError('Companion requires a browser Document');
  const win = doc.defaultView;
  let character = validateManifest(options.character, win);
  let state = checked(options.state ?? 'idle', COMPANION_STATES, 'state');
  let size = checkedSize(options.size ?? 80);
  let motion = checked(options.motion ?? 'auto', ['auto', 'reduce'], 'motion');
  const decorative = options.decorative !== false;
  const element = doc.createElement('span');
  element.dataset.phoenixCompanion = '';
  element.style.cssText = 'display:inline-block;flex:0 0 auto;vertical-align:middle;line-height:0;pointer-events:none;contain:layout paint;';
  if (decorative) { element.setAttribute('aria-hidden', 'true'); element.setAttribute('inert', ''); }
  else element.setAttribute('role', 'img');
  const shadow = element.attachShadow({ mode: 'open' });
  const style = doc.createElement('style');
  style.textContent = ':host{overflow:hidden}canvas{display:block;width:100%;height:100%;pointer-events:none}';
  const canvas = doc.createElement('canvas'); canvas.setAttribute('aria-hidden', 'true');
  shadow.append(style, canvas);
  const context = canvas.getContext('2d');
  if (!context) throw new Error('Canvas 2D is unavailable');
  const media = win.matchMedia?.('(prefers-reduced-motion: reduce)');
  let destroyed = false, paused = false, visible = !win.IntersectionObserver;
  let elapsed = 0, lastTime = null, raf = 0, generation = 0, lease, image, clip, painted = -1;
  let ready = Promise.resolve();
  const alive = () => { if (destroyed) throw new Error('Companion has been destroyed'); };
  const reduced = () => motion === 'reduce' || !!media?.matches;
  const canRun = () => !destroyed && !paused && visible && element.isConnected && !doc.hidden && !reduced() && image && (clip.loop || elapsed < clip.duration);
  function stop() { if (raf) win.cancelAnimationFrame(raf); raf = 0; lastTime = null; }
  function layout() {
    element.style.width = element.style.height = `${size}px`;
    const pixels = Math.round(size * Math.min(win.devicePixelRatio || 1, 2));
    if (canvas.width !== pixels || canvas.height !== pixels) canvas.width = canvas.height = pixels;
    painted = -1;
    element.dataset.character = character.id; element.dataset.state = state;
    element.dataset.detail = size <= 40 && character.clips.has(`compact/${state}`) ? 'compact' : 'standard';
    element.dataset.motion = reduced() ? 'reduce' : 'auto';
    if (!decorative) element.setAttribute('aria-label', `${options.label ?? character.label}: ${state}`);
  }
  function paint() {
    if (!image || destroyed) return;
    const time = reduced() ? (clip.loop ? 0 : clip.duration) : clip.loop ? elapsed % clip.duration : Math.min(elapsed, clip.duration);
    let low = 0, high = clip.frames.length - 1;
    while (low < high) { const mid = Math.ceil((low + high)/2); if (clip.frames[mid].at <= time) low = mid; else high = mid - 1; }
    const cell = clip.frames[low].cell;
    if (cell === painted) return;
    context.clearRect(0, 0, canvas.width, canvas.height);
    const stride = clip.cellSize + 2 * (clip.gutter ?? 0);
    const sx = (cell % clip.columns) * stride + (clip.gutter ?? 0);
    const sy = Math.floor(cell/clip.columns) * stride + (clip.gutter ?? 0);
    context.drawImage(image, sx, sy, clip.cellSize, clip.cellSize, 0, 0, canvas.width, canvas.height);
    painted = cell; element.dataset.frame = String(cell);
  }
  function tick(now) {
    raf = 0;
    if (!canRun()) { lastTime = null; return; }
    if (lastTime !== null) elapsed += Math.min(now - lastTime, 100);
    lastTime = now; paint();
    if (canRun()) raf = win.requestAnimationFrame(tick); else lastTime = null;
  }
  function sync() {
    stop(); layout();
    if (reduced() && clip && !clip.loop) elapsed = clip.duration;
    paint(); if (canRun()) raf = win.requestAnimationFrame(tick);
  }
  function load() {
    stop(); const token = ++generation;
    lease?.release(); lease = null; image = null; painted = -1;
    clip = character.clips.get(`${size <= 40 && character.clips.has(`compact/${state}`) ? 'compact' : 'standard'}/${state}`);
    layout(); delete element.dataset.assetError;
    lease = acquire(win, clip.url);
    ready = lease.promise.then(decoded => {
      if (destroyed || token !== generation) return;
      const stride = clip.cellSize + 2 * (clip.gutter ?? 0);
      const maxCell = Math.max(...clip.frames.map(f => f.cell));
      if (decoded.width < stride * clip.columns || decoded.height < stride * (Math.floor(maxCell/clip.columns) + 1)) throw new Error('Companion atlas dimensions do not match its manifest');
      image = decoded; sync();
    }).catch(error => {
      if (destroyed || token !== generation) return;
      element.dataset.assetError = 'true'; stop(); lease?.release(); lease = null;
      options.onError?.(error); throw error;
    });
    // The public ready promise preserves rejection; observing it avoids global noise.
    ready.catch(() => {});
  }
  const observer = win.IntersectionObserver ? new win.IntersectionObserver(entries => {
    visible = entries[entries.length-1]?.isIntersecting ?? false; sync();
  }) : null;
  observer?.observe(element);
  const visibility = () => sync();
  doc.addEventListener('visibilitychange', visibility);
  media?.addEventListener?.('change', visibility);
  const controller = Object.freeze({
    element, get ready() { return ready; }, get state() { return state; }, get size() { return size; }, get motion() { return motion; }, get character() { return character.id; },
    setState(next, { restart = false } = {}) {
      alive(); checked(next, COMPANION_STATES, 'state');
      if (next === state && !restart) return controller;
      state = next; elapsed = 0; load(); return controller;
    },
    setCharacter(next) { alive(); const validated = validateManifest(next, win); character = validated; elapsed = 0; load(); return controller; },
    setSize(next) { alive(); checkedSize(next); const old = size <= 40; size = next; if (old !== (size <= 40)) load(); else sync(); return controller; },
    setMotion(next) { alive(); motion = checked(next, ['auto', 'reduce'], 'motion'); sync(); return controller; },
    setPaused(next) { alive(); paused = !!next; sync(); return controller; },
    refresh() { alive(); sync(); return controller; },
    destroy() {
      if (destroyed) return;
      destroyed = true; generation++; stop(); observer?.disconnect();
      doc.removeEventListener('visibilitychange', visibility); media?.removeEventListener?.('change', visibility);
      lease?.release(); lease = null; image = null;
      context.clearRect(0,0,canvas.width,canvas.height);canvas.width=canvas.height=0;element.remove();
    },
  });
  load(); return controller;
}

export function mountCompanion(container, options = {}) {
  if (!container?.append || !container.ownerDocument) throw new TypeError('Companion requires a DOM container');
  const companion = renderCompanion({ ...options, document: container.ownerDocument });
  container.append(companion.element); companion.refresh(); return companion;
}
