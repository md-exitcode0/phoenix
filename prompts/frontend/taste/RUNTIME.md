---
name: phoenix-taste-runtime
description: Phoenix's integrated, bounded design-to-build contract.
source: taste/SKILL.md
---

# Phoenix design runtime

<!-- Modified for Phoenix: integrated brief/brand/page/assets/build/review method
and native utilities. Derived portions: Copyright 2026 TasteCode contributors,
Apache-2.0. License and change map: licenses/tastecode/, docs/iris-design.md. -->

This is the complete runtime brief. The longer `taste/SKILL.md` is reference
material, not standing prompt payload. Apply this brief first; load at most one
task-specific supplement only when it contributes a genuinely missing workflow.

## Read the actual job

Before editing, infer the artifact class, user, primary workflow, existing
product language, supplied references, and quiet constraints. Inspect the real
surface and its tokens/components before inventing a direction. If the user
asked for a component, build or edit that component—not a landing page, fake
application shell, or approval showcase around it. If the user asked to build
or fix the real product, edit the real product; create a design master only
when the user explicitly asks to choose a direction first.

Privately set a design read and three dials: `DESIGN_VARIANCE`,
`MOTION_INTENSITY`, and `VISUAL_DENSITY`.
Match them to the product rather than defaulting to model taste. Ask one design
question only when two plausible readings would materially change the outcome.

## Make the product understandable

The visitor has not read the brief. In the first viewport, say what the product
is, what it does, who it helps, and the next useful action. Use concrete nouns
and ordinary verbs. A memorable headline may be expressive; the nearby sentence
must be literal. "A two-day festival of live electronic music and film" explains
an event. "A collision of unstable signals" does not. Never make the visitor
decode a metaphor to discover the product category.

Write the offer and real action before styling them. Order sections by visitor
questions and information dependencies, not a template. Every section earns its
space with a useful answer, proof, comparison, or action. Remove repeated claims,
vague slogans, decorative labels and dense lectures. Name links by destination
and controls by outcome. Never invent pricing, endorsements, availability,
performance or a working signup/download. Keep honest beta and example-data
labels; do not hide an unavailable service to make a page look finished.

## One integrated working process

For a serious new page or redesign, keep a compact design plan with the existing
workflow evidence, not a second approval system. Use these stages inside the
same authorized task; do not stop after each stage or ask the user to reprompt:

1. **Brief.** Original request, product/category, audience, offer, primary action,
   required content and scope. Separate verified facts from assumptions. Reuse
   supplied assets and the current project. Clarify only a genuine blocking
   choice; do not invent evidence to avoid asking.
2. **Brand.** Preserve, extend or create the system. Record locked decisions,
   one distinctive composition principle, and a restraint against repeating it
   everywhere. Inspect actual reference pixels, not just filenames. Extract type
   roles, heading/media proportions, density, crops and responsive behavior.
   Reference material is not permission to copy third-party artwork or branding.
3. **Page.** Give each section a visitor question, purpose, concrete copy,
   preceding information dependencies, a measured composition, a compact-screen
   transformation, and a motion decision. No fixed section count. Different
   projects need different geometry and rhythm, not a renamed favorite template;
   pages in one product must still share a coherent identity.
4. **Assets.** Obtain meaningful imagery, real app captures and type assets before
   depending on them. Record source, allowed use and actual path. Preserve user
   assets. A rendered interface example may be native HTML; do not invent fake
   product screenshots or use a diagram as a substitute for the real product.
5. **Build.** Edit the actual stack and preserve unrelated work. Implement the
   planned geometry before polishing. Use the existing `work`, file and browser
   tools; there is no separate design mode. Keep every material state usable.
6. **Review and repair.** Inspect desktop and compact views and follow the actual
   controls. Find the highest-impact discrepancy, fix its root cause, and inspect
   the changed result. Revisit an incorrect plan instead of treating it as an
   excuse. Keep the best working version; do not regress it to satisfy a checklist.

`design_studio` supplies local supporting tools, with no model call: `palette`
derives light/dark semantic roles independently and reports exact contrast pairs;
locked colors never silently change. `typography` returns stable candidates from
a project seed for a new identity; preserve an existing brand and verify actual
font loading. `review_blueprint` checks section decisions and dependency order;
`review_copy` flags unsupported claims, empty actions and formulaic language.
Save useful output in the existing design plan. A mechanical pass is not visual,
truth or comprehension acceptance. Do not reroll until a familiar style returns.

Use `taste/reveal.js` only when native scroll entrances fit the design. It keeps
initially visible content visible, honors reduced motion and supplies cleanup.
No universal fade-up, forced animation quota or extra motion dependency.

## Craft and scope

Reuse the actual stack, routes, tokens, icon family and state logic. A bounded
fix is not a redesign. Do not default to gradient headlines, dark-neon canvases,
three identical feature cards, card nesting, fake device chrome or decorative
blobs. Cards group real discrete items, not every paragraph. Keep legible type,
accessible contrast, one primary action, stable dimensions and semantic controls.
Use transform/opacity for motion, reduced-motion support and `100dvh`-safe layouts.

## State completeness

Design the usable state machine, not one screenshot. Cover the relevant normal,
hover, focus, pressed, disabled, loading, empty, error, overflow, narrow-screen,
and reduced-motion states. For a cursor or other component, test the component
at realistic scale on representative light and dark surfaces. A minimal isolated
harness is allowed only when the real app cannot expose the state cleanly.

## Verify the outcome

State one concise design read, then build. Inspect desktop and narrow renders,
console errors, overflow, clipping, contrast, assets, font fallback, keyboard
focus, first/final animation frames and every material interaction. Fix visible
weakness and repeat the same checks. Compilation or an analyzer PASS is not
visual verification.

Do a fresh-visitor pass before final: close implementation notes, reopen the
actual page at the top, and explain its category, offer and next action using
only visible content. Then perform that action. Inspect screenshots through
native vision or `image_analyze` after `browser_screenshot`/`ui_snap`; a failed
capture is not a look. A still image cannot prove motion or working interactions.
Record visible evidence, not an invented user-study result. Confusion is a
product defect: fix the copy, hierarchy or action and repeat the same journey.

The generated product carries its own identity. Never copy library names,
"powered by" badges, template credits or authoring instructions into its UI.
Preserve required copyright, license and asset-provenance notices in their
proper legal/source locations, not as gratuitous promotional branding.

Finish only when the actual requested surface works, looks intentional, and
has concrete verification evidence. Report what changed, what was exercised,
and any honest limitation; do not hide unfinished production work behind a
polished concept artifact.
