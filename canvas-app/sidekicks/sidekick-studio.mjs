const MODELS = Object.freeze([
  { kind: 'ember', name: 'Ember', color: '#e55732' },
  { kind: 'cinder', name: 'Cinder', color: '#7d8c9c' },
  { kind: 'kiln', name: 'Kiln', color: '#c49b62' },
  { kind: 'wisp', name: 'Wisp', color: '#84aa9f' },
]);

function startStudio() {
  const api = window.PhoenixSidekicks;
  const byId = (id) => document.getElementById(id);
  const status = byId('studio-status');
  const retry = byId('retry-preview');
  if (!api) {
    status.textContent = 'The 3D runtime could not load. Build the sidekick bundles and reopen the studio.';
    status.dataset.error = 'true';
    return;
  }
  const preview = byId('selected-model');
  const nativeCanvases = [...document.querySelectorAll('[data-native-size]')];
  const modelButtons = [...document.querySelectorAll('[data-select-model]')];
  const expressionButtons = [...document.querySelectorAll('[data-expression-choice]')];
  const colors = new Map(MODELS.map(({ kind, color }) => [kind, color]));
  const media = window.matchMedia('(prefers-reduced-motion: reduce)');
  const blobURLs = new Set();
  let selected = MODELS[0];
  let expression = 'bright';
  let wireframe = false;
  let lastStats = -Infinity;
  let exported = false;

  function announce(text, error = false) {
    status.textContent = text;
    status.dataset.error = String(error);
  }

  function updateDetails() {
    const state = api.getState(preview);
    const details = state?.stats;
    if (!details) return;
    byId('mesh-count').textContent = details.meshes.toLocaleString();
    byId('triangle-count').textContent = details.triangles.toLocaleString();
    byId('material-count').textContent = details.materials.toLocaleString();
    retry.hidden = !state.error;
    if (!state.error && !exported) announce(`${selected.name} is ready to inspect.`);
  }

  function selectedCanvases() { return [preview, ...nativeCanvases]; }

  function applySelection() {
    byId('selected-name').textContent = selected.name;
    byId('selected-number').textContent = String(MODELS.indexOf(selected) + 1).padStart(2, '0');
    byId('native-framing-note').textContent = selected.kind === 'wisp' ?
      'Full model at native CSS sizes.' : 'Portrait crop at 24, 32 and 48 px.';
    const color = colors.get(selected.kind);
    byId('character-color').value = color;
    byId('color-hex').value = color;
    byId('color-hex').setCustomValidity('');
    for (const button of modelButtons) button.setAttribute('aria-pressed', String(button.dataset.selectModel === selected.kind));
    for (const canvas of selectedCanvases()) {
      canvas.dataset.sidekick = selected.kind;
      canvas.dataset.color = color;
      canvas.dataset.expression = expression;
      canvas.setAttribute('aria-label', canvas === preview ?
        `${selected.name} 3D model. Drag or use arrow keys to rotate.` : `${selected.name} at ${canvas.dataset.nativeSize} pixels`);
    }
    const libraryCanvas = document.querySelector(`[data-select-model="${selected.kind}"] canvas`);
    libraryCanvas.dataset.color = color;
    api.refresh();
    for (const canvas of selectedCanvases()) api.setState(canvas, { wireframe });
    exported = false;
    lastStats = -Infinity;
    announce(`Preparing ${selected.name}…`);
  }

  function selectModel(kind) {
    const model = MODELS.find((item) => item.kind === kind);
    if (!model) throw new RangeError(`Unknown sidekick: ${kind}`);
    selected = model;
    applySelection();
    api.resetRotation(preview);
    api.setState(preview, { autoRotate: byId('auto-rotate').checked });
    return selected.kind;
  }

  function setExpression(value) {
    if (!api.expressions.includes(value)) throw new RangeError(`Unknown expression: ${value}`);
    expression = value;
    for (const button of expressionButtons) button.setAttribute('aria-pressed', String(button.dataset.expressionChoice === value));
    for (const canvas of document.querySelectorAll('canvas.phoenix-sidekick')) canvas.dataset.expression = value;
    exported = false;
    api.refresh();
    return expression;
  }

  function setColor(value) {
    if (!/^#[\da-f]{6}$/i.test(value)) throw new TypeError('Enter a six-digit hex color, such as #e55732.');
    colors.set(selected.kind, value.toLowerCase());
    applySelection();
    return colors.get(selected.kind);
  }

  function setSurface(surface) {
    if (!['light', 'dark'].includes(surface)) throw new RangeError('Choose a light or dark surface.');
    document.documentElement.dataset.theme = surface;
    for (const button of document.querySelectorAll('[data-surface]')) button.setAttribute('aria-pressed', String(button.dataset.surface === surface));
    api.refresh();
    return surface;
  }

  function updateMotion() {
    const isPaused = api.stats().paused;
    const button = byId('toggle-motion');
    button.setAttribute('aria-pressed', String(isPaused));
    button.querySelector('span').textContent = isPaused ? 'Resume' : 'Pause';
    button.title = isPaused ? 'Resume motion (Space)' : 'Pause motion (Space)';
    button.querySelector('path').setAttribute('d', isPaused ? 'm7 4 8 6-8 6Z' : 'M7 4v12 M13 4v12');
    const reduce = api.stats().reducedMotion;
    byId('auto-rotate').disabled = reduce;
    byId('motion-note').textContent = reduce ?
      'Reduced motion is on. Dragging and arrow keys still work.' : isPaused ?
        'Motion is paused. Expressions and rotation controls still work.' : 'Motion stays gentle. Space pauses it.';
  }

  function toggleMotion() { api.setPaused(!api.stats().paused); updateMotion(); }

  for (const button of modelButtons) button.addEventListener('click', () => selectModel(button.dataset.selectModel));
  for (const button of expressionButtons) button.addEventListener('click', () => setExpression(button.dataset.expressionChoice));
  for (const button of document.querySelectorAll('[data-surface]')) button.addEventListener('click', () => setSurface(button.dataset.surface));
  document.querySelector('.model-grid').addEventListener('keydown', (event) => {
    const current = modelButtons.indexOf(event.target);
    if (current < 0 || !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
    const delta = ['ArrowLeft', 'ArrowUp'].includes(event.key) ? -1 : 1;
    const next = modelButtons[(current + delta + modelButtons.length) % modelButtons.length];
    next.focus();
    selectModel(next.dataset.selectModel);
    event.preventDefault();
  });
  byId('character-color').addEventListener('input', (event) => setColor(event.target.value));
  byId('color-hex').addEventListener('input', (event) => {
    const value = event.target.value;
    if (/^#[\da-f]{6}$/i.test(value)) setColor(value);
    else event.target.setCustomValidity('Use # followed by six hexadecimal digits.');
  });
  byId('color-hex').addEventListener('change', (event) => { if (!event.target.checkValidity()) event.target.reportValidity(); });
  byId('reset-color').addEventListener('click', () => setColor(selected.color));
  byId('wireframe').addEventListener('change', (event) => {
    wireframe = event.target.checked;
    for (const canvas of selectedCanvases()) api.setState(canvas, { wireframe });
  });
  byId('auto-rotate').addEventListener('change', (event) => api.setState(preview, { autoRotate: event.target.checked }));
  byId('reset-view').addEventListener('click', () => {
    api.resetRotation(preview);
    byId('auto-rotate').checked = false;
    announce('View reset. Turntable is off.');
  });
  byId('toggle-motion').addEventListener('click', toggleMotion);
  preview.addEventListener('pointerdown', () => { byId('auto-rotate').checked = false; });
  preview.addEventListener('keydown', (event) => {
    if (event.code === 'Space') {
      toggleMotion();
      event.preventDefault();
      event.stopImmediatePropagation();
    } else if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home'].includes(event.key)) byId('auto-rotate').checked = false;
  }, true);
  preview.addEventListener('phoenix-sidekick-render', () => {
    if (performance.now() - lastStats < 300) return;
    lastStats = performance.now();
    updateDetails();
  });
  document.addEventListener('phoenix-sidekick-error', (event) => {
    announce(`${event.detail.kind}: ${event.detail.message}`, true);
    retry.hidden = false;
  });
  retry.addEventListener('click', () => { announce('Retrying the 3D preview…'); api.refresh(); });
  media.addEventListener('change', updateMotion);
  window.addEventListener('phoenix-sidekicks-motionchange', updateMotion);

  byId('export-model').addEventListener('click', async () => {
    const button = byId('export-model');
    const label = button.querySelector('span');
    const exportSelection = { ...selected, color: colors.get(selected.kind), expression };
    button.disabled = true;
    label.textContent = 'Exporting…';
    announce(`Preparing ${exportSelection.name}.glb…`);
    try {
      const buffer = await api.exportGLB(exportSelection.kind, { color: exportSelection.color, expression: exportSelection.expression });
      const url = URL.createObjectURL(new Blob([buffer], { type: 'model/gltf-binary' }));
      blobURLs.add(url);
      const link = document.createElement('a');
      link.href = url;
      link.download = `phoenix-${exportSelection.kind}-${exportSelection.expression}.glb`;
      link.hidden = true;
      document.body.append(link);
      link.click();
      link.remove();
      setTimeout(() => { URL.revokeObjectURL(url); blobURLs.delete(url); }, 30000);
      exported = true;
      announce(`${exportSelection.name}.glb exported · ${(buffer.byteLength / 1048576).toFixed(2)} MB`);
    } catch (error) {
      announce(`Export failed: ${error.message}`, true);
    } finally {
      button.disabled = false;
      label.textContent = 'Export .glb';
    }
  });

  window.addEventListener('pagehide', (event) => {
    for (const url of blobURLs) URL.revokeObjectURL(url);
    blobURLs.clear();
    if (!event.persisted) {
      media.removeEventListener('change', updateMotion);
      window.removeEventListener('phoenix-sidekicks-motionchange', updateMotion);
      api.dispose();
    }
  });
  window.PhoenixSidekickStudio = Object.freeze({
    selectModel, setExpression, setColor, setSurface,
    state: () => ({ kind: selected.kind, color: colors.get(selected.kind), expression, wireframe, surface: document.documentElement.dataset.theme, paused: api.stats().paused }),
  });
  applySelection();
  updateMotion();
}

if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', startStudio, { once: true });
else startStudio();
