# Blender companion playback interface

The player is one small ES module: `player.mjs`. Each character supplies its own
transparent atlas manifest. It uses browser Canvas 2D, loads same-origin assets,
and adds no renderer dependency or global CSS. Mantis remains an experimental
option; the warm fluffy character is a separate direction. The SVG mantis in
`../mantis/` is preserved as a prototype.

```js
import { mountCompanion, renderCompanion, COMPANION_STATES } from './companions/creatures/player.mjs';

// character is the selected character's manifest; assets are still under review.
const creature = mountCompanion(avatarSlot, {
  character,
  size: 80,
  state: 'idle',
  motion: 'auto',
  decorative: true,
  onError(error) { /* retain the existing avatar fallback */ },
});
await creature.ready;
creature.setState('browsing');
await creature.ready; // newest asset load, useful to a review/test caller
creature.setState('coding');
creature.setState('waiting');
creature.setState('success'); // finite acknowledgment, then holds its final pose
creature.setState('error');
creature.setState('success', { restart: true }); // only for a real distinct event
creature.setCharacter(otherCharacterManifest);
creature.setSize(32);
creature.setMotion('reduce');
creature.setPaused(true);
creature.refresh();
creature.destroy();
```

`renderCompanion(options)` returns the same detached controller and accepts
`document`. Append `controller.element` and call `refresh()` for manual
insertion. `mountCompanion(container, options)` appends its owned host while
preserving the container's other children.

The six states are exactly `idle`, `browsing`, `coding`, `waiting`, `success`,
`error`. The frozen controller exposes `element`, `ready`, and read-only
`character`, `state`, `size`, `motion`. Setters and `refresh()` chain. Repeating
the selected state does not restart it. `destroy()` is idempotent and removes
only the owned host, RAF, observers, listeners and image leases. Other setters
after destruction throw. State interruption supersedes in-flight loading;
stale image completions cannot restart an old state or destroyed instance.

Size is 16–256 CSS pixels; 64–96 is the intended header range. At 40 and below,
the manifest's compact variant is selected if provided. DPR is capped at 2.
Decorative is the default: pointer inert, `inert`, `aria-hidden`, no focus target.
With `decorative: false`, the host is one `role=img` with a safe attribute label
`label: state`; it never adds live announcements. Label defaults to the manifest
label. `onError` receives an asset load/validation error; `ready` rejects and
`data-asset-error` is set, without inventing a runtime failure state.

Motion `auto` respects OS reduced motion; `reduce` forces still poses. Reduced
motion shows the first sustained-state pose or the settled finite-state pose,
with no deferred accent when motion is reenabled. Manual pause, document
visibility and viewport visibility suspend elapsed playback. A finite animation
keeps its semantic state after settling and never replays on refresh or resume.
Always destroy on slot teardown. Without IntersectionObserver, manual insertion
requires `refresh()` and the caller should pause hidden slots.

## Manifest

```js
const character = {
  id: 'fluffy', label: 'Fluffy companion',
  variants: {
    standard: {
      idle: {
        url: new URL('./assets/fluffy/idle.png', import.meta.url).href,
        cellSize: 192, columns: 8, gutter: 2,
        duration: 16000, loop: true,
        frames: [{ at: 0, cell: 0 }, { at: 7500, cell: 1 }],
      },
      // Supply all six states with actual rendered poses and timestamps.
    },
    // compact: { ...all six states },
  },
};
```

Frames are ordered by `at` in milliseconds. Held frames can share cells; no
uniform bobbing or interpolated 2D head transforms are imposed. Camera,
lighting, baseline and scale are authored in Blender and fixed between frames.
Transparent gutters prevent neighboring-cell bleed. Each active instance holds
one atlas lease; instances share active decoded images. A state/character change
releases its old image lease. Atlas dimensions and manifests are validated.

## Feed actual Phoenix activity

Task 1 owns shared conversation/header wiring. Reuse the runtime ownership,
current-turn, historical-replay and ask/approval gates already being implemented
there. This player has no event subscription or transcript heuristic.

| Actual selected-owner evidence | State |
| --- | --- |
| Accepted live browser/search/fetch tool start, cleared by matching receipt | `browsing` |
| Accepted live code editing/execution with actual coding context | `coding` |
| Unresolved user question or approval | `waiting` |
| Confirmed terminal completion, excluding queued/interrupted/awaiting-input | One `success` accent |
| Confirmed selected run failure | One `error` accent |
| Current work without specific activity evidence, or settled normal state | `idle` |

Do not animate from historical tools, another coworker's event, arbitrary
transcript prose or generic `using_tool` status. Clear stale tools on selection
change and preserve the existing avatar preference seam. Feed Phoenix's Minimal
preference through `setMotion('reduce')`; OS reduction remains independently
honored. Character selection does not rename Phoenix or replace its entire
avatar system.

## Current review stage

The source rigs contain all six real keyed actions. Full atlas batches remain
paused for character review. The controller API is available for integration;
review assets and final manifests will be supplied separately. A still render or
partial loop proof must not be represented as a completed six-state asset set.
