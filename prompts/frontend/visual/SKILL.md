# Visual field guide

Working knowledge for anything that ends up as pixels: 3D scenes and models,
SVG icons and characters, websites, images, diagrams. It complements the
design contract (`taste/SKILL.md`), which covers briefs and brands. This guide
covers **how the thing is actually built and how to judge it**.

Load only the file your task needs:

| File | Use it when |
| --- | --- |
| `visual/looking.md` | Every visual task, before calling anything done. How to judge a render like a senior artist. |
| `visual/references.md` | You need photos/examples of a real subject, or someone asked you for references. |
| `visual/3d.md` | Blender, three.js, any modeled object or scene, organic forms, materials, lighting, cameras. |
| `visual/svg.md` | Icons, avatars, faces, logos, illustrations, charts drawn by hand in SVG. |
| `visual/web.md` | Page layout, typography, spacing, category conventions, responsive behavior. |

## The seven laws

1. **Silhouette first, detail last.** A thing reads by its outline at thumbnail
   size. If the silhouette is wrong, no texture, shader or gradient saves it.
   Block the big shapes, check them small, and only then add medium and fine
   detail. The ratio that reads as "crafted" is roughly big : medium : small =
   70 : 20 : 10 of the visual interest.
2. **Measure, don't guess.** Proportions come from references or real
   dimensions written down as numbers (length/width ratio, curvature, angle,
   spacing in px). "Looks about right" is how every amateur result happens.
3. **Values before colors.** Light/dark structure carries form, hierarchy and
   depth. Check it in grayscale. A composition that fails in grayscale fails
   in color.
4. **Real things are irregular.** Nature and good craft have variation:
   no perfectly uniform color, no perfectly straight edge, no perfect
   symmetry, no exact repeats. Add variation deliberately and subtly
   (±3–10%), never as noise sprinkled over everything.
5. **Fewer, better elements.** Every element must earn its place. Remove until
   it breaks, then add back the last thing. Complexity is not quality;
   simple shapes executed precisely beat busy shapes executed loosely.
6. **Consistency is the craft.** One stroke width, one corner radius family,
   one light direction, one spacing scale, one type scale. Inconsistency is
   the number one tell of generated work.
7. **Judge the real output.** Render it, screenshot it, open it at the size
   people will see it. Code that "should" look right is not evidence.
   See `visual/looking.md`.

## Write what you see

When you inspect an image or render, write your observations in visible text
(or a notes file) in the same turn: what is right, what is wrong, measured
values. Observations that live only in your head are gone next round, and you
will end up inspecting the same image again. One inspection per question;
if a note already answers it, use the note.
