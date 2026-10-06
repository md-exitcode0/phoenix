# Phoenix sidekicks

Four editable, procedural 3D characters. Every visible character part is built in
`models.mjs`: meshes, curves, vertex colors, material parameters and animation.
There are no character PNGs, SVG cutouts, downloaded character models or face
textures. The lighting environment is generated separately by the renderer.

| Model | Construction |
| --- | --- |
| Ember | Rounded bird body, swept flame feathers, articulated wings and tail |
| Cinder | Graphite scout with modeled ears, muzzle, paws and a curved ember tail |
| Kiln | Open spherical ceramic helmet, inset visor, bronze joints and furnace core |
| Wisp | Connected smooth-union cloud surface, face geometry and a flame curl |

## In Phoenix

Open a coworker's **Configure** dialog, select **3D sidekick**, choose a model and
expression, then save. Existing custom images and legacy flame settings remain
stored when switching modes. A model is never assigned to an agent implicitly.
The company palette and other appearance settings are not changed.

The **model studio** is available from the avatar editor. Its standalone page is
`canvas-app/ui/sidekick-studio.html`; serve that UI directory locally or use the
native editor's embedded studio. It offers rotation, expression/color controls,
light/dark surfaces, a wireframe view, native-size previews and GLB export.

## Use the models directly

```js
import { createSidekick } from './models.mjs';

const sidekick = createSidekick('kiln', {
  color: '#f2a063',
  quality: 'studio', // or 'avatar'
});
scene.add(sidekick.object);

// Call with elapsed seconds inside your render loop.
sidekick.update(elapsedSeconds, {
  expression: 'curious',
  lookX: 0.2,
  lookY: 0,
  activity: 'idle',
  reducedMotion: false,
});

// When the character is no longer used:
sidekick.dispose();
```

Models face +Z, with +Y up. The six expressions are `bright`, `joy`, `calm`,
`curious`, `mischief` and `sleepy`. `thinking`/`working` activity changes the
appendage cadence; `celebrate` adds a small reaction. Reduced motion holds a
stable pose without blinking or autonomous movement.

## Use the UI renderer

Load the locally bundled `sidekicks-bundle.js`, then insert:

```html
<canvas class="phoenix-sidekick"
        data-sidekick="kiln"
        data-color="#f2a063"
        data-expression="curious"
        data-sidekick-interactive="true"
        style="width:160px;height:160px"
        aria-label="Kiln 3D preview"></canvas>
```

The runtime observes added/removed canvases and changed attributes. It shares a
single lazy WebGL renderer per document and copies each genuine 3D render to a
2D presentation canvas. It does not create one WebGL context per avatar. Small
idle avatars do not need a continuous animation loop. Invisible canvases stop
rendering; OS reduced motion and Phoenix's Minimal motion preference apply.

`window.PhoenixSidekicks` exposes `refresh`, `dispose`, `stats`, `setState`,
`getState`, `resetRotation`, `setPaused`, `renderAt`, `setTime` and `exportGLB`.
`renderAt(0)` holds a reproducible frame; `setTime(null)` resumes the live clock.
WebGL failure displays an explicit unavailable state rather than pretending a
static picture is a model.

## Build and test

From this directory, with Node.js available:

```sh
npm ci --ignore-scripts
npm test
npm run build
node export.mjs ./exports
```

The build reuses the existing Phoenix Chromium-shell esbuild installation. The
only new dependency is pinned Three.js. The production bundles work without a
CDN or network connection. `export.mjs` writes one GLB per model and reimports
each to verify its complete mesh and triangle counts. Existing output files are
not overwritten. GLBs contain static meshes, materials and the selected pose;
the interactive procedural animation is JavaScript in `models.mjs`, not baked
animation clips or a skinning rig.

Three.js is MIT-licensed; its full notice is retained in `THREE-LICENSE.txt`.
