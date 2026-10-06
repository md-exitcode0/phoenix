# References: gathering and using them

References are the fastest path to realism and taste. They are also the
fastest way to burn a whole session. Gather a small strong set, extract the
facts, and get back to building.

## Gathering (converge, then stop)

- Decide what you need before searching: the views (front, side, 3/4, top,
  detail close-ups), the lighting condition, the variants (ripe/unripe,
  new/worn, day/night). Write this list; it is your stop condition.
- Prefer sources with clear licensing and high resolution: Wikimedia Commons
  (API: `action=query&generator=search&gsrnamespace=6&prop=imageinfo&iiprop=url|size|mime|extmetadata`),
  Openverse, Unsplash, Pexels, museum open-access collections, manufacturer
  spec sheets for dimensions.
- Download to one folder with descriptive names
  (`03_side_profile_studio.jpg`). Keep a `sources.md` with URL, license,
  author and what each image shows.
- **Look at each candidate once.** Write the verdict immediately in
  `sources.md` (keep/reject + why + measured facts). Never re-inspect an image
  to answer a question the notes already answer.
- 6–12 strong references beat 40 mediocre ones. When every item on your
  needs list is covered by at least one good image, stop gathering.

## Contact sheets

Combine candidates into one labeled grid (ImageMagick:
`montage *.jpg -tile 4x -geometry 400x400+8+8 -label '%f' sheet.jpg`, or PIL).
One look at a sheet replaces a dozen single inspections. Use sheets to
compare, cull and hand over.

## Extracting facts

A reference is only useful once turned into numbers and rules:

- **Proportions:** overall length : width : height, and positions of key features
  as fractions of the length (e.g. "widest point at 0.45 L").
- **Curvature/angles:** arc of the main axis, taper toward the ends, bend angle.
- **Color:** sample 3–5 swatches (base, highlight, shadow, accent, defects) as
  hex, and note where each appears.
- **Surface:** facets, ridges, seams, pores, gloss level, where wear occurs.
- **Variation:** what differs between specimens (the range, not one sample).

Write these in the handoff or brief. The builder should rarely need to open the
photos again.

## Handing references to someone else

Return: the folder path, the contact sheet, and a short brief with one line per
kept image (what it demonstrates), the extracted facts above, disagreements
between sources, and the practical build implications. If you were asked only
for references, returning this brief **is** the finished job.
