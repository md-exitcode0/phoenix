// Export actual geometry for use in Blender, a glTF viewer or another app.
// Procedural animation remains in models.mjs; these GLBs contain a posed model.
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import * as THREE from 'three';
import { GLTFExporter } from 'three/addons/exporters/GLTFExporter.js';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { createSidekick, SIDEKICKS } from './models.mjs';

const destination = resolve(process.argv[2] || 'exports');
await mkdir(destination, { recursive: true });

// Three's exporter uses the browser FileReader API to finalize its Blob. The
// exporter does not read images here: all character materials are texture-free.
if (!globalThis.FileReader) globalThis.FileReader = class {
  readAsArrayBuffer(blob) {
    blob.arrayBuffer().then(result => {
      this.result = result;
      this.onload?.({ target: this });
      this.onloadend?.({ target: this });
    }, error => { this.error = error; this.onerror?.({ target: this }); });
  }
};

const records = [];
for (const kind of SIDEKICKS) {
  const model = createSidekick(kind);
  let imported;
  try {
    model.update(0, { expression: 'bright', reducedMotion: true });
    const scene = new THREE.Scene(); scene.name = `Phoenix ${kind}`; scene.add(model.object);
    const buffer = await new GLTFExporter().parseAsync(scene, { binary: true, trs: true });
    if (!(buffer instanceof ArrayBuffer)) throw new Error('Expected binary glTF output');
    const header = new DataView(buffer);
    if (header.getUint32(0, true) !== 0x46546c67 || header.getUint32(4, true) !== 2 ||
        header.getUint32(8, true) !== buffer.byteLength) throw new Error('Invalid GLB header');
    imported = await new GLTFLoader().parseAsync(buffer, '');
    let meshes = 0, triangles = 0;
    imported.scene.traverse(node => {
      if (!node.isMesh) return;
      meshes++;
      const position = node.geometry.getAttribute('position');
      if (!position?.count || !position.array.every(Number.isFinite)) throw new Error(`Invalid imported geometry: ${node.name}`);
      triangles += (node.geometry.index?.count ?? position.count) / 3;
    });
    if (meshes !== model.stats.meshes || triangles !== model.stats.triangles) throw new Error('Export changed the character geometry');
    const filename = `phoenix-${kind}.glb`;
    await writeFile(join(destination, filename), new Uint8Array(buffer), { flag: 'wx' });
    records.push({ kind, filename, bytes: buffer.byteLength, meshes, triangles,
      roundtrip: true, bakedAnimationClips: false, proceduralAnimationSource: 'models.mjs' });
  } finally {
    const geometries = new Set(), materials = new Set();
    imported?.scene.traverse(node => {
      if (node.isMesh) { geometries.add(node.geometry); for (const m of Array.isArray(node.material) ? node.material : [node.material]) materials.add(m); }
    });
    for (const geometry of geometries) geometry.dispose();
    for (const material of materials) material.dispose();
    model.dispose();
  }
}
await writeFile(join(destination, 'manifest.json'), JSON.stringify(records, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ destination, models: records }, null, 2));
