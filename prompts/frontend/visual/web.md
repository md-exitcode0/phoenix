# Web: layout, type, rhythm

The brand and brief come from `taste/SKILL.md`. This file is the construction
knowledge that makes a page look built by a senior designer.

## Pick the layout from the job, not from habit

Different categories have different DNA. Choose deliberately, and make three
sites in three categories look like three different studios made them.

| Category | Layout DNA |
| --- | --- |
| Editorial / magazine | Asymmetric multi-column grid, big serif display type, strong typographic hierarchy, generous gutters, pull quotes, image captions, bylines. |
| Product / SaaS | Clear promise + product visual above the fold, proof (logos, numbers), feature sections with real UI, pricing table, dense but calm. |
| Portfolio / studio | Work first: large imagery, minimal chrome, unusual navigation allowed, case-study pages with narrative. |
| Local business / hospitality | Photography of the real place, hours/location/booking reachable in one tap, warmth over cleverness. |
| E-commerce | Product imagery, price and add-to-cart always obvious, filters, trust signals, fast scanning grid. |
| Dashboard / tool | Information density, alignment, tables, restraint in color (color = meaning), keyboard-friendly. |
| Event / campaign | One bold idea, big type, date and call to action unmissable, motion allowed. |
| Documentation | Left nav, readable measure (60–75 characters), code blocks, search, anchors. |

Avoid the generic stack of centered hero → three feature cards → testimonial →
CTA band unless the job truly calls for it.

## Grid and spacing

- A 12-column grid at desktop (max content width 1120–1320 px), 4–6 columns on
  tablet, 4 on mobile, 16–24 px side gutters on phones.
- One spacing scale (4 or 8 px base): 4, 8, 12, 16, 24, 32, 48, 64, 96, 128.
  Section padding 96–160 px desktop, 56–80 px mobile. Never invent in-between values.
- Related things close, unrelated things far (proximity creates grouping).
  Vertical rhythm: space above a heading > space below it.
- Break the grid on purpose (a bleeding image, an overlapping card) once or
  twice per page, not everywhere.

## Typography

- Two families max (display + text), or one superfamily. Scale ratio 1.2–1.333
  for UI, 1.414–1.618 for editorial display.
- Body 16–18 px (editorial 18–21), line-height 1.5–1.7, measure 60–75 characters.
  Display line-height 0.95–1.15, tighter letter-spacing (−1% to −3%) at large
  sizes; uppercase labels get +4% to +10% tracking.
- Hierarchy through size and weight contrast, not through color soup. Three to four
  text levels are usually enough.
- Use real content. Lorem ipsum hides layout problems. Write plausible, specific copy.

## Color

- Build a palette from one or two brand hues plus neutrals with a slight hue tint
  (never pure gray #808080 everywhere). Define tokens: bg, surface, text,
  muted, border, accent, accent-contrast, success/warn/danger.
- Contrast: body text ≥ 4.5:1, large text ≥ 3:1. Check it.
- Accent color is scarce: roughly 5–10% of the surface. When everything is highlighted,
  nothing is.

## Imagery

- Real, specific photography or purpose-made illustration beats stock clichés.
  If the page needs images, ask a coworker who can find or generate them with the
  exact subject, mood, aspect ratio and count; don't settle for placeholders.
- Consistent treatment across a page: same aspect ratios family, same color grade,
  same corner radius.
- Always set width/height or aspect-ratio (no layout shift), use `object-fit: cover`,
  modern formats, `loading="lazy"` below the fold.

## Details that signal quality

- Hover/focus/active states on every interactive element; visible focus ring.
- Buttons: 40–48 px tall, horizontal padding ≈ 2× vertical, one primary per view.
- Icons from one set with one stroke weight (see `visual/svg.md`).
- Subtle motion with purpose: 150–250 ms UI transitions, ease-out for entering,
  ease-in for leaving; respect `prefers-reduced-motion`.
- Responsive is a redesign at each size, not a squeeze: reorder, resize type,
  collapse navigation, keep tap targets ≥ 44 px.

## Verify in a real browser

Screenshot at 1440×900 and 390×844, full page and above the fold. Check: first
glance reads correctly, nothing overflows horizontally, no text collisions,
images load, contrast holds, spacing follows the scale. Then run the
`visual/looking.md` protocol.
