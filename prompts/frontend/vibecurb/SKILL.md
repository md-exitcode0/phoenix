# VibeCurb — strict anti-mean protocols (router)

Vendored from https://github.com/Yu-369/VibeCurb (MIT, Yu-369, 2026). Thesis:
"AI defaults to the mean. Models are trained on millions of average websites.
If you don't constrain them, they will build you an average website. VibeCurb
is the constraint."

Five STRICT pipelines. Each replaces guesswork with the same order: design
read/extraction → quality gate (no code until the extraction passes) → precise
build → visual diff (PASS/FAIL tables) → drift rejection (generic defaults,
CSS keyword easings, AI-purple gradients, placeholder patterns get bounced).

LOAD DISCIPLINE — load exactly ONE per task, whole, and follow it as the
governing pipeline for that task (its gates override the default flow). Do not
preload all five; pick by trigger:

- `vibecurb/pixel-perfect.md` (35K) — a screenshot/mockup/Figma export IS the
  spec; replicate it in code. You are a translator, not a designer: every
  visual decision comes from the image.
- `vibecurb/visual-redesign.md` (48K) — existing WORKING code that looks bad.
  Surgical aesthetic upgrade: rebuild the visual layer, never break state,
  handlers, API calls, or business logic.
- `vibecurb/hero.md` (26K) — hero / above-the-fold / landing header ONLY.
  Does not govern full pages.
- `vibecurb/motion.md` (114K) — animations, transitions, micro-interactions,
  scroll effects, kinetic type, hover physics. Load only for a motion pass.
- `vibecurb/imagegen.md` (40K) — art-directing image_gen when producing
  design REFERENCE comps (structured website-section images a developer can
  code from). Produces no code; feeds pixel-perfect/hero/motion.
