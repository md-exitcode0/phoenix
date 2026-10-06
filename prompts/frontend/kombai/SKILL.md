# Kombai Skills Pack — Distilled Playbook for Iris
**Date:** 2026-07-17 (UTC)
**Purpose:** A compact, machine-usable pack of Kombai's design principles, skill-like instructions, and redesign playbook that Iris can load to build websites and UI at Kombai's quality bar.
**Source:** Distilled from 49 docs.kombai.com pages + kombai.com main site (see kombai-docs-full-extract-2026-07-17.md and kombai-methodology-synthesis-2026-07-17.md for full citations)

---

## 1. THE DESIGN LOOP (always follow this order)

1. **Design first, code second.** Never jump straight to code without a visual reference. Describe the UI in natural language, generate a visual design, iterate on it visually, THEN generate code from the approved design.
2. **Iterate visually.** Use element selection, CSS editing, and snipping to refine the design before generating any code. Catch spacing/alignment issues that are invisible in code-only workflows.
3. **Preview in browser.** Code is previewed in a real browser. DOM elements are selectable, CSS is editable visually, text is editable live. Console errors and performance metrics feed back to the agent.
4. **Match the mode to the task.** Plan (complex features) → Design (new UI) → Code (after approval) → Debug (fix issues) → Ask (understand codebase). Never skip planning for complex work.

---

## 2. DESIGN SYSTEM HIERARCHY (3 tiers — use the right one)

### Tier 1: THEME (token-level, most precise)
Use when your design system is settled and every token must be locked down.
- **Structure:** Theme name, Fonts, Typography, Primary Color, Secondary Color, Neutrals, Errors, Backgrounds, Borders, Shadow, Spacing & Radius
- **How to create:** Extract from an existing website, extract from a design, or generate via chat
- **Key property:** Token changes propagate automatically to every applied design
- **Rule for Iris:** Before designing anything new, define a Theme. Store as CSS custom properties or a design token file. This is the foundation — everything inherits from it.

### Tier 2: STYLE GUIDE (lightweight, visual identity)
Use when speed matters more than precision, or when exploring visual directions.
- **Structure:** Name, Colors (primary/secondary/tertiary/neutral + hex), Typography (Headline/Body/Label + font + size), Spacing, Radius, Additional Instructions (freeform text for LLM)
- **How to create:** Build from scratch, import from a design, or import from a URL
- **Key property:** Regenerates the design to match parameters while preserving layout and content
- **Rule for Iris:** When you don't have time for a full Theme, start with a Style Guide. Define: a seed color, a palette, typography pairing, corner radius preference, and 3-5 design instructions (e.g., "generous whitespace," "dark theme with one accent," "no gradients on text").

### Tier 3: BLOCK (reusable UI patterns)
Use to reuse specific UI structures or layout patterns.
- **Structure:** Entire design or section stored for reuse — navigation bars, card layouts, hero sections, pricing tables
- **Rule for Iris:** Once you've designed a good component, save it as a reusable Block. Build a Block library so future designs remix existing patterns instead of starting from scratch. A Block should be self-contained — its own layout, styles, and content structure.

---

## 3. VARIANTS AND PASSES (never generate just one)

- **Variants** = how many independent design options to generate from one prompt. Each takes a genuinely different visual direction (not just color swaps — different layout structures, density, visual hierarchy).
- **Passes** = how thoroughly each design is refined. More passes = more refinement iterations.
- **Default:** 1 Variant with 3 refinement passes.
- **Rule for Iris:** Never generate one design and call it done. Generate 2-3 variants with genuinely different visual directions. Compare side by side. Pick the strongest, then run 2-3 refinement passes:
  - Pass 1: Fix obvious issues (overflow, alignment, spacing)
  - Pass 2: Polish details (hover states, transitions, micro-interactions)
  - Pass 3: Consistency check (does it match the Theme/Style Guide?)

---

## 4. THE 7-AXIS DESIGN REVIEW (every design must pass all 7)

1. **Visual Design:** Color palettes, typography scales, spacing tokens — does the implementation match the aesthetic intent?
2. **UX/Usability:** Navigation patterns, information hierarchy — are user flows logical and intuitive?
3. **Responsive/Mobile:** Layout adaptation across breakpoints — are mobile layouts optimized? Are touch targets accessible?
4. **Accessibility:** WCAG compliance — contrast ratios, ARIA attributes, keyboard navigation, visible focus states.
5. **Micro-interactions/Motion:** Hover states, animations, transitions — are they present and fluid?
6. **Consistency:** Design system adherence — are there deviations in design usage across pages?
7. **Performance:** Rendering impact — bundle size issues, optimization opportunities.

**Rule for Iris:** Every design must pass all 7 checks before being considered done. Create a checklist. If any axis fails, fix it before shipping. This is non-negotiable — it's what separates Kombai-quality output from AI slop.

---

## 5. WIREFRAME METHODOLOGY (ask before you design)

Before generating any wireframe, ask these 5 clarifying questions:
1. Primary goal of the page
2. Layout structure
3. Content density
4. Navigation patterns
5. Responsiveness priority (Mobile-first vs. Desktop-first)

Then generate **3 wireframe options** with genuinely different layout approaches (structural variants, not color variants).

Wireframe rules:
- Grayscale, low-fidelity, sketch-like style (focus on structure, not styling)
- Label components with [REUSE] (exists in codebase) or [NEW] badges
- Use **realistic placeholder content** — actual table headers, form labels, card descriptions that reflect what the real UI would display. NOT "Lorem ipsum."

**Rule for Iris:** Never start designing without answering these 5 questions first. Generate 3 structural variants. Use realistic content. Label which components can be reused vs. which are new.

---

## 6. THE REDESIGN PLAYBOOK (Improve UI — 6 stages)

This is Kombai's guided process for upgrading existing UI. Adapt depth to scope:
- Small specific change → apply directly
- Focused improvement → refine within existing design language
- Open-ended redesign → full 6-stage process

### Stage 1: Look at the real UI first
Never work from an imagined interface. Use a screenshot, a running URL, or run the app and view it directly. If the app can't be run, reconstruct the UI from the codebase and confirm the reconstruction with the user.

### Stage 2: Understand current state
Review ALL pages and ALL states (empty, loading, error, filled, edge cases). Extract the existing design system (colors, typography, spacing, radii, shadows). Map the information architecture and navigation.

### Stage 3: Diagnose and confirm
Produce a problem list of concrete issues tied to specific screens and components. Share it for confirmation before proposing anything. This is where you lock in hard constraints (fixed palette, dark mode requirement, reference app to take cues from).

### Stage 4: Propose directions
- **For a redesign:** Present 2-3 complete design directions. Each is a representative screen with its own palette, typography, layout/navigation, and density — using the SAME sample data so they're easy to compare. One is marked as recommended. The user can pick one, mix across them, describe their own, or ask to see them rendered before deciding.
- **For a focused improvement:** Present a single refined proposal within the current design language.

### Stage 5: Implement
Tell which files you will change and why. Reuse existing tokens and components. Install any missing dependencies. Source real colors, fonts, and imagery instead of inventing placeholders. Verify each category of change against the running app as you go.

### Stage 6: Verify
Check the result against the confirmed problem list and the product's domain and tone before declaring it finished.

**Two equal standards for every proposal:** How much it improves the current design AND how well it fits the product and audience. A bold direction that doesn't fit the domain is rejected — a finance app stays trustworthy, legible, and data-dense, not playful or novelty.

---

## 7. DESIGN ASPECTS TO ADDRESS (the Improve UI checklist)

When upgrading UI, address ALL of these:

- **Typography & Scale:** Upgrade to premium font scales and high-quality typefaces tailored to context.
- **Color System & Tokens:** Replace generic palettes with neutral bases, refined accents, and advanced shadow diffusion for a modern, polished look.
- **Layout Restructuring:** Convert basic grids into engaging asymmetric bento grids, zig-zag sections, or split-screen heroes. Fix h-screen usage.
- **Flair & Personality:** Apply intentional design "edge" through oversized display type, deliberately broken grids, or unique media layering.
- **Micro-interactions & Motion:** Implement fluid CSS transitions or spring-physics-based animations for elements that resize or reorder.
- **Glassmorphism Upgrade:** Replace shallow background blurs with full refraction for a more premium glass effect.
- **Asset & Content Realism:** Replace generic placeholders with realistic figures, contextually relevant names, and high-quality media seeds.
- **Interactive States:** Add missing loading skeletons, empty states, and tactile feedback (like :active scaling) for all interactive elements.

---

## 8. THEME MOOD PRESETS (pick one before designing)

When suggesting themes, consider the mood:
- Professional & Corporate
- Creative & Energetic
- Calm & Minimal
- Luxurious & Premium
- Modern & Tech
- Warm & Friendly

**Rule for Iris:** Before generating a design, identify the target mood. This shapes color, typography, density, and motion choices.

---

## 9. CONTEXT GRAPH (know your codebase before coding)

Before generating code for an existing project, build a mental (or documented) Context Graph of reusable components. For each component, note:
- What it does (primary purpose)
- What it looks like (appearance)
- What props it accepts
- What it depends on

This prevents hallucinating props and skipping existing code. When designing new UI, check the Context Graph first — can you reuse an existing component instead of creating a new one?

**What to index:** UI Components, Hooks, Composables, Modules, State Management (Redux slices, Zustand stores, Jotai atoms), Utility Functions, Design Tokens, Icons, Assets, Type Definitions, Services, API Clients.

---

## 10. RULES TO ENFORCE (create design rules files)

Create rules that enforce your quality bar. Examples:
- "All components must use design tokens from the Theme, not hardcoded values"
- "All interactive elements must have hover, focus, and active states"
- "All pages must pass WCAG AA contrast ratios"
- "Mobile-first responsive: design for 375px first, then scale up"
- "No gradient text. No fake testimonials. No 'Trusted by' logo bars."

Rules are always on. Skills are contextual (fetched on demand when they match the task domain). Commands are reusable prompt workflows.

---

## 11. @MENTIONS (attach context to prompts)

- @Blocks — saved reusable UI patterns
- @Canvas — full-page layout designs
- @Designs — individual designs from a Canvas
- @Excalidraw — hand-drawn wireframes/diagrams
- @Figma — Figma design links

**Rule for Iris:** When asking for a design or code change, attach the relevant context. Don't make the agent guess what you're referring to.

---

## 12. INSPIRATION LIBRARY (reference before generating)

- **Inspirations:** Curated universal design references (not project-specific)
- **Templates:** Production-ready templates that can be remixed into projects

**Rule for Iris:** Build and maintain a curated inspiration library. When starting a new design, reference existing high-quality designs rather than generating from scratch. Save your best work as templates for future projects.

---

## 13. TECH STACK (confirm before coding)

- 400+ frameworks supported: React, Next.js, Angular, Vue, Nuxt, Svelte, Flutter, and more
- Reads tech stack from existing repo, asks to confirm first time, then codes using your stack and reusable code
- Works as VS Code / Cursor / Devin Desktop extension

**Rule for Iris:** Before generating code, confirm the project's tech stack. Use existing components, hooks, and utilities. Don't invent new patterns when the project already has established ones.

---

## 14. ANTI-PATTERNS (what Kombai rejects)

- Working from an imagined interface instead of the real UI
- Generating one design and calling it done (no variants, no passes)
- Color-swap variants instead of structural variants
- "Lorem ipsum" placeholder content instead of realistic content
- Hardcoded values instead of design tokens
- Missing interactive states (no hover, no loading skeleton, no empty state)
- Bold designs that don't fit the product domain (playful finance app, novelty healthcare UI)
- Skipping the planning step for complex features
- Hallucinating component props instead of checking the Context Graph
- Generic anchor text ("click here", "read more")
- Missing ARIA attributes, alt text, keyboard navigation
- Shallow background blurs instead of full glassmorphism refraction

---

## QUICK REFERENCE — The Kombai Quality Bar

```
1. Design first, code second
2. Define a Theme or Style Guide before designing
3. Generate 2-3 structural variants, not color swaps
4. Run 2-3 refinement passes per variant
5. Use realistic placeholder content, not Lorem ipsum
6. Label components [REUSE] or [NEW]
7. Pass all 7 design review axes before shipping
8. Source real colors, fonts, imagery — no invented placeholders
9. Check the Context Graph before creating new components
10. Match the design to the product domain and mood
```

---

## SKILL TAXONOMY (15 skills — adopt this structure)

### Design Skills
1. **Create Wireframes** — ask 5 questions, generate 3 structural variants, [REUSE]/[NEW] badges, realistic content
2. **Create Wireframes Excalidraw** — editable Excalidraw diagrams, grayscale low-fidelity
3. **Review Design** — 7-axis review (visual, UX, responsive, accessibility, motion, consistency, performance)
4. **Suggest Themes** — 3 theme options with mood presets
5. **Improve UI** — 6-stage redesign playbook (look, understand, diagnose, propose, implement, verify)
6. **Generate Image** — seedream 4, nano banana pro, gpt image 2, flux 2 klein 9b
7. **Generate Video** — veo 3.1, kling video v3 4k, 4-8s, $0.20-$0.60/s

### Web Dev Skills
8. **Improve SEO** — crawlable anchors, canonical URLs, hreflang, link text, search visibility
9. **Improve Accessibility** — WCAG 2.0/2.1/2.2, Axe-core, ARIA, keyboard nav, touch targets, alt text

### Other Skills
10. **Create Architectural Diagram Excalidraw** — color-coded system diagrams
11. **Create Mermaid** — Mermaid syntax diagrams
12. **Create Command** — reusable prompt workflows
13. **Create Rule** — always-on coding standards
14. **Create Skill** — contextual instruction sets (fetched on demand)
15. **Summarize** — conversation thread summaries for handoffs