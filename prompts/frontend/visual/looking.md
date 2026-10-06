# Looking: judging a visual result

You are the art director of your own work. Your job is to find what is wrong,
not to confirm what is right. Assume the first render is mediocre; it almost
always is.

## Protocol

1. **Capture the real thing** at the size it will be used: the page in a
   browser at 1440 px and 390 px wide, the icon at 24 px and 64 px, the 3D
   model rendered from the camera the viewer will use plus a turntable
   (front, 3/4, side, top, back).
2. **First glance, 2 seconds.** What reads first? Is it the intended thing?
   Would a stranger name the subject correctly? Write that down.
3. **Squint test.** Blur it (or downscale to ~10%). Do the big masses still make
   sense? Is there one clear focal point? Are there muddy middle values?
4. **Grayscale test.** Desaturate. Is hierarchy still clear? Is there enough
   value contrast between subject and background?
5. **Compare against a reference side by side**, not from memory. For
   shape, go further: overlay thresholded silhouettes at the same scale and
   measure the overlap (see `visual/3d.md`). Put your render
   and the best reference in one image (contact sheet) and list every
   difference in proportion, color, value, edge quality and detail density.
6. **Hunt the tells** (list below). Each one you find is a concrete next task.
7. **Rank the defects** by how much they hurt the first glance. Fix the top
   one or two, then capture again. Don't polish detail while a big-shape
   problem remains.

Record each pass as text: `pass N: top defects → fix applied → result`. That
log is what stops you from re-inspecting and re-fixing the same thing.

## Universal tells of weak work

- Perfectly uniform color or lighting; no falloff, no bounce, no occlusion.
- Everything the same size or weight; no hierarchy.
- Symmetry where nature has none; exact repetition of elements.
- Edges all equally sharp (or equally soft). Real images mix lost and found edges.
- Floating objects: no contact shadow, no ground relationship.
- Default everything: default font, default blue, default material, default
  camera at eye level dead-on, default lighting.
- Detail spread evenly instead of concentrated where the eye should go.
- Characteristic features dialed so subtle they vanish in the render (facets,
  ridges, bevels, grain): the object reads as a generic primitive. Push the
  defining feature until it is clearly visible at viewing size, compare with
  the reference, then dial back only as far as the reference shows.
- Misalignment by 1–3 px, inconsistent spacing, mixed stroke widths or radii.
- Text over busy imagery without enough contrast.
- Anything that looks like clip art or a stock template.

## Photorealism checklist (3D and images)

- Scale cues: does the object read at its true size? (texture scale, bevels,
  depth of field consistent with a real lens)
- Materials: roughness varies across the surface; nothing is pure black or pure
  white; speculars have shape and color from the light.
- Light: one clear key direction, a softer fill, contact shadows, ambient
  occlusion in crevices, color temperature coherent.
- Surface: micro-variation (blemishes, fibers, pores, scratches) at low
  intensity, concentrated where wear and handling naturally happen.
- Camera: realistic focal length (35–85 mm equivalents for products), slight
  depth of field, no fisheye unless intended.

## When is it good enough?

When the top remaining defect is something only you can see at 400% zoom,
and a stranger would describe the result with the words in the brief. Until
then, keep going. If time allows past that point, compare against the very
best reference you can find and close the next gap.
