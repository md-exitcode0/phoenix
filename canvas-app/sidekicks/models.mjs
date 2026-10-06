/**
 * Phoenix sidekicks. Every visible part is generated from geometry below.
 * No image sprites, downloaded character meshes, bitmap faces or baked renders.
 * +Y is up, +Z is the face. The returned rig is independent and disposable.
 */
import * as THREE from 'three';
import { MarchingCubes } from 'three/addons/objects/MarchingCubes.js';

export const SIDEKICKS = Object.freeze(['ember', 'cinder', 'kiln', 'wisp']);
export const EXPRESSIONS = Object.freeze(['bright', 'joy', 'calm', 'curious', 'mischief', 'sleepy']);
const TAU = Math.PI * 2;
const V = (x, y, z) => new THREE.Vector3(x, y, z);
const mix = (a, b, t) => new THREE.Color(a).lerp(new THREE.Color(b), t);

class Kit {
  constructor(kind, quality) {
    this.object = new THREE.Group();
    this.object.name = `Phoenix_${kind}`;
    this.object.userData = { character: kind, procedural: true, source: 'models.mjs', version: 1 };
    this.geometries = new Set(); this.materials = new Set(); this.cache = new Map();
    this.segments = quality === 'avatar' ? 20 : 36;
    this.eyeRigs = []; this.mouths = []; this.moving = [];
    this.kind = kind; this.disposed = false;
  }
  geometry(key, create) {
    if (!this.cache.has(key)) { const g = create(); this.geometries.add(g); this.cache.set(key, g); }
    return this.cache.get(key);
  }
  material(name, color, properties = {}) {
    const m = new THREE.MeshPhysicalMaterial({ color, roughness: 0.34, metalness: 0.03,
      clearcoat: 0.35, clearcoatRoughness: 0.25, ...properties });
    m.name = name; this.materials.add(m); return m;
  }
  group(name, parent = this.object, position = [0, 0, 0]) {
    const g = new THREE.Group(); g.name = name; g.position.set(...position); parent.add(g); return g;
  }
  mesh(name, geometry, material, parent, position = [0, 0, 0], scale = [1, 1, 1], rotation = [0, 0, 0]) {
    this.geometries.add(geometry);
    const mesh = new THREE.Mesh(geometry, material); mesh.name = name;
    mesh.position.set(...position); mesh.scale.set(...scale); mesh.rotation.set(...rotation);
    mesh.castShadow = true; mesh.receiveShadow = true; (parent || this.object).add(mesh); return mesh;
  }
  ball(name, mat, parent, position, scale, rotation) {
    const g = this.geometry('sphere', () => new THREE.SphereGeometry(1, this.segments, Math.round(this.segments * 0.7)));
    return this.mesh(name, g, mat, parent, position, scale, rotation);
  }
  ring(name, mat, parent, radius, thickness, position, rotation = [0, 0, 0], scale = [1, 1, 1]) {
    const key = `torus:${radius}:${thickness}`;
    return this.mesh(name, this.geometry(key, () => new THREE.TorusGeometry(radius, thickness, 10, this.segments * 2)),
      mat, parent, position, scale, rotation);
  }
  cylinder(name, mat, parent, top, bottom, height, position, rotation = [0, 0, 0]) {
    return this.mesh(name, new THREE.CylinderGeometry(top, bottom, height, this.segments, 1), mat, parent, position, [1, 1, 1], rotation);
  }
  line(name, mat, parent, points, thickness = 0.015) {
    const curve = new THREE.CatmullRomCurve3(points.map(p => V(...p)));
    return this.mesh(name, new THREE.TubeGeometry(curve, 22, thickness, 6, false), mat, parent);
  }
  /** Elliptic swept volume: a sculpted feather, ear, tail or flame, not a flat cutout. */
  sweep(name, mat, parent, points, widths, depths, colors = null) {
    const curve = new THREE.CatmullRomCurve3(points.map(p => V(...p)));
    const longitudinal = this.segments, radial = 12;
    const positions = [], indices = [], vertexColors = [];
    const interpolate = (values, t) => {
      const u = t * (values.length - 1), i = Math.min(values.length - 2, Math.floor(u));
      return THREE.MathUtils.lerp(values[i], values[i + 1], u - i);
    };
    for (let i = 0; i <= longitudinal; i++) {
      const t = i / longitudinal, center = curve.getPoint(t), tangent = curve.getTangent(t).normalize();
      const reference = Math.abs(tangent.z) > 0.94 ? V(0, 1, 0) : V(0, 0, 1);
      const across = new THREE.Vector3().crossVectors(reference, tangent).normalize();
      const normal = new THREE.Vector3().crossVectors(tangent, across).normalize();
      const width = Math.max(0.001, interpolate(widths, t)), depth = Math.max(0.001, interpolate(depths, t));
      for (let j = 0; j <= radial; j++) {
        const angle = j / radial * TAU;
        const p = center.clone().addScaledVector(across, width * Math.cos(angle))
          .addScaledVector(normal, depth * Math.sin(angle));
        positions.push(p.x, p.y, p.z);
        if (colors) {
          const u = t * (colors.length - 1), k = Math.min(colors.length - 2, Math.floor(u));
          const c = mix(colors[k], colors[k + 1], u - k);
          // A shallow central feather highlight, carried by the actual vertex colors.
          c.multiplyScalar(0.91 + 0.09 * Math.max(0, Math.sin(angle)));
          vertexColors.push(c.r, c.g, c.b);
        }
        if (i < longitudinal && j < radial) {
          const a = i * (radial + 1) + j, b = a + 1, c = a + radial + 1, d = c + 1;
          indices.push(a, b, c, b, d, c);
        }
      }
    }
    const g = new THREE.BufferGeometry();
    g.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
    if (colors) g.setAttribute('color', new THREE.Float32BufferAttribute(vertexColors, 3));
    g.setIndex(indices); g.computeVertexNormals();
    return this.mesh(name, g, mat, parent);
  }
  blinkable(name, parent, position) {
    const g = this.group(name, parent, position); this.eyeRigs.push(g); return g;
  }
  articulated(group, axis, amount, phase = 0) {
    this.moving.push({ group, axis, amount, phase, rest: group.rotation[axis] }); return group;
  }
  dispose() {
    if (this.disposed) return;
    for (const g of this.geometries) g.dispose();
    for (const m of this.materials) m.dispose();
    this.object.removeFromParent(); this.disposed = true;
  }
}

function eye(kit, head, x, y, z, { size = 1, glow = false, accent = '#ffc476', tilt = 0 } = {}) {
  const group = kit.blinkable(`eye_${x < 0 ? 'left' : 'right'}`, head, [x, y, z]);
  group.rotation.z = tilt;
  const socket = kit.material('Eye / polished charcoal', '#201919', { roughness: 0.18, clearcoat: 1, clearcoatRoughness: 0.08 });
  const iris = kit.material('Eye / amber iris', glow ? accent : '#5c3020', {
    roughness: 0.18, clearcoat: 1, emissive: glow ? accent : '#000000', emissiveIntensity: glow ? 0.48 : 0,
  });
  const black = kit.material('Eye / pupil', '#100e12', { roughness: 0.12, clearcoat: 1 });
  const white = kit.material('Eye / catchlight', '#fff8e8', { roughness: 0.12, emissive: '#fff4d9', emissiveIntensity: 0.24 });
  kit.ball('eye_socket', socket, group, [0, 0, 0], [0.19 * size, 0.226 * size, 0.094 * size]);
  kit.ball('amber_lens', iris, group, [0, -0.012 * size, 0.060 * size], [0.151 * size, 0.180 * size, 0.05 * size]);
  kit.ball('pupil', black, group, [0.007 * size, -0.008 * size, 0.1 * size],
    [(glow ? 0.039 : 0.105) * size, 0.142 * size, 0.022 * size]);
  kit.ball('large_catchlight', white, group, [-0.050 * size, 0.075 * size, 0.109 * size], [0.048 * size, 0.057 * size, 0.018 * size]);
  kit.ball('small_catchlight', white, group, [0.049 * size, -0.065 * size, 0.109 * size], [0.017 * size, 0.02 * size, 0.012 * size]);
  return group;
}

function makeEmber(kit, accent) {
  const bodyColor = mix('#ef732c', accent, 0.20);
  const orange = kit.material('Ember / tangerine enamel', bodyColor, { roughness: 0.40, clearcoat: 0.30 });
  const gold = kit.material('Ember / golden down', '#ffbd5b', { roughness: 0.47, clearcoat: 0.18 });
  const beak = kit.material('Ember / warm beak', '#d77929', { roughness: 0.29, clearcoat: 0.18 });
  const feet = kit.material('Ember / feet', '#b65329', { roughness: 0.47 });
  const dark = kit.material('Ember / beak seam', '#682c22', { roughness: 0.6 });
  const plumage = kit.material('Ember / flame feathers', '#ffffff', { vertexColors: true, roughness: 0.34, clearcoat: 0.34 });
  const fire = ['#b72e27', bodyColor, '#ffbb50'];
  const sunlight = ['#e15c29', '#ffab43', '#ffe092'];
  const body = kit.group('body');
  // A taller, tapered torso keeps Ember avian and elegant instead of reading as
  // a round mascot. The breast and ruff overlap the body as modeled plumage.
  kit.ball('pear_shaped_body', orange, body, [0, 1.03, -0.015], [0.48, 0.77, 0.40]);
  kit.ball('golden_breast', gold, body, [0, 1.08, 0.29], [0.31, 0.54, 0.17]);
  for (let i = 0; i < 7; i++) {
    const x = (i - 3) * 0.075;
    kit.sweep(`breast_feather_${i}`, plumage, body,
      [[x, 1.48, 0.41], [x * 1.18, 1.26, 0.48], [x * 1.36, 0.97, 0.44], [x * 1.55, 0.74, 0.32]],
      [0.018, 0.061, 0.052, 0.001], [0.016, 0.031, 0.024, 0.001], sunlight);
  }
  const ruff = kit.group('neck_ruff', body, [0, 1.52, 0.02]);
  for (const s of [-1, 1]) {
    for (let i = 0; i < 3; i++) {
      const y = 0.14 - i * 0.12;
      kit.sweep(`ruff_${s < 0 ? 'left' : 'right'}_${i}`, plumage, ruff,
        [[s * 0.10, y, 0.22], [s * 0.30, y - 0.02, 0.27], [s * (0.48 + i * 0.035), y - 0.08, 0.16], [s * (0.60 + i * 0.04), y - 0.02, 0.05]],
        [0.035, 0.075, 0.055, 0.001], [0.023, 0.035, 0.022, 0.001], i === 1 ? sunlight : fire);
    }
  }
  const tail = kit.group('tail_fan', body, [0, 0.72, -0.22]);
  for (let i = 0; i < 9; i++) {
    const f = (i - 4) / 4;
    kit.sweep(`tail_plume_${i}`, plumage, tail,
      [[f * 0.06, 0.02, 0], [f * 0.34, -0.08, -0.42], [f * 0.76, 0.02 + (1 - Math.abs(f)) * 0.06, -0.81],
        [f * 1.10, 0.52 + Math.abs(f) * 0.18, -1.05]],
      [0.030, 0.090, 0.105, 0.001], [0.021, 0.034, 0.028, 0.001], i % 2 ? sunlight : fire);
  }
  kit.articulated(tail, 'y', 0.035, 0.7);
  for (const s of [-1, 1]) {
    const wing = kit.group(`wing_${s < 0 ? 'left' : 'right'}`, body, [s * 0.40, 1.38, -0.015]);
    kit.ball('wing_shoulder', orange, wing, [s * 0.035, -0.04, 0], [0.17, 0.34, 0.20], [0, 0, s * 0.22]);
    kit.sweep('wing_covert', plumage, wing,
      [[s * 0.02, 0.15, 0.13], [s * 0.26, 0.10, 0.17], [s * 0.43, -0.04, 0.10], [s * 0.52, -0.18, 0.02]],
      [0.055, 0.12, 0.10, 0.001], [0.035, 0.060, 0.035, 0.001], sunlight);
    for (let i = 0; i < 7; i++) {
      kit.sweep(`flight_feather_${i}`, plumage, wing,
        [[s * 0.015, 0.06 - i * 0.055, 0.10], [s * 0.27, -0.03 - i * 0.055, 0.14],
          [s * (0.48 + i * 0.006), -0.15 - i * 0.070, 0.06], [s * (0.68 - i * 0.018), -0.23 - i * 0.115, -0.02]],
        [0.045, 0.088, 0.068, 0.001], [0.029, 0.047, 0.030, 0.001], i % 2 ? sunlight : fire);
    }
    kit.articulated(wing, 'z', s * 0.035, 0.3);
    kit.cylinder('ankle', feet, body, 0.052, 0.064, 0.31, [s * 0.205, 0.28, 0.055]);
    kit.ball('foot_pad', feet, body, [s * 0.205, 0.105, 0.13], [0.145, 0.075, 0.15]);
    for (let t = -1; t <= 1; t++) {
      kit.sweep('talon', beak, body,
        [[s * 0.205 + t * 0.070, 0.105, 0.20], [s * 0.205 + t * 0.080, 0.085, 0.29], [s * 0.205 + t * 0.075, 0.065, 0.36]],
        [0.028, 0.034, 0.001], [0.025, 0.026, 0.001]);
    }
  }
  const head = kit.group('head', body, [0, 1.90, 0.04]); kit.head = head;
  kit.ball('phoenix_head', orange, head, [0, 0.02, 0], [0.53, 0.58, 0.47]);
  kit.ball('brow_mask', gold, head, [0, 0.01, 0.365], [0.39, 0.27, 0.12]);
  for (const s of [-1, 1]) {
    eye(kit, head, s * 0.215, 0.055, 0.455, { size: 0.72, tilt: -s * 0.11 });
    kit.sweep('brow_feather', plumage, head,
      [[s * 0.08, 0.19, 0.43], [s * 0.24, 0.24, 0.46], [s * 0.41, 0.22, 0.36], [s * 0.55, 0.16, 0.18]],
      [0.028, 0.065, 0.050, 0.001], [0.022, 0.030, 0.020, 0.001], sunlight);
    kit.sweep('cheek_plume', plumage, head,
      [[s * 0.31, -0.12, 0.34], [s * 0.48, -0.16, 0.31], [s * 0.61, -0.08, 0.20], [s * 0.68, 0.08, 0.08]],
      [0.055, 0.095, 0.065, 0.001], [0.045, 0.042, 0.020, 0.001], fire);
  }
  const crown = kit.group('crown', head);
  const crests = [
    [[-0.36, 0.30, -0.08], [-0.46, 0.48, -0.13], [-0.47, 0.66, -0.20], [-0.42, 0.82, -0.28]],
    [[-0.18, 0.45, -0.10], [-0.24, 0.66, -0.14], [-0.17, 0.87, -0.22], [-0.04, 1.03, -0.32]],
    [[0.02, 0.50, -0.13], [0.08, 0.73, -0.18], [0.17, 0.95, -0.27], [0.16, 1.13, -0.40]],
    [[0.20, 0.43, -0.12], [0.34, 0.63, -0.18], [0.38, 0.82, -0.29], [0.30, 0.98, -0.42]],
    [[0.36, 0.28, -0.10], [0.48, 0.43, -0.17], [0.54, 0.58, -0.28], [0.50, 0.72, -0.39]],
  ];
  crests.forEach((points, i) => {
    kit.sweep(`crown_flame_${i}`, plumage, crown, points,
      [0.070, 0.115, 0.082, 0.001], [0.055, 0.065, 0.034, 0.001], i % 2 ? sunlight : fire);
    if (i === 1 || i === 2) {
      kit.sweep(`crown_glint_${i}`, plumage, crown, points.map((p, j) => [p[0] + 0.015, p[1] - 0.03, p[2] + 0.075 * (1 - j / 3)]),
        [0.025, 0.044, 0.022, 0.001], [0.010, 0.014, 0.008, 0.001], sunlight);
    }
  });
  kit.articulated(crown, 'z', 0.018, 0.8);
  kit.sweep('upper_beak', beak, head,
    [[0, -0.04, 0.43], [0, -0.06, 0.59], [0, -0.13, 0.77], [0, -0.22, 0.83], [0, -0.28, 0.75]],
    [0.078, 0.102, 0.072, 0.040, 0.002], [0.055, 0.078, 0.052, 0.030, 0.002]);
  kit.ball('lower_beak', gold, head, [0, -0.235, 0.615], [0.070, 0.050, 0.090]);
  kit.line('beak_seam', dark, head, [[-0.060, -0.205, 0.671], [0, -0.235, 0.735], [0.060, -0.205, 0.671]], 0.006);
  return body;
}

function makeCinder(kit, accent) {
  const warm = mix('#ffb04d', accent, 0.18);
  const shell = kit.material('Cinder / graphite ceramic', '#262832', { roughness: 0.26, metalness: 0.48, clearcoat: 0.85 });
  const shadow = kit.material('Cinder / soft joints', '#181b22', { roughness: 0.55, metalness: 0.18 });
  const muzzle = kit.material('Cinder / muzzle', '#3b3b43', { roughness: 0.38, metalness: 0.30 });
  const copper = kit.material('Cinder / copper trim', '#c9823d', { roughness: 0.29, metalness: 0.78 });
  const glow = kit.material('Cinder / ember core', warm, { roughness: 0.22, emissive: warm, emissiveIntensity: 0.60, clearcoat: 1 });
  const gradient = kit.material('Cinder / ear enamel', '#ffffff', { vertexColors: true, roughness: 0.34, clearcoat: 0.7 });
  const body = kit.group('body');
  kit.ball('seated_body', shell, body, [0, 0.84, -0.035], [0.46, 0.61, 0.39]);
  kit.ball('chest_panel', muzzle, body, [0, 1.035, 0.27], [0.31, 0.32, 0.16]);
  kit.ring('collar', copper, body, 0.30, 0.028, [0, 1.31, 0], [Math.PI / 2, 0, 0]);
  for (const s of [-1, 1]) {
    kit.ball('collar_rivet', copper, body, [s * 0.19, 1.315, 0.235], [0.025, 0.025, 0.018]);
    kit.line('chest_inlay', copper, body,
      [[s * 0.22, 1.20, 0.335], [s * 0.25, 1.05, 0.405], [s * 0.20, 0.88, 0.36]], 0.009);
  }
  kit.sweep('chest_ember', glow, body,
    [[0, 1.20, 0.40], [-0.055, 1.11, 0.44], [0, 1.02, 0.45], [0.055, 1.14, 0.42]],
    [0.015, 0.075, 0.065, 0.001], [0.008, 0.025, 0.023, 0.001]);
  for (const s of [-1, 1]) {
    kit.ball('haunch', shell, body, [s * 0.34, 0.42, -0.12], [0.30, 0.34, 0.34]);
    const leg = kit.group(`foreleg_${s}`, body, [s * 0.25, 0.91, 0.20]);
    kit.ball('foreleg', shell, leg, [s * 0.012, -0.30, 0.035], [0.16, 0.35, 0.18], [0.13, 0, -s * 0.06]);
    kit.ball('paw', shell, leg, [s * 0.025, -0.735, 0.14], [0.215, 0.155, 0.255]);
    kit.ring('cuff', copper, leg, 0.132, 0.016, [s * 0.01, -0.58, 0.065], [Math.PI / 2, 0, 0]);
    for (const t of [-1, 1]) kit.line('toe_seam', shadow, leg,
      [[s * 0.025 + t * 0.058, -0.73, 0.383], [s * 0.025 + t * 0.06, -0.652, 0.323]], 0.008);
    kit.ball('rear_paw', shadow, body, [s * 0.46, 0.13, -0.035], [0.23, 0.125, 0.275]);
  }
  const tail = kit.group('curled_tail', body);
  kit.sweep('tail', shell, tail,
    [[0.18, 0.43, -0.28], [0.70, 0.29, -0.64], [1.02, 0.60, -0.61], [0.91, 1.06, -0.44], [0.71, 1.18, -0.30]],
    [0.17, 0.19, 0.18, 0.14, 0.04], [0.17, 0.19, 0.18, 0.14, 0.04]);
  kit.sweep('ember_tail_tip', gradient, tail,
    [[0.87, 1.055, -0.44], [0.80, 1.19, -0.35], [0.71, 1.24, -0.27], [0.73, 1.42, -0.25]],
    [0.095, 0.14, 0.08, 0.001], [0.092, 0.13, 0.065, 0.001], ['#b87833', warm, '#ffdf9d']);
  kit.articulated(tail, 'y', 0.06, 0.6);
  const head = kit.group('head', body, [0, 1.88, 0.025]); kit.head = head;
  kit.ball('sculpted_head', shell, head, [0, 0, 0], [0.76, 0.66, 0.60]);
  for (const s of [-1, 1]) {
    const ear = kit.group(`ear_${s}`, head);
    const points = [[s * 0.46, 0.30, -0.08], [s * 0.58, 0.60, -0.10], [s * 0.66, 0.89, -0.08], [s * 0.60, 1.11, -0.02]];
    kit.sweep('outer_ear', shell, ear, points, [0.22, 0.245, 0.13, 0.001], [0.18, 0.13, 0.07, 0.001]);
    kit.sweep('ear_inset', gradient, ear,
      [[s * 0.47, 0.44, 0.085], [s * 0.57, 0.64, 0.023], [s * 0.62, 0.84, -0.005], [s * 0.60, 1.00, -0.016]],
      [0.085, 0.12, 0.063, 0.001], [0.015, 0.023, 0.014, 0.001], ['#75431e', '#e79a49', '#ffd580']);
    kit.articulated(ear, 'z', s * 0.022, s);
    eye(kit, head, s * 0.294, -0.005, 0.541, { size: 1.12, glow: true, accent: warm, tilt: -s * 0.12 });
    kit.ball('muzzle_pad', muzzle, head, [s * 0.123, -0.305, 0.529], [0.18, 0.12, 0.12]);
    kit.line('cheek_trim', copper, head,
      [[s * 0.67, -0.07, 0.27], [s * 0.64, -0.21, 0.38], [s * 0.49, -0.31, 0.44]], 0.013);
    kit.line('brow_trim', copper, head,
      [[s * 0.15, 0.19, 0.565], [s * 0.30, 0.22, 0.555], [s * 0.42, 0.16, 0.50]], 0.010);
  }
  const noseGeometry = new THREE.OctahedronGeometry(1, 1);
  kit.mesh('button_nose', noseGeometry, shadow, head, [0, -0.243, 0.654], [0.083, 0.055, 0.065], [0, 0, Math.PI]);
  const smile = kit.group('smile', head, [0, -0.335, 0.637]);
  kit.line('smile_left', shadow, smile, [[0, 0.042, 0], [0, 0, 0.014], [-0.074, -0.012, -0.009], [-0.10, 0.008, -0.02]], 0.01);
  kit.line('smile_right', shadow, smile, [[0, 0, 0.014], [0.074, -0.012, -0.009], [0.10, 0.008, -0.02]], 0.01);
  kit.mouths.push(smile);
  return body;
}

function makeKiln(kit, accent) {
  const glowColor = mix('#ffbf58', accent, 0.16);
  const ceramic = kit.material('Kiln / warm porcelain shell', '#e7d4b9', { metalness: 0.13, roughness: 0.28, clearcoat: 0.9, clearcoatRoughness: 0.20 });
  const brass = kit.material('Kiln / satin bronze', '#b77b43', { metalness: 0.83, roughness: 0.31, clearcoat: 0.25 });
  const dark = kit.material('Kiln / charcoal joints', '#282a2e', { metalness: 0.50, roughness: 0.38 });
  const glass = kit.material('Kiln / curved obsidian visor', '#171b21', { metalness: 0.32, roughness: 0.18, clearcoat: 1, clearcoatRoughness: 0.14 });
  const glow = kit.material('Kiln / live ember', glowColor, { emissive: glowColor, emissiveIntensity: 1.05, roughness: 0.22, clearcoat: 0.8 });
  const rim = kit.material('Kiln / rim highlight', '#e1ad6f', { metalness: 0.85, roughness: 0.25 });
  const body = kit.group('body');
  kit.ball('body_housing', ceramic, body, [0, 0.69, -0.025], [0.45, 0.38, 0.36]);
  kit.ball('belly_plate', dark, body, [0, 0.67, 0.278], [0.33, 0.26, 0.13]);
  kit.cylinder('neck_pivot', brass, body, 0.18, 0.23, 0.29, [0, 1.15, -0.025]);
  for (let i = 0; i < 3; i++) kit.ring('neck_spring', dark, body, 0.185, 0.025, [0, 1.04 + i * 0.085, -0.025], [Math.PI / 2, 0, 0]);
  kit.ring('furnace_port', brass, body, 0.133, 0.035, [0, 0.73, 0.397]);
  kit.ball('furnace_core', glow, body, [0, 0.73, 0.388], [0.096, 0.096, 0.04]);
  kit.line('belly_panel_seam', rim, body,
    [[-0.23, 0.84, 0.355], [-0.28, 0.67, 0.392], [-0.21, 0.48, 0.345]], 0.010);
  kit.line('belly_panel_seam', rim, body,
    [[0.23, 0.84, 0.355], [0.28, 0.67, 0.392], [0.21, 0.48, 0.345]], 0.010);
  for (const s of [-1, 1]) for (const y of [0.50, 0.88])
    kit.ball('panel_fastener', brass, body, [s * 0.22, y, 0.34], [0.026, 0.026, 0.015]);
  for (const s of [-1, 1]) {
    const arm = kit.group(`arm_${s}`, body, [s * 0.455, 0.86, -0.01]);
    kit.ball('shoulder_bearing', brass, arm, [0, 0, 0], [0.17, 0.17, 0.17]);
    kit.ball('arm_sleeve', dark, arm, [s * 0.07, -0.195, 0.005], [0.13, 0.23, 0.13], [0, 0, s * 0.2]);
    kit.ball('mitten', ceramic, arm, [s * 0.105, -0.355, 0.02], [0.17, 0.17, 0.17]);
    kit.articulated(arm, 'z', s * 0.035, 1.1);
    kit.cylinder('shin', dark, body, 0.105, 0.13, 0.31, [s * 0.235, 0.31, 0.015], [0, 0, s * -0.08]);
    kit.ball('boot', brass, body, [s * 0.25, 0.145, 0.12], [0.25, 0.145, 0.32]);
    kit.ball('boot_toe', ceramic, body, [s * 0.25, 0.18, 0.29], [0.21, 0.11, 0.17]);
    kit.ball('sole', dark, body, [s * 0.25, 0.059, 0.10], [0.25, 0.047, 0.32]);
  }
  const head = kit.group('head', body, [0, 1.99, 0]); kit.head = head;
  // An actual open spherical helmet and inset cap, not a face painted on a ball.
  const helmet = new THREE.SphereGeometry(0.94, kit.segments * 2, kit.segments, 0, TAU, 0.83, Math.PI - 0.83);
  helmet.rotateX(Math.PI / 2);
  kit.mesh('open_helmet_shell', helmet, ceramic, head, [0, 0, 0], [1.02, 0.95, 1]);
  const visor = new THREE.SphereGeometry(0.926, kit.segments * 2, kit.segments, 0, TAU, 0, 0.86);
  visor.rotateX(Math.PI / 2);
  kit.mesh('spherical_visor', visor, glass, head, [0, 0, 0], [1.02, 0.95, 1]);
  kit.ring('visor_gasket', dark, head, 0.693, 0.055, [0, 0, 0.629], [0, 0, 0], [1.02, 0.95, 1]);
  kit.ring('machined_face_rim', rim, head, 0.707, 0.028, [0, 0, 0.648], [0, 0, 0], [1.02, 0.95, 1]);
  for (const s of [-1, 1]) {
    kit.cylinder('ear_housing', brass, head, 0.278, 0.278, 0.14, [s * 0.98, 0.0, -0.09], [0, 0, Math.PI / 2]);
    kit.cylinder('ear_inner', dark, head, 0.217, 0.217, 0.015, [s * 1.065, 0.0, -0.09], [0, 0, Math.PI / 2]);
    kit.ring('ear_concentric_ring', rim, head, 0.184, 0.026, [s * 1.078, 0.0, -0.09], [0, Math.PI / 2, 0]);
    kit.cylinder('ear_core', glow, head, 0.095, 0.095, 0.018, [s * 1.092, 0, -0.09], [0, 0, Math.PI / 2]);
    for (let i = 0; i < 4; i++) {
      const a = i * Math.PI / 2 + Math.PI / 4;
      kit.ball('ear_fastener', dark, head, [s * 1.06, Math.cos(a) * 0.247, -0.09 + Math.sin(a) * 0.247], [0.027, 0.023, 0.023]);
    }
    const e = kit.blinkable(`eye_${s}`, head, [s * 0.238, 0.028, 0.899]);
    kit.ball('glowing_eye', glow, e, [0, 0, 0], [0.077, 0.162, 0.032]);
    kit.ball('eye_core', kit.material('Kiln / eye center', '#ffdfa0', { emissive: '#ffc87a', emissiveIntensity: 0.8, roughness: 0.35 }),
      e, [-0.007, 0.008, 0.027], [0.039, 0.122, 0.012]);
  }
  const mouth = kit.group('smile', head, [0, -0.253, 0.884]);
  kit.line('smile_arc', rim, mouth, [[-0.091, 0.026, -0.007], [-0.05, -0.006, 0.014], [0.015, -0.016, 0.019], [0.085, 0.025, -0.006]], 0.014);
  kit.mouths.push(mouth);
  kit.line('helmet_top_seam', brass, head, [[0, 0.72, 0.49], [0, 0.89, 0.06], [0, 0.71, -0.49], [0, 0.18, -0.92]], 0.018);
  for (const s of [-1, 1]) {
    kit.line('helmet_side_seam', rim, head,
      [[s * 0.55, 0.58, 0.59], [s * 0.69, 0.28, 0.70], [s * 0.70, -0.20, 0.70]], 0.010);
  }
  const antenna = kit.group('antenna', head, [-0.41, 0.64, -0.13]);
  kit.line('antenna_stem', brass, antenna, [[0, 0, 0], [-0.09, 0.21, 0], [-0.01, 0.42, 0.005]], 0.031);
  kit.ball('antenna_ember', glow, antenna, [-0.01, 0.435, 0.005], [0.077, 0.10, 0.07]);
  kit.ring('antenna_collar', dark, antenna, 0.053, 0.021, [-0.013, 0.36, 0.005], [Math.PI / 2, 0, 0]);
  kit.articulated(antenna, 'z', 0.022, 0.3);
  return body;
}

/** Smooth-union implicit cloud: the silhouette is one connected 3D surface. */
function cloudGeometry(segments) {
  const resolution = segments < 30 ? 32 : 48;
  const mc = new MarchingCubes(resolution, new THREE.MeshBasicMaterial(), false, false, 40000);
  mc.isolation = 0;
  const lobes = [
    [0, 0.10, 0, 0.68], [-0.53, -0.23, 0.02, 0.45], [0.55, -0.22, -0.03, 0.46],
    [-0.88, -0.07, -0.08, 0.34], [0.88, -0.06, -0.08, 0.35],
    [-0.40, 0.46, -0.12, 0.40], [0.36, 0.49, -0.10, 0.39], [0, 0.70, -0.08, 0.34],
    [-0.28, -0.53, -0.02, 0.29], [0.26, -0.52, -0.04, 0.31],
  ];
  const extent = 1.65, smoothness = 0.25;
  for (let z = 0; z < resolution; z++) for (let y = 0; y < resolution; y++) for (let x = 0; x < resolution; x++) {
    const px = (x / resolution * 2 - 1) * extent;
    const py = (y / resolution * 2 - 1) * extent;
    const pz = (z / resolution * 2 - 1) * extent / 0.76;
    let distance = 10;
    for (const [cx, cy, cz, radius] of lobes) {
      const b = Math.hypot(px - cx, py - cy, pz - cz) - radius;
      const h = Math.max(smoothness - Math.abs(distance - b), 0) / smoothness;
      distance = Math.min(distance, b) - h * h * h * smoothness / 6;
    }
    mc.field[z * resolution * resolution + y * resolution + x] = -distance;
  }
  mc.update();
  // Keep only live triangles; the MarchingCubes staging capacity is not the model.
  const n = mc.geometry.drawRange.count;
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.Float32BufferAttribute(mc.geometry.attributes.position.array.slice(0, n * 3), 3));
  geometry.setAttribute('normal', new THREE.Float32BufferAttribute(mc.geometry.attributes.normal.array.slice(0, n * 3), 3));
  geometry.scale(extent, extent, extent);
  mc.geometry.dispose(); mc.material.dispose();
  return geometry;
}

function makeWisp(kit, accent) {
  const warm = mix('#f8a951', accent, 0.22);
  const cloud = kit.material('Wisp / warm opal', '#ffffff', { vertexColors: true, roughness: 0.35,
    metalness: 0.02, clearcoat: 0.9, clearcoatRoughness: 0.25, sheen: 0.25, sheenColor: '#ffe7af', sheenRoughness: 0.75 });
  const face = kit.material('Wisp / face', '#332022', { roughness: 0.24, clearcoat: 0.8 });
  const shine = kit.material('Wisp / catchlights', '#fff8df', { emissive: '#fff0ba', emissiveIntensity: 0.20 });
  const blush = kit.material('Wisp / warm cheeks', '#ec8257', { roughness: 0.5 });
  const plume = kit.material('Wisp / flame crown', '#ffffff', { vertexColors: true, roughness: 0.30, clearcoat: 0.8 });
  const body = kit.group('body', kit.object, [0, 1.30, 0]);
  const geometry = cloudGeometry(kit.segments), color = [];
  const positions = geometry.attributes.position;
  for (let i = 0; i < positions.count; i++) {
    const t = THREE.MathUtils.clamp((positions.getY(i) + 0.6) / 1.6, 0, 1);
    const c = t < 0.55 ? mix('#e78055', warm, t / 0.55) : mix(warm, '#ffe0a1', (t - 0.55) / 0.45);
    color.push(c.r, c.g, c.b);
  }
  geometry.setAttribute('color', new THREE.Float32BufferAttribute(color, 3));
  kit.mesh('continuous_cloud_surface', geometry, cloud, body);
  const head = kit.group('face_rig', body); kit.head = head;
  for (const s of [-1, 1]) {
    const e = kit.blinkable(`eye_${s}`, head, [s * 0.258, 0.17, 0.485]);
    kit.ball('bean_eye', face, e, [0, 0, 0], [0.066, 0.098, 0.042], [0, s * 0.22, -s * 0.08]);
    kit.ball('catchlight', shine, e, [-0.016, 0.032, 0.032], [0.019, 0.026, 0.010]);
    kit.ball('blush', blush, head, [s * 0.385, -0.025, 0.448], [0.105, 0.048, 0.020], [0, s * 0.28, 0]);
  }
  const mouth = kit.group('smile', head, [0, -0.079, 0.511]);
  kit.line('smile_arc', face, mouth, [[-0.106, 0.039, -0.013], [-0.066, -0.04, 0.0], [0.017, -0.051, 0.010], [0.10, 0.041, -0.009]], 0.018);
  kit.mouths.push(mouth);
  const tuft = kit.group('crown', body);
  kit.sweep('curling_flame', plume, tuft,
    [[0.02, 0.79, -0.065], [-0.06, 1.00, -0.03], [0.04, 1.18, -0.015], [0.22, 1.24, -0.06], [0.24, 1.40, -0.08]],
    [0.135, 0.175, 0.12, 0.066, 0.001], [0.11, 0.12, 0.079, 0.046, 0.001], [warm, '#ffc77e', '#fff0bb']);
  kit.sweep('flame_wisp_left', plume, tuft,
    [[-0.24, 0.62, -0.06], [-0.38, 0.78, -0.04], [-0.35, 0.98, -0.08], [-0.27, 1.10, -0.13]],
    [0.060, 0.080, 0.050, 0.001], [0.050, 0.055, 0.032, 0.001], [warm, '#ffc77e', '#fff0bb']);
  kit.sweep('flame_wisp_right', plume, tuft,
    [[0.31, 0.57, -0.08], [0.43, 0.71, -0.06], [0.45, 0.87, -0.10], [0.39, 1.00, -0.15]],
    [0.052, 0.072, 0.045, 0.001], [0.045, 0.052, 0.029, 0.001], [warm, '#ffc77e', '#fff0bb']);
  kit.articulated(tuft, 'z', 0.025, 0.5);
  const sparkMat = kit.material('Wisp / floating embers', '#ffd797', { emissive: '#ffbd63', emissiveIntensity: 0.42, roughness: 0.30 });
  const embers = kit.group('orbiting_embers', body);
  kit.ball('ember_left', sparkMat, embers, [-0.85, 0.83, -0.06], [0.046, 0.068, 0.046], [0, 0, -0.4]);
  kit.ball('ember_right', sparkMat, embers, [0.98, 0.47, -0.12], [0.038, 0.056, 0.038], [0, 0, 0.3]);
  kit.ball('ember_high', sparkMat, embers, [0.68, 1.14, -0.10], [0.025, 0.040, 0.025], [0, 0, 0.2]);
  kit.articulated(embers, 'z', 0.035, 1.5);
  return body;
}

/** Construct one independent model. Numbers are scene units; geometry is editable. */
export function createSidekick(kind, { color = '#f2a063', quality = 'studio' } = {}) {
  if (!SIDEKICKS.includes(kind)) throw new RangeError(`Unknown Phoenix sidekick: ${String(kind)}`);
  if (typeof color !== 'string' || !/^#[0-9a-f]{6}$/i.test(color)) throw new TypeError('Sidekick color must be a six-digit hex color');
  if (!['studio', 'avatar'].includes(quality)) throw new RangeError('Sidekick quality must be studio or avatar');
  const kit = new Kit(kind, quality);
  let body;
  try { body = ({ ember: makeEmber, cinder: makeCinder, kiln: makeKiln, wisp: makeWisp })[kind](kit, color); }
  catch (error) { kit.dispose(); throw error; }
  const bodyRestY = body.position.y;
  const head = kit.head, headRest = head.rotation.clone();
  let meshes = 0, triangles = 0;
  kit.object.traverse(node => {
    if (node.isMesh) { meshes++; triangles += (node.geometry.index?.count ?? node.geometry.attributes.position.count) / 3; }
  });
  const stats = Object.freeze({ kind, meshes, triangles, geometries: kit.geometries.size, materials: kit.materials.size,
    textures: 0, generatedFromCode: true });
  function update(seconds = 0, { expression = 'bright', lookX = 0, lookY = 0, activity = 'idle', reducedMotion = false } = {}) {
    if (kit.disposed) return;
    if (!Number.isFinite(seconds)) seconds = 0;
    const t = reducedMotion ? 0 : seconds;
    expression = ({ focused: 'curious', determined: 'bright' })[expression] || expression;
    if (!EXPRESSIONS.includes(expression)) expression = 'bright';
    const busy = activity === 'working' || activity === 'thinking';
    const joy = expression === 'joy' || activity === 'celebrate';
    const sleepy = expression === 'sleepy';
    const amount = reducedMotion ? 0 : 1;
    const look = value => Number.isFinite(value) ? THREE.MathUtils.clamp(value, -1, 1) : 0;
    const lx = look(lookX), ly = look(lookY);
    const breathe = Math.sin(t * 1.7) * 0.015 * amount;
    body.position.y = bodyRestY + (kind === 'wisp' ? Math.sin(t * 1.5) * 0.055 : Math.sin(t * 1.8) * 0.012) * amount;
    body.scale.set(1 - breathe * 0.3, 1 + breathe, 1 - breathe * 0.3);
    head.rotation.set(headRest.x - ly * 0.085 + (sleepy ? 0.09 : 0),
      headRest.y + lx * 0.13 + Math.sin(t * 0.63) * 0.022 * amount,
      headRest.z + (expression === 'curious' ? 0.09 : expression === 'mischief' ? -0.065 : 0)
        + Math.sin(t * 0.84) * 0.014 * amount);
    const cycle = t % 5.7;
    const blink = !reducedMotion && cycle > 4.85 && cycle < 5.07 ? Math.max(0.055, Math.abs((cycle - 4.96) / 0.11)) : 1;
    kit.eyeRigs.forEach((eye, i) => {
      let open = sleepy ? 0.40 : expression === 'calm' ? 0.76 : 1;
      if (expression === 'mischief' && i === 0) open *= 0.68;
      if (joy) open *= 0.76;
      eye.scale.y = Math.max(0.045, open * blink);
    });
    kit.mouths.forEach(mouth => { mouth.scale.y = joy ? 1.55 : sleepy ? 0.72 : 1; });
    for (const item of kit.moving) {
      item.group.rotation[item.axis] = item.rest + Math.sin(t * (busy ? 3.8 : 1.6) + item.phase) * item.amount * (joy ? 2.5 : 1) * amount;
    }
    if (joy && !reducedMotion) body.position.y += Math.pow(Math.max(0, Math.sin(t * 3.2)), 2) * 0.04;
  }
  update(0);
  return { object: kit.object, update, dispose: () => kit.dispose(), stats };
}
