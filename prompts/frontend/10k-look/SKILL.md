---
name: the-10k-look
description: Field guide for 3D/immersive/award-style websites — ten laws of the expensive feel, the stack (three.js/R3F, GSAP, Lenis), motion language (damped values, easing, choreography), lighting and materials, shaders, reactive-background recipes, postprocessing budgets, scroll craft, micro-interactions, loading, performance, accessibility floor, pre-ship checklist.
---

# THE $10K LOOK
### A complete field guide to building 3D websites that look like money

> Everything in here is battle-tested taste + working code. The goal: sites with
> depth, motion, and atmosphere — the kind that win Awwwards and make clients
> assume a five-figure invoice. Read part I even if you skip everything else;
> the difference between "$10k" and "template" is 80% taste, 20% tech.

---

## Table of contents

- **[Part I — Why expensive sites feel expensive](#part-i)** (the principles)
- **[Part II — The stack](#part-ii)** (pick these, stop researching)
- **[Part III — Typography, color & layout](#part-iii)** (the 80% that isn't 3D)
- **[Part IV — The motion language](#part-iv)** (easing, damping, choreography)
- **[Part V — The 3D scene done right](#part-v)** (cameras, lighting, materials, models)
- **[Part VI — Shaders: the actual magic](#part-vi)** (noise, warping, interactivity)
- **[Part VII — Reactive backgrounds: full recipes](#part-vii)**
- **[Part VIII — Postprocessing](#part-viii)** (the film look, with a budget)
- **[Part IX — The scroll experience](#part-ix)** (Lenis, pinning, camera paths)
- **[Part X — Micro-interactions](#part-x)** (magnetic buttons, cursors, text reveals)
- **[Part XI — Loading & first impression](#part-xi)**
- **[Part XII — Performance](#part-xii)** (budgets, mobile, on-demand rendering)
- **[Part XIII — Accessibility & graceful degradation](#part-xiii)**
- **[Part XIV — The pre-ship checklist](#part-xiv)**
- **[Part XV — Study list](#part-xv)** (sites, studios, resources)

---

<a name="part-i"></a>
# PART I — Why expensive sites feel expensive

Before any code: cheap sites and expensive sites usually contain the *same
ingredients*. The difference is discipline. Internalize these ten laws.

## 1. One idea per site

Every legendary site is ONE visual concept executed relentlessly. A floating
glass object. A particle field that answers the cursor. A camera falling
through a scene as you scroll. Pick one hero idea and make everything else
serve it. Two competing ideas read as noise; one idea repeated at every scale
(hero, hover states, page transitions) reads as intent — and intent reads as
budget.

**Test:** describe your site's visual concept in one sentence. If you need
"and", cut something.

## 2. Motion hierarchy: everything moves, nothing jumps

On an expensive site, *nothing* appears at 100% opacity in 0ms. Everything
arrives — but arrival is choreographed, not uniform:

- **Background layer** — slow, continuous, ignorable (drifting gradient, particle idle)
- **Content layer** — enters once, on scroll, with stagger; then holds still
- **Interactive layer** — responds in <100ms but *settles* slowly (lerp out)

The eye should always find exactly one thing "most alive." When everything
animates at the same amplitude, nothing feels alive — it feels like a
PowerPoint transition pack.

## 3. Ease-out is the sound of money

The single highest-leverage change you can make to any site: replace every
`ease` / `linear` / default with a long-tailed ease-out. Expensive motion
starts fast and lands like a feather. Cheap motion is symmetric (`ease-in-out`
on everything) or, worse, linear.

```
Cheap:      ▁▂▃▄▅▆▇█   linear — mechanical
Cheap:      ▁▂▄▆█▆▄▂▁  ease-in-out everywhere — floaty, indecisive
Expensive:  ▁▅▇███████  expo/quint-out — decisive, then delicate
```

Full easing library in Part IV.

## 4. Near-monochrome + one signal color

Look at any award site: the palette is 90% one family (usually near-black or
near-white with 3–4 gray steps) plus ONE hue that means "alive" — used for the
accent glow, the hover state, the active element. Color is a *signal*, not
decoration. When everything is colorful, color stops carrying information.

**Rule: color = alive.** Full saturation only on things that are currently
responding to the user. Everything settled goes near-monochrome.

## 5. Detail density: the 2% that reads as 98%

Grain overlay at 2–4% opacity. Hairline (1px, 10% alpha) borders instead of
box-shadows. Tiny monospace labels (`SCROLL`, `01 / 04`, `©2026`) in the
corners. Micro-parallax of 4–8px on hover. A custom selection color. None of
these are individually noticeable; together they are the entire difference
between "designed" and "assembled." Cheap sites have zero of these. Expensive
sites have all of them and none of them shout.

## 6. Whitespace is the most expensive material

Big type, few words, oceans of space. A hero with 6 words at 8vw reads as
confidence. A hero with a paragraph reads as a brochure. Spacing scale should
be geometric (see Part III) and the biggest gaps should feel *slightly too
big* in your editor — that's how you know they're right in the browser.

## 7. Light sells 3D; geometry doesn't

A cube lit beautifully beats a detailed model lit flatly, every time.
Beginners spend 90% of their time on models and 10% on lighting. Invert that.
Environment maps, one strong rim light, and soft contact shadows will make
primitive geometry look like a product shoot (recipes in Part V).

## 8. The background must answer the user

The "$10k feel" is largely: *the site notices you*. Cursor moves → the
gradient leans toward it, the particles part, the object tilts 3°. Scroll
velocity → the scene stretches or blurs slightly. These reactions must be
**damped** (Part IV) — instant reaction is Flash-era; lagged, weighted
reaction is luxury. The background is a pet, not a mirror.

## 9. Restraint in postprocessing

Bloom threshold high enough that only emissive things glow. Grain barely
visible. Chromatic aberration only at the screen edges or during motion, if at
all. Every effect you can *name* while browsing the site is over-tuned. The
film look should be felt, not seen (budgets in Part VIII).

## 10. Performance IS aesthetics

A dropped frame destroys the illusion faster than any design mistake. 60fps
mediocre beats 30fps gorgeous, always. Every decision in this guide comes with
its cost; Part XII is the bill. Treat the frame budget like a design
constraint, because it is one.

---

<a name="part-ii"></a>
# PART II — The stack

Pick these. Stop researching. This combination is what the top studios
actually ship.

## The core

| Need | Use | Why |
|---|---|---|
| 3D engine | **three.js** | The ecosystem. Nothing else is close for web. |
| React integration | **@react-three/fiber (R3F)** | Declarative scenes, hooks, ecosystem |
| 3D helpers | **@react-three/drei** | 90% of common tasks pre-solved (cameras, env maps, text, scroll) |
| DOM animation | **GSAP** (+ ScrollTrigger) | Timelines, scroll choreography, the industry standard |
| Smooth scroll | **lenis** | The de-facto award-site scroll feel |
| Postprocessing | **@react-three/postprocessing** (or `postprocessing` vanilla) | Fast, batched effects |
| Text splitting | **SplitType** (free) or GSAP SplitText | Line/word/char reveals |
| Utility springs | **maath** | damp/spring helpers designed for r3f |

```bash
npm i three @react-three/fiber @react-three/drei @react-three/postprocessing gsap lenis split-type maath
```

## Vanilla three vs React Three Fiber

| | Vanilla three.js | R3F |
|---|---|---|
| Best for | Single-canvas experiences, shader art, maximum control | Product sites, content + 3D mixed, teams |
| Learning value | Higher — you touch everything | Lower ceiling of understanding |
| Speed to ship | Slower | Much faster (drei does the boring parts) |
| Perf ceiling | Identical (R3F has no meaningful overhead) | Identical |

**Recommendation:** R3F + drei for anything with real content and deadlines.
Learn vanilla three underneath it anyway — every hard problem drops you to
raw three, and shaders don't care which wrapper you use.

## Framework

Anything works. Next.js/Astro/Vite+React are all fine. Two rules:

1. The canvas must never remount on route change — keep it outside the router
   outlet, or use a persistent layout.
2. 3D components load client-side only (`dynamic(() => import(...), { ssr:
   false })` in Next). Server-render the DOM content for SEO/LCP; hydrate the
   canvas after.

## What to skip

- **Physics engines** (rapier/cannon) — unless physics IS the concept. Fake it with springs.
- **Heavy scroll libraries** other than Lenis — ScrollTrigger + Lenis covers everything.
- **UI animation libraries on top of GSAP** — pick one motion system. Mixing
  Framer Motion and GSAP means two tickers, two easing vocabularies, doubled bugs.

---

<a name="part-iii"></a>
# PART III — Typography, color & layout

The 3D scene gets the screenshots; the typography gets the trust. Most "wow"
sites are typographically simple but *exact*.

## Fonts that read expensive

One display face + one text/UI face + a monospace for labels. Never more.

**Free and genuinely good (Fontshare / Google):**
- Display: **Clash Display**, **Cabinet Grotesk**, **General Sans**, **Bricolage Grotesque**
- Text/UI: **Satoshi**, **General Sans**, **Inter** (use *Inter Display* for large sizes)
- Serif contrast (editorial feel): **Gambetta**, **Sentient**, **EB Garamond**
- Mono labels: **JetBrains Mono**, **IBM Plex Mono**, **Space Grotesk** (grotesk-mono vibe)

**Paid, the award-site defaults:** Neue Montreal, Söhne, Founders Grotesk,
PP Editorial New, Canela, ABC Diatype. (Recognize them in the wild; you don't
need them.)

## Type rules

```css
:root {
  /* Fluid type: min, preferred (vw-based), max */
  --text-hero: clamp(3rem, 8vw, 9rem);
  --text-h2:   clamp(2rem, 4.5vw, 4.5rem);
  --text-body: clamp(1rem, 1.1vw, 1.125rem);
  --text-label: 0.6875rem;               /* 11px — the mono micro-label size */
}

.hero-title {
  font-size: var(--text-hero);
  line-height: 0.95;                     /* display type: 0.9–1.0, ALWAYS < 1.1 */
  letter-spacing: -0.03em;               /* big type gets tighter: -0.02 to -0.05em */
  font-weight: 500;                      /* medium reads more expensive than bold */
  text-wrap: balance;
}

.label {
  font-family: var(--font-mono);
  font-size: var(--text-label);
  letter-spacing: 0.12em;                /* small type gets looser */
  text-transform: uppercase;
  opacity: 0.55;
}

body { line-height: 1.6; }               /* body text: 1.5–1.7 */
```

Non-negotiables:
- Display line-height **under 1.1** and negative tracking. Default 1.5
  line-height on a 7rem headline is the #1 amateur tell.
- Body max-width **60–70ch**.
- Mono uppercase micro-labels with wide tracking = instant editorial credibility.
- `font-display: swap` + preload the display font's woff2 (Part XI).

## Color system

```css
:root {
  /* Near-black canvas — NEVER pure #000 (crushes depth, kills shadow gradation) */
  --bg:        #0a0a0b;
  --surface:   #121214;
  --surface-2: #1a1a1e;

  /* Text ladder — NEVER pure #fff for body */
  --text-hi:   #f2f2f0;   /* headings */
  --text-mid:  rgba(242, 242, 240, 0.65);   /* body */
  --text-low:  rgba(242, 242, 240, 0.38);   /* labels, captions */

  /* ONE signal hue. Alive things only. */
  --accent:      #7c5cff;
  --accent-glow: rgba(124, 92, 255, 0.35);

  /* The hairline — borders everywhere use this, never box-shadow */
  --hairline: rgba(242, 242, 240, 0.09);
}

::selection { background: var(--accent); color: var(--bg); }  /* free polish */
```

Notes:
- Dark-first. Dark canvases make WebGL glow, bloom, and gradients look 3×
  better for free, and hide banding.
- Tint your grays toward the accent by 1–2% — pure neutral gray reads "OS,"
  tinted gray reads "brand."
- Hairlines over shadows: `border: 1px solid var(--hairline)` is the
  premium container treatment. Box-shadows on cards read 2016.

## Spacing & layout

```css
:root {
  /* Geometric scale — big gaps must be MUCH bigger than small ones */
  --space-1: 0.5rem;  --space-2: 1rem;   --space-3: 2rem;
  --space-4: 4rem;    --space-5: 8rem;   --space-6: 16rem;
}
section { padding-block: var(--space-5); }  /* sections breathe: 8rem+ on desktop */
```

- 12-column fluid grid, but *break* it deliberately: one element crossing a
  column boundary or overlapping the 3D canvas creates the layered, art-directed
  feel.
- Full-bleed canvas + inset content: canvas at 100vw behind, text constrained
  to the grid in front.
- Corners: pick ONE radius language. Either sharp (0–2px, editorial) or soft
  (16–24px, product). 8px-everything is the default-Tailwind tell.

## The grain overlay (do this on every project)

```css
.grain {
  position: fixed; inset: -50%;
  width: 200%; height: 200%;
  pointer-events: none;
  z-index: 9999;
  opacity: 0.04;
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='300' height='300'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.9' numOctaves='2'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");
  animation: grain-shift 0.6s steps(4) infinite;
}
@keyframes grain-shift {
  0% { transform: translate(0, 0); }  25% { transform: translate(-2%, 3%); }
  50% { transform: translate(3%, -2%); } 75% { transform: translate(-3%, -3%); }
  100% { transform: translate(0, 0); }
}
@media (prefers-reduced-motion: reduce) { .grain { animation: none; } }
```

Static grain reads as a dirty screen; *animated* grain reads as film. Keep
opacity 0.03–0.05.

---

<a name="part-iv"></a>
# PART IV — The motion language

This part is the guide's core. Motion quality is 100% learnable and it's
where the money-feel actually lives.

## The easing library (copy these)

**CSS:**
```css
:root {
  --ease-out-expo:  cubic-bezier(0.16, 1, 0.3, 1);    /* THE money curve. Default. */
  --ease-out-quint: cubic-bezier(0.22, 1, 0.36, 1);   /* slightly softer arrival */
  --ease-in-out:    cubic-bezier(0.83, 0, 0.17, 1);   /* only for things that leave AND arrive on screen */
  --ease-snap:      cubic-bezier(0.5, 1.6, 0.4, 1);   /* tiny overshoot — micro-interactions only */
}
```

**GSAP:** `expo.out` (default), `power4.out`, `power3.inOut` (transits),
`elastic.out(1, 0.35)` (settle-back on magnetic elements),
`back.out(1.4)` (small UI pops).

## Duration rules

| What | Duration |
|---|---|
| Hover / micro feedback | 150–300ms |
| Element entrance | 600–1000ms |
| Hero / page-level choreography | 1000–1600ms |
| Page transitions | 600–900ms total |
| Anything continuous (backgrounds) | no duration — damped values (below) |

Cheap sites animate everything at 300ms. Expensive sites run micro at 200ms
and macro at 1200ms — the *contrast* between snappy and langorous is the
style. Stagger siblings at 40–90ms; never reveal a list simultaneously.

## Lerp & damp — the soul of "reactive"

Everything continuous (cursor followers, camera drift, hover tilt, background
response) uses damped values, NOT tweens. Tweens have an end; damped values
chase a target forever, which is what makes a scene feel alive.

**Frame-rate-independent damping (use this, not naive lerp):**

```js
// Naive lerp — WRONG for continuous motion: speed depends on refresh rate
value += (target - value) * 0.1;  // 144Hz users get 2.4× faster response

// Correct — exponential decay with delta time:
function damp(current, target, lambda, dt) {
  return current + (target - current) * (1 - Math.exp(-lambda * dt));
}
// three has this built in:
// THREE.MathUtils.damp(current, target, lambda, delta)
// maath has damp for vectors/eulers: import { damp3, dampE } from 'maath/easing'
```

**Lambda cheat sheet** (per-second responsiveness):
- `1.5–3` — dreamy, heavy (background gradients, big meshes)
- `4–6` — luxurious follow (camera parallax, cursor-reactive objects) ← default
- `8–12` — responsive but soft (custom cursor ring, hover tilt)
- `15+` — nearly instant (only for things that must feel physical)

**The universal mouse-parallax rig** (this pattern is half the guide):

```jsx
// R3F: everything "notices" the cursor through damped pointer values
import * as THREE from 'three'
import { useFrame } from '@react-three/fiber'
import { useRef } from 'react'

function ParallaxRig({ children, strength = 0.35 }) {
  const group = useRef()
  useFrame(({ pointer }, dt) => {
    // pointer is already normalized to -1..1
    group.current.rotation.y = THREE.MathUtils.damp(
      group.current.rotation.y, pointer.x * strength, 4, dt)
    group.current.rotation.x = THREE.MathUtils.damp(
      group.current.rotation.x, -pointer.y * strength * 0.6, 4, dt)
  })
  return <group ref={group}>{children}</group>
}
```

Tilt strength 0.2–0.4 radians max. The user should feel *noticed*, not tracked.

## Entrance choreography (GSAP)

One timeline per scene-entrance. Order: structure → content → detail.

```js
import gsap from 'gsap'

const tl = gsap.timeline({ defaults: { ease: 'expo.out', duration: 1.1 } })
tl.fromTo('.hero-bg',    { scale: 1.15, opacity: 0 }, { scale: 1, opacity: 1, duration: 1.6 })
  .fromTo('.hero-line',  { yPercent: 110 }, { yPercent: 0, stagger: 0.08 }, '-=1.1')
  .fromTo('.hero-meta',  { y: 16, opacity: 0 }, { y: 0, opacity: 1, stagger: 0.05, duration: 0.7 }, '-=0.6')
  .fromTo('.hero-cta',   { y: 12, opacity: 0 }, { y: 0, opacity: 1, duration: 0.6 }, '-=0.4')
```

Rules:
- Overlap everything (`'-=x'` position params). Sequential = slideshow;
  overlapped = choreography.
- Scale-down entrances (1.15 → 1) feel like a camera settling; scale-UP
  entrances feel like a popup ad. Enter from 1.05–1.2, land at 1.
- Never animate `top/left/width` — only `transform` and `opacity` (compositor-only).

## Scroll-triggered reveals (the standard section pattern)

```js
import gsap from 'gsap'
import ScrollTrigger from 'gsap/ScrollTrigger'
gsap.registerPlugin(ScrollTrigger)

gsap.utils.toArray('[data-reveal]').forEach((el) => {
  gsap.fromTo(el,
    { y: 48, opacity: 0 },
    {
      y: 0, opacity: 1, duration: 1.1, ease: 'expo.out',
      scrollTrigger: { trigger: el, start: 'top 85%', once: true },
    })
})
```

- `once: true` for content. Re-triggering reveals on scroll-up is noise.
- Start at `top 85%` — reveal *before* the element centers, never after.
- Distance 30–60px. 200px fly-ins are 2015.

## Text reveals (the award-site signature)

```js
import SplitType from 'split-type'

const split = new SplitType('.hero-title', { types: 'lines,words' })
// CSS: .line { overflow: hidden; }  ← the mask
gsap.fromTo(split.words,
  { yPercent: 110 },
  { yPercent: 0, duration: 1.2, ease: 'expo.out', stagger: 0.045 })
```

The overflow-hidden line-mask + yPercent slide is THE text reveal. Char-level
staggers (0.015s) only for short display words — on paragraphs it's seasick.
Re-split on resize (line breaks change). For paragraphs, reveal by *line*, not
word.

## Page transitions

Minimum viable premium: overlay wipe or fade-through-black, 600–900ms, with
the incoming page running its entrance timeline. The 3D canvas persists across
routes (Part II) and *reacts* to the transition — swing the camera, shift the
background hue — so the world feels continuous. If using Next.js App Router,
simplest reliable path is an exit animation on a custom `Link` before
`router.push`, plus entrance timelines keyed to pathname.

---

<a name="part-v"></a>
# PART V — The 3D scene done right

## The canvas skeleton

```jsx
import { Canvas } from '@react-three/fiber'
import { Environment, ContactShadows, Float } from '@react-three/drei'
import { Suspense } from 'react'

export default function Scene() {
  return (
    <Canvas
      dpr={[1, 2]}                          // clamp pixel ratio — free perf
      camera={{ position: [0, 0.4, 5], fov: 32 }}  // long lens = product-shoot look
      gl={{ antialias: true, alpha: true }} // alpha:true → DOM background shows through
      style={{ position: 'fixed', inset: 0, zIndex: -1 }}
    >
      <Suspense fallback={null}>
        <ParallaxRig>
          <Float speed={1.2} rotationIntensity={0.4} floatIntensity={0.6}>
            <HeroObject />
          </Float>
        </ParallaxRig>
        <Environment preset="studio" />
        <ContactShadows position={[0, -1.4, 0]} opacity={0.45} scale={8} blur={2.6} far={3} />
      </Suspense>
    </Canvas>
  )
}
```

## Camera: think like a photographer

- **FOV 25–35** for hero objects. Long lenses compress perspective =
  product-photography look. Wide FOV (60+) reads as gamey/fisheye — only for
  environments you fly through.
- **Never park the camera at eye level dead-center.** Slightly above and
  off-axis (like the skeleton above) is instantly more composed.
- **Move the camera, not the object,** for scroll journeys (Part IX). Move
  the *object* for hover/idle life.
- Depth composition: something near (out-of-focus foreground particle),
  something mid (the subject), something far (background gradient). Three
  layers minimum or the scene reads flat.

## Lighting recipes (this is where the money is)

**Recipe 1 — Studio product (the safe default):**
```jsx
<Environment preset="studio" />           {/* base: soft reflections everywhere */}
<directionalLight position={[3, 4, 2]} intensity={1.2} />          {/* key */}
<directionalLight position={[-4, 2, -3]} intensity={2.5} color="#7c5cff" /> {/* RIM — the hero light */}
```
The **rim light** — strong, colored, from behind-and-beside — is the single
biggest "expensive render" ingredient. It draws a bright edge that separates
the object from the background.

**Recipe 2 — Moody dark (for near-black sites):**
```jsx
<Environment preset="city" environmentIntensity={0.25} />
<spotLight position={[2, 5, 2]} angle={0.35} penumbra={1} intensity={60} castShadow />
<pointLight position={[-3, -1, -2]} intensity={8} color="#2a2a45" />  {/* cool fill from below */}
```
Dark scenes need penumbra 1 (soft spot edges) and a barely-there cool fill so
shadows aren't dead black.

**Recipe 3 — Custom environment glow (no light objects at all):**
Use drei's `<Environment>` with `<Lightformer>` children to place glowing
rectangles in a custom env map — this is how the glossy "studio for free"
looks are done:
```jsx
import { Environment, Lightformer } from '@react-three/drei'

<Environment resolution={256}>
  <Lightformer position={[0, 3, 0]} scale={[10, 1, 1]} intensity={4} />       {/* strip above */}
  <Lightformer position={[-4, 0, 1]} scale={[1, 3, 1]} intensity={2} color="#7c5cff" rotation-y={Math.PI / 2} />
</Environment>
```

**Shadows:** `<ContactShadows>` (blurry ground blob) sells groundedness at
almost no cost and is enough for 90% of sites. Real cast shadows only when
geometry demands it; `<AccumulativeShadows>` for soft static-scene raytraced
look.

## Materials

**The transmission/glass look (the 2024–26 award-site material):**
```jsx
import { MeshTransmissionMaterial } from '@react-three/drei'

<mesh geometry={geometry}>
  <MeshTransmissionMaterial
    thickness={0.6} roughness={0.12} transmission={1}
    ior={1.4} chromaticAberration={0.04} backside
  />
</mesh>
```
Expensive to render — ONE transmission object per scene, and clamp DPR harder
when using it.

**Brushed metal:** `meshStandardMaterial` with `metalness={0.9}`,
`roughness={0.35}`, and a real environment map (metal without an env map
renders black — if your metal looks wrong, it's always the env map).

**Velvet/soft matte:** `meshPhysicalMaterial` with `sheen={1}`,
`sheenColor` near your accent, high roughness.

**Emissive for bloom:** materials with `emissive` color and
`emissiveIntensity={2+}` are what selective bloom (Part VIII) picks up.
`toneMapped={false}` on a `meshBasicMaterial` does the same for UI-ish glows.

## Models

Pipeline: **Blender → glTF (.glb) → compress → gltfjsx**.

```bash
# Compress: meshopt/draco + ktx2 textures + dedupe, in one tool:
npx gltf-transform optimize in.glb out.glb --compress meshopt --texture-compress ktx2
# Generate a typed React component from the model:
npx gltfjsx out.glb --transform --types
```

- Budget: hero model **< 2MB compressed**, < 150k triangles.
- Bake ambient occlusion into a texture in Blender rather than computing it live.
- No model? **Don't fake one.** Icospheres, toruses, and superellipsoids with
  great materials + lighting beat a mediocre downloaded model. Abstract reads
  premium; clip-art 3D reads cheap.

## Idle life

A scene where nothing moves is a screenshot. Give the hero object *breath*:

```jsx
useFrame(({ clock }) => {
  const t = clock.elapsedTime
  mesh.current.position.y = Math.sin(t * 0.6) * 0.08          // slow bob
  mesh.current.rotation.z = Math.sin(t * 0.4) * 0.03          // micro sway
})
```

Or just drei's `<Float>`. Amplitudes tiny (0.05–0.1 units), frequencies slow
(0.3–0.8Hz), and layer two sines at different frequencies so the loop never
reads as a loop.

---

<a name="part-vi"></a>
# PART VI — Shaders: the actual magic

Every distinctive background, every organic gradient, every "how did they do
that" effect is a fragment shader. This is the highest-leverage skill in this
entire guide. You need surprisingly little GLSL.

## Minimal setup (R3F)

```jsx
import * as THREE from 'three'
import { useFrame, useThree } from '@react-three/fiber'
import { useMemo, useRef } from 'react'

function ShaderPlane({ frag, vert }) {
  const mat = useRef()
  const { viewport } = useThree()
  const uniforms = useMemo(() => ({
    uTime:  { value: 0 },
    uMouse: { value: new THREE.Vector2(0.5, 0.5) },
    uRes:   { value: new THREE.Vector2(1, 1) },
  }), [])

  useFrame(({ clock, pointer }, dt) => {
    uniforms.uTime.value = clock.elapsedTime
    // damp the mouse INSIDE the uniform — reactive but weighted (Part IV)
    uniforms.uMouse.value.x = THREE.MathUtils.damp(uniforms.uMouse.value.x, (pointer.x + 1) / 2, 3, dt)
    uniforms.uMouse.value.y = THREE.MathUtils.damp(uniforms.uMouse.value.y, (pointer.y + 1) / 2, 3, dt)
  })

  return (
    <mesh scale={[viewport.width, viewport.height, 1]}>
      <planeGeometry args={[1, 1]} />
      <shaderMaterial ref={mat} uniforms={uniforms} vertexShader={vert} fragmentShader={frag} />
    </mesh>
  )
}

const vert = /* glsl */ `
  varying vec2 vUv;
  void main() {
    vUv = uv;
    gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
  }
`
```

## The noise toolkit (memorize the shape, copy the code)

```glsl
// --- Value noise + fbm: the workhorse. Good enough for backgrounds. ---
float hash(vec2 p) {
  return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123);
}
float noise(vec2 p) {
  vec2 i = floor(p), f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);            // smoothstep the cell
  return mix(mix(hash(i),                hash(i + vec2(1.0, 0.0)), u.x),
             mix(hash(i + vec2(0.0,1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float fbm(vec2 p) {                            // fractal brownian motion: stacked octaves
  float v = 0.0, a = 0.5;
  for (int i = 0; i < 5; i++) {
    v += a * noise(p);
    p *= 2.0; a *= 0.5;
  }
  return v;
}
```

For gradient-quality simplex noise, grab from **lygia.xyz** or the
**glsl-noise** package (`snoise`). But fbm-of-value-noise carries most
backgrounds fine.

**The three moves that generate 90% of shader art:**

1. **fbm** — organic clouds from nothing.
2. **Domain warping** — feed noise its own output: `fbm(p + fbm(p + t))`.
   Instant "living ink" / marble / smoke.
3. **Palette mapping** — map the noise value through 2–3 brand colors with
   `mix()` / smoothstep bands.

## Interactive uniforms — what to wire where

| Input | Uniform | Use |
|---|---|---|
| Cursor (damped, λ≈3) | `uMouse` | warp center, light position, reveal radius |
| Scroll progress 0–1 | `uScroll` | hue shift, warp intensity, camera-adjacent fx |
| Scroll *velocity* (damped) | `uVelocity` | stretch/blur/distort while flicking — settles when still |
| Time | `uTime` | everything drifts; multiply small (0.02–0.1) |
| Entrance progress | `uReveal` | shader-driven page reveals (noise-edged wipes) |

Scroll velocity into a uniform (with Lenis):

```js
lenis.on('scroll', ({ velocity }) => {
  targetVel = velocity            // raw, spiky
})
// in the raf: uniforms.uVelocity.value = damp(uniforms.uVelocity.value, targetVel * 0.01, 5, dt)
```

Velocity-reactive distortion — the background smears slightly when you flick
the page and settles when you stop — is one of the strongest "this site is
alive" signals there is, and it's one uniform.

## Vertex displacement (the liquid blob / terrain move)

```glsl
// vertex shader — displace along normals with 3D-ish noise
uniform float uTime;
uniform float uIntensity;      // wire this to mouse proximity or scroll velocity
varying float vDisp;

void main() {
  float n = fbm(position.xy * 1.5 + uTime * 0.25);   // cheap: 2D noise on 3D pos
  vDisp = n;
  vec3 displaced = position + normal * n * uIntensity;
  gl_Position = projectionMatrix * modelViewMatrix * vec4(displaced, 1.0);
}
```

Use an icosphere with enough segments (`<icosahedronGeometry args={[1, 64]} />`),
displace along normals, color by `vDisp` in the fragment. That's the entire
"liquid orb" genre. Raise `uIntensity` on hover (damped!) and the orb *responds*.

---

<a name="part-vii"></a>
# PART VII — Reactive backgrounds: full recipes

The background carries the atmosphere. All recipes below share the rules:
**dark base, slow drift, damped mouse reaction, grain on top.**

## Recipe 1 — The living gradient (domain-warped fbm)

The most versatile premium background in existence. Full fragment shader:

```glsl
uniform float uTime;
uniform vec2  uMouse;      // damped, 0..1
uniform vec2  uRes;
varying vec2  vUv;

// [hash / noise / fbm from Part VI here]

void main() {
  vec2 p = vUv * 3.0;
  p.x *= uRes.x / uRes.y;                       // aspect-correct

  // cursor leans the field toward itself — weighted, not mirrored
  vec2 m = (uMouse - 0.5) * 1.2;
  p += m * 0.55;

  // domain warp: two nested fbm passes, drifting at different speeds
  vec2 q = vec2(fbm(p + uTime * 0.04),
                fbm(p + vec2(5.2, 1.3) - uTime * 0.03));
  vec2 r = vec2(fbm(p + 4.0 * q + vec2(1.7, 9.2)),
                fbm(p + 4.0 * q + vec2(8.3, 2.8)));
  float f = fbm(p + 4.0 * r);

  // palette: near-black → deep brand → accent bloom (only in the hottest spots)
  vec3 base   = vec3(0.039, 0.039, 0.043);      // matches --bg
  vec3 deep   = vec3(0.10, 0.07, 0.22);
  vec3 accent = vec3(0.486, 0.361, 1.0);        // matches --accent

  vec3 col = mix(base, deep, smoothstep(0.25, 0.75, f));
  col = mix(col, accent, smoothstep(0.68, 0.98, f) * 0.55);

  // vignette — cheap, always worth it
  float vig = smoothstep(1.25, 0.35, length(vUv - 0.5) * 1.6);
  col *= vig;

  // dithering: kills gradient banding on dark backgrounds. Do not skip.
  col += (hash(vUv * uRes + uTime) - 0.5) / 255.0;

  gl_FragColor = vec4(col, 1.0);
}
```

Tuning: drift speeds 0.03–0.06; accent coverage under ~15% of the frame
(smoothstep band high and narrow); mouse influence 0.3–0.7. **Perf:** render
this on a plane at *half resolution* into a render target and upscale, or just
run the whole canvas at `dpr={1}` when the background is the only 3D — warped
fbm at 4 octaves × 5 taps is real GPU work on 4K screens.

## Recipe 2 — Particle field with mouse repulsion

The second signature look: thousands of points, parting around the cursor.

```jsx
function Particles({ count = 6000 }) {
  const mat = useRef()
  const { positions, seeds } = useMemo(() => {
    const positions = new Float32Array(count * 3)
    const seeds = new Float32Array(count)
    for (let i = 0; i < count; i++) {
      positions[i*3]   = (Math.random() - 0.5) * 14
      positions[i*3+1] = (Math.random() - 0.5) * 8
      positions[i*3+2] = (Math.random() - 0.5) * 4
      seeds[i] = Math.random()
    }
    return { positions, seeds }
  }, [count])

  const uniforms = useMemo(() => ({
    uTime:  { value: 0 },
    uMouse: { value: new THREE.Vector3(999, 999, 0) },  // world-space, parked offscreen
  }), [])

  useFrame(({ clock, pointer, viewport }, dt) => {
    uniforms.uTime.value = clock.elapsedTime
    const tx = (pointer.x * viewport.width) / 2
    const ty = (pointer.y * viewport.height) / 2
    uniforms.uMouse.value.x = THREE.MathUtils.damp(uniforms.uMouse.value.x, tx, 5, dt)
    uniforms.uMouse.value.y = THREE.MathUtils.damp(uniforms.uMouse.value.y, ty, 5, dt)
  })

  return (
    <points>
      <bufferGeometry>
        <bufferAttribute attach="attributes-position" args={[positions, 3]} />
        <bufferAttribute attach="attributes-aSeed" args={[seeds, 1]} />
      </bufferGeometry>
      <shaderMaterial
        ref={mat} uniforms={uniforms} transparent depthWrite={false}
        blending={THREE.AdditiveBlending}
        vertexShader={particleVert} fragmentShader={particleFrag}
      />
    </points>
  )
}

const particleVert = /* glsl */ `
  uniform float uTime;
  uniform vec3  uMouse;
  attribute float aSeed;
  varying float vAlpha;

  void main() {
    vec3 pos = position;

    // idle drift — every particle on its own clock
    pos.y += sin(uTime * 0.4 + aSeed * 6.283) * 0.15;
    pos.x += cos(uTime * 0.3 + aSeed * 6.283) * 0.1;

    // mouse repulsion with smooth falloff
    vec2 toMouse = pos.xy - uMouse.xy;
    float dist = length(toMouse);
    float force = smoothstep(2.2, 0.0, dist);          // radius 2.2 world units
    pos.xy += normalize(toMouse + 0.0001) * force * 0.9;

    vec4 mv = modelViewMatrix * vec4(pos, 1.0);
    gl_Position = projectionMatrix * mv;
    gl_PointSize = (2.0 + aSeed * 3.0) * (4.0 / -mv.z); // size by depth
    vAlpha = 0.25 + aSeed * 0.4 + force * 0.5;          // disturbed particles brighten: color = alive
  }
`
const particleFrag = /* glsl */ `
  varying float vAlpha;
  void main() {
    float d = length(gl_PointCoord - 0.5);              // round soft sprite
    float a = smoothstep(0.5, 0.1, d) * vAlpha;
    gl_FragColor = vec4(vec3(0.75, 0.72, 1.0), a);
  }
`
```

The two lines that make it feel expensive: **damped mouse** (the field reacts
with weight) and **force-brightening** (disturbed particles light up —
color = alive). 6k particles in one draw call is nothing; 50k is fine on
desktop.

## Recipe 3 — Wave / dot grid

Ordered grid of points, Z-displaced by traveling noise, mouse adds a local
swell. Same skeleton as Recipe 2 with grid positions:

```js
// grid positions instead of random:
const nx = 120, ny = 70
for (let i = 0, y = 0; y < ny; y++)
  for (let x = 0; x < nx; x++, i++) {
    positions[i*3]   = (x / nx - 0.5) * 16
    positions[i*3+1] = (y / ny - 0.5) * 9
    positions[i*3+2] = 0
  }
```
```glsl
// vertex: traveling wave + mouse swell
pos.z  = noise(pos.xy * 0.35 + uTime * 0.15) * 1.2;
pos.z += smoothstep(2.0, 0.0, length(pos.xy - uMouse.xy)) * 0.9;
```

Tilt the whole grid back (`rotation-x={-0.9}`), place below the fold line, and
you have the classic "topographic ocean" hero floor.

## Recipe 4 — Cursor trail / fluid feel (the flowmap technique)

The true fluid-simulation look (ink following the cursor, distorting the
scene) uses a **ping-pong render target**: draw the cursor as a soft splat
into a texture each frame, fade the previous frame slightly, then use that
texture as a distortion/reveal map in your main shader.

Don't hand-roll it first time: use the tiny **`@funtech-inc/use-shader-fx`**
package (R3F hooks for flowmap/fluid/noise) or port Codrops' flowmap demos.
Understand the architecture, ship the library. Budget: one 512×512 RT is
cheap; a full Navier–Stokes fluid sim (stable fluids) is heavier — desktop
only, feature-flag it.

## Recipe 5 — CSS-only luxury (when WebGL is overkill)

For content-heavy pages that still need atmosphere:

```css
.bg {
  position: fixed; inset: 0; z-index: -1;
  background: #0a0a0b; overflow: hidden;
}
.blob {
  position: absolute; width: 55vmax; height: 55vmax; border-radius: 50%;
  filter: blur(90px); opacity: 0.5; will-change: transform;
  animation: drift 26s ease-in-out infinite alternate;
}
.blob--1 { background: #2a1a6e; top: -15%; left: -10%; }
.blob--2 { background: #12333a; bottom: -20%; right: -5%; animation-duration: 34s; animation-delay: -8s; }
@keyframes drift {
  from { transform: translate(0, 0) scale(1); }
  to   { transform: translate(12vw, -8vh) scale(1.15); }
}
```

Two or three huge blurred blobs, drifting on 25–35s alternating loops, under
the grain overlay from Part III. Add a JS one-liner that damps a
`translate` on the blob container toward the cursor and even this reacts.
This is also your `prefers-reduced-motion` / no-WebGL fallback (Part XIII).

---

<a name="part-viii"></a>
# PART VIII — Postprocessing

```jsx
import { EffectComposer, Bloom, Noise, Vignette, ChromaticAberration } from '@react-three/postprocessing'

<EffectComposer>
  <Bloom mipmapBlur intensity={0.5} luminanceThreshold={1.0} />
  <Noise opacity={0.02} />
  <Vignette darkness={0.55} offset={0.3} />
</EffectComposer>
```

**The budget (per effect, and in total — less than you think):**

| Effect | Setting | Rule |
|---|---|---|
| Bloom | threshold **1.0**, intensity 0.3–0.7, `mipmapBlur` | Threshold 1.0 = only HDR/emissive things glow. Whole-scene bloom (low threshold) is the #1 postprocessing tell. |
| Noise/grain | opacity 0.015–0.03 | Alternative to the CSS grain — use one, not both. |
| Vignette | darkness 0.4–0.7 | Always safe. Focuses the frame. |
| Chromatic aberration | offset ≤ 0.0015, or velocity-driven | At rest: barely at edges. Better: wire to scroll velocity so it only smears during motion. |
| Depth of field | desktop only | Expensive; only when a near/far layer story exists. |
| Tone mapping | ACES (three default) | Leave it on; it's why highlights roll off filmically. |

The test from Part I: if a visitor can *name* an effect, halve it. Total
postprocessing GPU cost should stay under ~2ms; `mipmapBlur` bloom + noise +
vignette fits easily.

---

<a name="part-ix"></a>
# PART IX — The scroll experience

## Lenis + ScrollTrigger (the canonical wiring)

```js
import Lenis from 'lenis'
import gsap from 'gsap'
import ScrollTrigger from 'gsap/ScrollTrigger'
gsap.registerPlugin(ScrollTrigger)

const lenis = new Lenis({
  duration: 1.1,                                   // 1.0–1.2. More = seasick.
  easing: (t) => Math.min(1, 1.001 - Math.pow(2, -10 * t)),
})
lenis.on('scroll', ScrollTrigger.update)
gsap.ticker.add((time) => lenis.raf(time * 1000))  // ONE ticker drives everything
gsap.ticker.lagSmoothing(0)
```

Lenis makes wheel scrolling glide (native scrollbar preserved, position:
sticky works). It is the single cheapest "expensive feel" upgrade for any
site. Do not stack it with CSS `scroll-behavior: smooth`.

## Scroll-linked 3D: camera on a rail

The flagship pattern — scroll drives the camera through the scene. Content
sections align with camera stations.

```jsx
import { CatmullRomCurve3, Vector3 } from 'three'
import { useFrame, useThree } from '@react-three/fiber'

const path = new CatmullRomCurve3([
  new Vector3(0, 0.5, 6),
  new Vector3(2.5, 1.2, 3),
  new Vector3(-1.5, 0.6, 1.5),
  new Vector3(0, 2.2, -2),
], false, 'catmullrom', 0.5)

function ScrollCamera({ progressRef }) {   // progressRef.current = damped 0..1 scroll
  const look = useMemo(() => new Vector3(), [])
  useFrame(({ camera }, dt) => {
    const t = progressRef.current
    const pos = path.getPointAt(Math.min(t, 0.999))
    camera.position.copy(pos)
    look.copy(path.getPointAt(Math.min(t + 0.04, 1)))   // look slightly ahead on the rail
    camera.lookAt(look)
  })
  return null
}

// feed it: lenis.on('scroll', ({ progress }) => { target = progress })
// and in a raf: progressRef.current = damp(progressRef.current, target, 4, dt)
```

**Damp the scroll progress before it touches the camera** (λ≈3–5). Raw scroll
into camera position feels like driving on rumble strips; damped feels like a
crane shot. Add the Part IV parallax rig *on top* (parent group) so the cursor
still tilts the view mid-journey.

## Pinned sections

```js
ScrollTrigger.create({
  trigger: '.chapter',
  start: 'top top',
  end: '+=250%',                 // pin for 2.5 viewport-heights of scroll
  pin: true,
  scrub: true,
  animation: gsap.timeline()
    .fromTo('.chapter .visual', { scale: 0.9 }, { scale: 1 })
    .fromTo('.chapter .copy-1', { opacity: 0 }, { opacity: 1 }, 0.1)
    .to('.chapter .copy-1',     { opacity: 0 }, 0.45)
    .fromTo('.chapter .copy-2', { opacity: 0 }, { opacity: 1 }, 0.55),
})
```

Rules for scrub animations: the user's finger is the playhead, so use LINEAR
distribution inside scrubbed timelines (easing fights the scrubbing), and
`scrub: true` (or `scrub: 0.5` for slight lag — often nicer). Pin at most 2–3
sections per page; a fully-pinned site stops feeling like a site.

## Scroll don'ts

- **Never hijack the wheel delta** (full-page-per-tick sliders). Lenis smooths;
  hijacking replaces. Users feel the difference as loss of control.
- Parallax offsets beyond ~15% of element height read as broken layout.
- Don't scrub *reveals* (opacity that tracks scroll both directions on body
  copy). Reveals fire once; scrub is for pinned set-pieces.
- Test with a mouse wheel, a trackpad, AND touch. Lenis handles the split, but
  your scrub distances (`end: '+=250%'`) feel different per device — tune on
  trackpad, verify on wheel.

---

<a name="part-x"></a>
# PART X — Micro-interactions

## Magnetic buttons

```js
function magnetize(el, strength = 0.35) {
  const xTo = gsap.quickTo(el, 'x', { duration: 0.8, ease: 'power3.out' })
  const yTo = gsap.quickTo(el, 'y', { duration: 0.8, ease: 'power3.out' })

  el.addEventListener('mousemove', (e) => {
    const r = el.getBoundingClientRect()
    xTo((e.clientX - r.left - r.width / 2) * strength)
    yTo((e.clientY - r.top - r.height / 2) * strength)
  })
  el.addEventListener('mouseleave', () => {
    gsap.to(el, { x: 0, y: 0, duration: 0.9, ease: 'elastic.out(1, 0.35)' })
  })
}
```

`quickTo` (not new tweens per event) for mousemove-driven values. Strength
0.2–0.4; the *inner label* can move at 1.5× the shell's strength for a layered
pull. The elastic settle-back is the charm — don't skip it.

## Custom cursor (lagged ring)

```js
const ring = document.querySelector('.cursor-ring')
let x = 0, y = 0, tx = 0, ty = 0
window.addEventListener('mousemove', (e) => { tx = e.clientX; ty = e.clientY })

gsap.ticker.add((_, dtMs) => {
  const dt = dtMs / 1000
  const k = 1 - Math.exp(-10 * dt)          // λ=10: responsive but weighted
  x += (tx - x) * k
  y += (ty - y) * k
  ring.style.transform = `translate(${x}px, ${y}px) translate(-50%, -50%)`
})
```

- Keep the native cursor OR a 4px dot at true position; the ring lags behind.
  Hiding the native cursor with a laggy replacement makes precise clicking
  feel broken.
- Grow the ring / morph to a label ("VIEW", "DRAG") over interactive elements
  — wire via `mouseenter` on `[data-cursor]` elements.
- Hide entirely on touch devices: `@media (hover: none) { .cursor-ring { display: none } }`.

## Link & hover states

Every interactive element needs a *designed* hover — never browser default:

```css
/* The underline that draws itself — exit reverses direction */
.link { position: relative; }
.link::after {
  content: ''; position: absolute; left: 0; bottom: -2px;
  width: 100%; height: 1px; background: currentColor;
  transform: scaleX(0); transform-origin: right;
  transition: transform 0.45s var(--ease-out-expo);
}
.link:hover::after { transform: scaleX(1); transform-origin: left; }
```

Other staples: text that rolls up to a duplicate (the `overflow: hidden` +
translateY pair); images that `scale(1.06)` inside a fixed mask over 0.8s;
cards that tilt 2–3° toward the cursor (damped). Hover feedback in under
150ms, settle over 400–800ms — react fast, relax slow.

## Image distortion on hover (the shader hover)

Plane with the image texture; a damped `uHover` uniform drives UV bulge + RGB
split:

```glsl
uniform sampler2D uTexture;
uniform float uHover;          // damped 0..1
uniform vec2  uMouse;          // in UV space of the plane
varying vec2  vUv;

void main() {
  float dist = length(vUv - uMouse);
  float bulge = smoothstep(0.45, 0.0, dist) * uHover * 0.08;
  vec2 uv = vUv + normalize(vUv - uMouse + 0.0001) * -bulge;   // pull toward cursor
  float split = bulge * 0.6;
  vec3 col = vec3(
    texture2D(uTexture, uv + vec2(split, 0.0)).r,
    texture2D(uTexture, uv).g,
    texture2D(uTexture, uv - vec2(split, 0.0)).b
  );
  gl_FragColor = vec4(col, 1.0);
}
```

For DOM-synced WebGL images (the canvas plane exactly covering an `<img>`),
drei's `<Image>` + `<View>` components do the position-syncing for you.

## Sound (optional, high-risk/high-reward)

Muted by default with a visible toggle, ≤ -18dB, hover ticks only on primary
nav. Done right (rare) it's unforgettable; done wrong it's instant-close.

---

<a name="part-xi"></a>
# PART XI — Loading & first impression

The preloader isn't a spinner — it's Act 1 of the choreography.

**The sequence that feels premium:**
1. Instant: solid `--bg` (inline critical CSS, correct `background-color` on
   `<html>` — never a white flash into a dark site).
2. Preloader: wordmark + a real progress number, monospace, small. Progress
   from actual asset loading — drei's `useProgress`:
   ```jsx
   const { progress } = useProgress()   // 0–100 from THREE.DefaultLoadingManager
   ```
   Ease the *displayed* number toward the real one (damped) so it never jumps
   backward or stalls visibly.
3. Exit: preloader wipes away (clip-path inset or translateY, 0.8s expo.inOut)
   **into** the hero entrance timeline — one continuous motion, no dead frame
   between preloader-gone and hero-arriving. This seam is where cheap sites die.
4. First frame of 3D is *already warm*: mount the canvas behind the preloader
   and render a few frames during load (compile shaders, upload textures) so
   the reveal doesn't stutter. `gl.compile()` / drei `<Preload all />` handles this.

**Font strategy:** preload the display woff2, `font-display: swap`, and set
`size-adjust` on the fallback so the swap doesn't reflow the hero:

```html
<link rel="preload" href="/fonts/display.woff2" as="font" type="font/woff2" crossorigin>
```

**Skip the preloader entirely** if total payload < ~1.5MB — a fast site that
choreographs its entrance beats any loading screen. Preloaders are for genuine
3D payloads, and even then: cap at ~3 seconds of patience.

---

<a name="part-xii"></a>
# PART XII — Performance

## The budgets

| Metric | Target |
|---|---|
| Frame rate | 60fps sustained, zero long tasks during scroll |
| Draw calls | < 100 (check `gl.info.render.calls`) |
| Triangles | < 500k desktop, < 100k mobile |
| JS (gzipped) | < 350KB total; three+r3f+drei ≈ 150–200KB of it |
| Hero model | < 2MB (meshopt/draco + KTX2) |
| Texture sizes | 1024² default, 2048² only the hero, KTX2/basis compressed |
| DPR | clamp: `dpr={[1, 2]}`; drop to 1.5 with transmission/DOF |
| LCP | < 2.5s — the DOM hero text is your LCP, never the canvas |

## The big levers

**1. DPR is quadratic.** DPR 3 → 2 halves+ the pixels shaded. Nobody sees the
difference on a moving gradient. Clamp always; consider drei
`<PerformanceMonitor>` to drop DPR adaptively when frames slip:

```jsx
<PerformanceMonitor onDecline={() => setDpr(1)} onIncline={() => setDpr(Math.min(window.devicePixelRatio, 2))}>
```

**2. On-demand rendering.** If the scene only moves on input/scroll, don't
render at 60fps while idle:

```jsx
<Canvas frameloop="demand">   {/* renders only when invalidate() is called */}
```
Continuous backgrounds can't use this, but product-viewer pages should.
Also: pause the loop entirely when the canvas scrolls out of view
(IntersectionObserver → `frameloop="never"` / stop the ticker) and on
`document.hidden`.

**3. Instancing.** Same geometry × many copies = `<Instances>` (drei) or
`InstancedMesh`. 1 draw call for 10,000 objects. Particles as `<points>` are
already one call.

**4. Compress everything.** `gltf-transform optimize` (Part V) is one command
for meshopt + KTX2. KTX2 textures stay compressed *in GPU memory* — this is
the difference between a phone browser surviving your site or not.

**5. One ticker.** GSAP's ticker drives Lenis (Part IX); R3F has its own loop.
Don't add more rafs — every independent raf is a chance to miss the frame.
Cursor/scroll handlers write to *targets*; damping happens in the loop
(passive listeners, no work in the event).

## Mobile strategy

Decide per-project, explicitly — never "the same site but slower":

- **Reduce** (default): same concept, half the particles, no transmission
  material, no DOF/chromatic aberration, DPR 1–1.5, simpler fbm (3 octaves).
- **Replace**: swap the WebGL background for the CSS blob recipe (Part VII #5);
  keep all DOM motion. Often *feels* better than a degraded 3D scene.
- Battery courtesy: cap continuous backgrounds at 30fps on mobile (render
  every other frame) — invisible on a drifting gradient.

Detect capability, not just width: `navigator.hardwareConcurrency`,
`devicePixelRatio`, a WebGL context probe, and react to
`PerformanceMonitor` rather than user-agent sniffing.

---

<a name="part-xiii"></a>
# PART XIII — Accessibility & graceful degradation

Award sites are notorious a11y disasters. Being the exception costs one day:

**1. `prefers-reduced-motion` is non-negotiable.**
```js
const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches
```
- Continuous background motion → static (or near-static: one slow 60s drift)
- Scroll choreography → simple fades or nothing (content just present)
- Lenis → don't instantiate; parallax/magnetic/cursor fx → off
- GSAP: `gsap.matchMedia()` lets you author both branches cleanly.

Build the reduced branch as a *design*, not an apology — it's the CSS-blob
background + instant content + your excellent typography. Still expensive-looking.

**2. WebGL failure is a real path.** Old GPUs, blocklisted drivers, WebGL
disabled. Probe for a context; on failure mount the CSS background. The site
must remain 100% usable — 3D is *atmosphere*, never the information.

**3. The canvas is decoration to assistive tech:** `aria-hidden="true"` on the
canvas wrapper; all real content is real DOM text (this also gives you SEO and
selectable text for free — another reason never to render copy inside WebGL).

**4. Keyboard:** visible `:focus-visible` styles (your accent color earns its
keep), logical tab order, skip-link. Custom cursors must not suppress focus
outlines.

**5. Contrast:** body text ≥ 4.5:1 *against the darkest and lightest states of
the animated background it sits on*. If the gradient drifts bright under your
text, add a local scrim (radial gradient behind the text block) — invisible,
and it fixes both a11y and legibility.

---

<a name="part-xiv"></a>
# PART XIV — The pre-ship checklist

**The feel**
- [ ] One-sentence visual concept; everything serves it
- [ ] Every duration/easing from the system (Part IV) — zero defaults
- [ ] All continuous motion damped & frame-rate independent (no naive lerp)
- [ ] Background answers cursor AND scroll velocity, weighted not mirrored
- [ ] Exactly one thing "most alive" at any moment; settled UI near-monochrome
- [ ] Grain, hairlines, mono labels, custom `::selection` — detail pass done

**The type & color**
- [ ] Display line-height < 1.1, negative tracking; body ≤ 70ch
- [ ] Palette: one family + one signal hue; no pure #000/#fff
- [ ] Fonts preloaded; no FOUT reflow of the hero

**The 3D**
- [ ] Rim light present; env map on all metallic/glossy materials
- [ ] Contact shadow grounds the hero object
- [ ] Idle breath (Float/sines) — nothing statically frozen
- [ ] FOV ≤ 35 for object heroes; camera composed off-axis
- [ ] Bloom threshold 1.0; no effect a visitor could name

**The plumbing**
- [ ] 60fps on a mid phone; draw calls < 100; DPR clamped
- [ ] gltf-transform'd models, KTX2 textures; JS < 350KB gz
- [ ] Loop pauses off-screen & on hidden tab; one ticker total
- [ ] Preloader → hero is one continuous timeline; shaders precompiled
- [ ] LCP < 2.5s (DOM text), no CLS from canvas mount or font swap

**The floor**
- [ ] `prefers-reduced-motion` branch designed, not disabled
- [ ] No-WebGL fallback mounts CSS background; site fully usable
- [ ] Canvas `aria-hidden`; all content real DOM; keyboard clean; contrast checked on animated backgrounds
- [ ] Tested: wheel + trackpad + touch, Safari included (it *will* differ)

---

<a name="part-xv"></a>
# PART XV — Study list

**Learn (in order):**
1. **Three.js Journey** (Bruno Simon) — the canonical course; the shader and R3F chapters alone justify it
2. **The Book of Shaders** (thebookofshaders.com) — free; read up through noise
3. **Maxime Heckel's blog** (blog.maximeheckel.com) — the best long-form writing on R3F/shader techniques on the internet
4. **Codrops** (tympanus.net/codrops) — deconstructions of real award-site effects, with source
5. **lygia.xyz** — production-grade GLSL utility library (noise, sdf, color)
6. **Shadertoy** — read shaders, steal moves (mind the licenses; rewrite, don't paste)

**Study these studios** (view-source of taste):
- **Lusion** (lusion.co) — the reactive-background masters
- **Active Theory** (activetheory.net) — canvas-first experiences
- **Locomotive** (locomotive.ca) — scroll craft
- **14islands**, **Obys**, **Studio Freight/Darkroom** (they built Lenis; their own sites demo it best)
- **Bruno Simon's portfolio** (bruno-simon.com) — the famous one

**Daily taste diet:** Awwwards SOTD, Godly (godly.website), Minimal Gallery.
Don't just look — for each site you like, name the *one idea* (Part I, law 1)
and find the damped value (there's always at least one).

---

*Written by Fable for the Phoenix workspace, 2026-07-07. The whole guide in one
line: pick one idea, damp everything, light it like a photographer, ease out
like you mean it, and spend the saved GPU budget on restraint.*
