# SVG: icons, characters, illustrations

## Setup

- Design on a grid. Icons: `viewBox="0 0 24 24"` with 2 px padding (live area
  20×20). Avatars and characters: `viewBox="0 0 64 64"` or `0 0 128 128`.
  Integer or half-pixel coordinates only for strokes of odd width, so lines
  land on the pixel grid.
- One stroke width per set (1.5 or 2 at 24 px), `stroke-linecap="round"`,
  `stroke-linejoin="round"`, `fill="none"` for line icons. One corner radius
  family (e.g. 2 and 4).
- Use `currentColor` so icons inherit color; expose a small set of CSS
  variables for multi-tone art instead of hard-coded hex everywhere.
- Hand-write paths with simple commands: `M L H V A C Z`. Prefer geometric
  primitives (`circle`, `rect rx`, `ellipse`, `line`) when they express the
  shape. Arcs (`A rx ry rot large sweep x y`) are the easiest way to get clean
  curves; cubic `C` for organic ones, with handles about 1/3 of the segment length.

## Icons

- Each icon = one clear idea, 2–5 shapes. If you need more, the metaphor is wrong.
- Optical balance beats mathematical centering: circles are drawn slightly
  larger than squares to look the same size (≈ 20 vs 18 in a 24 grid);
  triangles/play icons shift right ~1 px.
- Test at 16, 20, 24 and 32 px on light and dark backgrounds. If two icons in
  the set look like different families (weight, corner, detail), fix them.

## Characters, avatars and faces

Faces are where generated SVG looks worst. Rules that fix almost all of it:

- **Simple geometric head**: a circle, squircle or rounded rect. No complex
  jaw paths. Put the face in the lower half-to-60% of the head (eyes sit near
  the vertical middle of the head, not in the upper third).
- **Eyes**: two small solid dots or short vertical ovals (≈ 6–9% of head width),
  spaced about one eye-width-and-a-half apart, identical in size, perfectly
  level. Optional single tiny white highlight in the same position on both.
  No irises-with-pupils-with-lashes at avatar size; that is what makes faces
  look uncanny or "ramen-like".
- **Expression lives in 2–3 marks**: eye shape (dot / arc ◠ for happy / line
  for calm) plus mouth (short arc, small open oval, flat line) plus optional
  brows (short straight or slightly angled strokes). Keep mouth width ≈ the
  distance between the eyes.
- **Stroke weight** of facial marks matches the set's stroke width; round caps.
- **Asymmetry only on purpose** (a wink, a raised brow). Everything else is
  mirrored exactly; compute the right side from the left (x' = W − x).
- **Distinct variants through big shapes and color**, not detail: head shape,
  hair silhouette (a single bold shape), accessory (glasses, hat, headphones),
  and a palette per character. Many options, each simple.
- Palette: one skin/base tone, one hair/accent, one background; 2–3 colors
  per avatar. Harmonize the set (same saturation and lightness band).
- Check the set side by side at 32 px and 64 px. If you can't tell them apart at 32 px,
  the silhouettes are too similar. If any looks creepy, simplify the eyes first.

## Illustrations

- Build from flat shapes in 3 value layers (background, midground, subject).
  Add one shadow shape and one highlight shape per major form, not gradients
  everywhere.
- Limit the palette to 4–6 colors plus tints. Reuse them.
- Use `<g>` with transforms for repeated parts, `<use>` for reuse, `<clipPath>`
  for masking highlights into a shape.

## Quality checks

- Open it rendered (browser screenshot or `rsvg-convert -w 256 in.svg -o out.png`),
  at the real display size and at 4×.
- Validate: no stray points, no hairline gaps between shapes that should touch
  (overlap by 0.5 instead), no transform soup, no huge decimal precision
  (round to 2 decimals).
- Keep files small: an avatar should be well under 2 KB.
