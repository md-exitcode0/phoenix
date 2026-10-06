import test from 'node:test';
import assert from 'node:assert/strict';
import * as THREE from 'three';
import { createSidekick, SIDEKICKS, EXPRESSIONS } from './models.mjs';

for (const kind of SIDEKICKS) for (const quality of ['avatar', 'studio']) {
  test(`${kind}/${quality}: real finite geometry, materials and a complete silhouette`, () => {
    const model = createSidekick(kind, { quality });
    try {
      assert.equal(model.object.userData.procedural, true);
      assert.ok(model.stats.meshes > 10);
      assert.ok(model.stats.triangles > 1000 && model.stats.triangles < 250_000, JSON.stringify(model.stats));
      assert.equal(model.stats.textures, 0);
      model.object.traverse(node => {
        if (!node.isMesh) return;
        assert.ok(node.geometry.isBufferGeometry);
        const positions = node.geometry.getAttribute('position');
        assert.ok(positions.count > 0);
        for (const value of positions.array) assert.ok(Number.isFinite(value), `${node.name}: invalid position`);
        for (const value of node.geometry.getAttribute('normal').array) assert.ok(Number.isFinite(value), `${node.name}: invalid normal`);
        for (const [name, value] of Object.entries(node.material)) assert.ok(!value?.isTexture, `${node.name}: unexpected ${name}`);
      });
      const box = new THREE.Box3().setFromObject(model.object), size = box.getSize(new THREE.Vector3());
      assert.ok(size.x > 1 && size.x < 4, `${kind}: width ${size.x}`);
      assert.ok(size.y > 1.5 && size.y < 4, `${kind}: height ${size.y}`);
      assert.ok(size.z > 0.5 && size.z < 4, `${kind}: depth ${size.z}`);
      assert.ok(box.min.y > -0.15, `${kind}: below floor ${box.min.y}`);
    } finally { model.dispose(); }
  });
}

test('characters have materially different mesh structures, not four recolors', () => {
  const names = SIDEKICKS.map(kind => {
    const model = createSidekick(kind);
    try {
      const result = []; model.object.traverse(node => result.push(node.name));
      return result;
    } finally { model.dispose(); }
  });
  assert.ok(names[0].includes('tail_fan') && names[0].includes('wing_left'));
  assert.ok(names[1].includes('curled_tail') && names[1].includes('outer_ear'));
  assert.ok(names[2].includes('open_helmet_shell') && names[2].includes('spherical_visor'));
  assert.ok(names[3].includes('continuous_cloud_surface'));
  assert.equal(new Set(names.map(value => JSON.stringify(value))).size, 4);
});

test('expression, gaze, motion and reduced motion update rig geometry rather than images', () => {
  for (const kind of SIDEKICKS) {
    const model = createSidekick(kind);
    try {
      const allTransforms = () => {
        const values = [];
        model.object.traverse(node => values.push(...node.position.toArray(), ...node.quaternion.toArray(), ...node.scale.toArray()));
        return values;
      };
      model.update(0); const initial = allTransforms();
      model.update(2); assert.notDeepEqual(allTransforms(), initial);
      for (const expression of EXPRESSIONS) for (const activity of ['idle', 'thinking', 'working', 'celebrate']) {
        model.update(7.2, { expression, activity, lookX: 0.8, lookY: -0.5 });
        assert.ok(allTransforms().every(Number.isFinite));
      }
      model.update(1, { reducedMotion: true }); const stopped = allTransforms();
      model.update(9000, { reducedMotion: true }); assert.deepEqual(allTransforms(), stopped);
      model.update(NaN, { lookX: Infinity, lookY: NaN }); assert.ok(allTransforms().every(Number.isFinite));
    } finally { model.dispose(); }
  }
});

test('each model releases owned geometry and materials exactly once', () => {
  for (const kind of SIDEKICKS) {
    const model = createSidekick(kind), geometry = new Set(), materials = new Set();
    model.object.traverse(node => { if (node.isMesh) { geometry.add(node.geometry); materials.add(node.material); } });
    let count = 0;
    for (const resource of [...geometry, ...materials]) resource.addEventListener('dispose', () => count++);
    model.dispose(); model.dispose(); model.update(5);
    assert.equal(count, geometry.size + materials.size);
  }
});

test('invalid character, color and quality are rejected without producing a model', () => {
  assert.throws(() => createSidekick('website'), RangeError);
  assert.throws(() => createSidekick('ember', { color: 'url(external.png)' }), TypeError);
  assert.throws(() => createSidekick('ember', { quality: 'unbounded' }), RangeError);
});
