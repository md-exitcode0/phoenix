// Run only in the standalone model studio. Exercises the real renderer, not a GPU mock.
export async function checkSidekickBrowser() {
  if (!location.pathname.endsWith('/sidekick-studio.html')) throw new Error('Use the isolated character studio, not the application');
  const api = window.PhoenixSidekicks, studio = window.PhoenixSidekickStudio;
  if (!api || !studio) throw new Error('The actual model studio must be loaded');
  const main = document.getElementById('selected-model');
  const checks = [], images = [], exports = [];
  const original = studio.state(), oldMotion = document.documentElement.dataset.motion;
  const assert = (name, condition, evidence = null) => {
    checks.push({ name, passed: !!condition, evidence });
    if (!condition) throw new Error(name);
  };
  const settle = async () => { await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); };
  const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
  const pixels = canvas => {
    const data = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
    let hash = 2166136261, visible = 0;
    let left = canvas.width, top = canvas.height, right = 0, bottom = 0;
    for (let i = 0; i < data.length; i += 4) {
      hash = Math.imul(hash ^ data[i], 16777619);
      hash = Math.imul(hash ^ data[i + 1], 16777619);
      hash = Math.imul(hash ^ data[i + 2], 16777619);
      hash = Math.imul(hash ^ data[i + 3], 16777619);
      if (data[i + 3] > 20) {
        const n = i / 4, x = n % canvas.width, y = Math.floor(n / canvas.width);
        visible++; left = Math.min(left, x); right = Math.max(right, x); top = Math.min(top, y); bottom = Math.max(bottom, y);
      }
    }
    return { hash: (hash >>> 0).toString(16), visible, width: canvas.width, height: canvas.height, bounds: { left, top, right, bottom } };
  };
  try {
    api.setPaused(true);
    for (const kind of api.kinds) {
      studio.selectModel(kind); studio.setExpression('bright'); await settle();
      api.setState(main, { yaw: 0.22, pitch: 0.06, autoRotate: false, wireframe: false }); api.renderAt(0);
      const front = pixels(main);
      assert(`${kind}: actual nonempty front render`, front.visible > 1000 && main.dataset.sidekickStatus === 'ready', front);
      assert(`${kind}: whole silhouette fits`, front.bounds.top > 2 && front.bounds.bottom < front.height - 3 && front.bounds.left > 2 && front.bounds.right < front.width - 3);
      api.setState(main, { yaw: Math.PI + 0.22 }); api.renderAt(0);
      const rear = pixels(main);
      assert(`${kind}: rear is different geometry`, rear.visible > 1000 && rear.hash !== front.hash, rear);
      api.setState(main, { yaw: 0.22, wireframe: true }); api.renderAt(0);
      const wire = pixels(main);
      assert(`${kind}: wireframe changes actual rendered pixels`, wire.visible > 100 && wire.hash !== front.hash);
      api.setState(main, { wireframe: false }); studio.setExpression('sleepy'); await settle(); api.renderAt(0);
      const sleepy = pixels(main);
      assert(`${kind}: expression changes the rendered face`, sleepy.hash !== front.hash);
      studio.setExpression('bright'); await settle(); api.renderAt(4.96);
      assert(`${kind}: animation changes a real frame`, pixels(main).hash !== front.hash);
      api.renderAt(0);
      for (const canvas of document.querySelectorAll('[data-native-size]')) {
        const shot = pixels(canvas), size = Number(canvas.dataset.nativeSize);
        assert(`${kind}: ${size}px avatar is rendered`, shot.visible > 20 && canvas.dataset.sidekickStatus === 'ready');
        const framing = api.getState(canvas).renderedFraming;
        assert(`${kind}: ${size}px framing`, framing === (size <= 64 && kind !== 'wisp' ? 'portrait' : 'full'), framing);
      }
      const binary = await api.exportGLB(kind, { expression: 'curious' }), view = new DataView(binary);
      const jsonLength = view.getUint32(12, true);
      const json = JSON.parse(new TextDecoder().decode(new Uint8Array(binary, 20, jsonLength)));
      const exported = { kind, bytes: binary.byteLength, meshes: json.meshes.length, nodes: json.nodes.length,
        materials: json.materials.length, images: json.images?.length || 0, textures: json.textures?.length || 0 };
      assert(`${kind}: real texture-free GLB export`, view.getUint32(0, true) === 0x46546c67 && view.getUint32(4, true) === 2 &&
        view.getUint32(8, true) === binary.byteLength && exported.meshes > 5 && exported.images === 0 && exported.textures === 0, exported);
      exports.push(exported); images.push({ kind, front, rear, wire, sleepy });
    }
    assert('all previews share exactly one renderer', api.stats().rendererCount === 1 && api.stats().rendererCreations === 1);
    const beforeInvalid = api.getState(main);
    let rejected = false;
    try { api.setState(main, { yaw: 3, paused: 'not boolean' }); } catch { rejected = true; }
    assert('invalid API update cannot partially mutate state', rejected && JSON.stringify(beforeInvalid) === JSON.stringify(api.getState(main)));

    api.setTime(null); api.setPaused(false); document.documentElement.dataset.motion = 'minimal'; await wait(160);
    assert('Phoenix Minimal motion is honored', api.stats().reducedMotion === true);
    const motionRenders = api.stats().renderCount; await wait(250);
    assert('reduced motion has no continuous render loop', api.stats().renderCount === motionRenders);
    if (oldMotion === undefined) delete document.documentElement.dataset.motion; else document.documentElement.dataset.motion = oldMotion;
    await settle(); api.setPaused(true); api.renderAt(0); await wait(80);
    const stillRenders = api.stats().renderCount; await wait(200);
    assert('paused studio reaches idle', api.stats().renderCount === stillRenders);

    const visibility = document.body.style.visibility;
    document.body.style.visibility = 'hidden'; await wait(80);
    const hiddenRenders = api.stats().renderCount; await wait(200);
    assert('invisible models do not render', api.stats().renderCount === hiddenRenders);
    document.body.style.visibility = visibility; await settle();
    api.dispose();
    assert('dispose releases model cache, renderer and observers', api.stats().rendererCount === 0 && api.stats().cacheSize === 0 && api.stats().trackedCanvases === 0 && !api.stats().scheduled);
    api.refresh(); await settle(); api.renderAt(0);
    assert('renderer can be reinitialized after disposal', api.stats().rendererCount === 1 && api.stats().trackedCanvases === 9 && main.dataset.sidekickStatus === 'ready');
    assert('no unexpected renderer errors', api.stats().errorCount === 0, api.stats().graphicsError);
    return window.__sidekickVerification = { recordedUtc: new Date().toISOString(), passed: checks.length, failed: 0,
      renderer: 'actual Chrome WebGL', checks, images, exports, stats: api.stats() };
  } finally {
    document.body.style.visibility = '';
    if (oldMotion === undefined) delete document.documentElement.dataset.motion; else document.documentElement.dataset.motion = oldMotion;
    studio.selectModel(original.kind); studio.setExpression(original.expression); studio.setSurface(original.surface);
    api.setTime(null); api.setPaused(original.paused); api.resetRotation(main);
    api.setState(main, { autoRotate: false, wireframe: original.wireframe });
  }
}
