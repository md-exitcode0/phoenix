This module is a decoupled seam for the reviewed Blender atlas player. It is
not imported by the app yet and includes no player copy, manifest or art.
The parent-reviewed final assets and commit are required before header wiring.

`createConversationCompanion` accepts a host, injected `mountCompanion`, a
catalog of `{id, manifest}`, presentation callbacks `onFallback`/`onError`, and
size/Minimal settings. Catalog entries must refer to reviewed complete
six-state manifests. No character is selected by default. Existing avatar
preferences and host children remain untouched. A per-owner selection passes
`characterId` explicitly; a new owner falls back to its existing avatar unless
that owner's choice is supplied. Mantis is an optional experimental choice,
not a global replacement or default. Persisting preferences and labeling the
selector belong to the app's existing settings path, not this controller.

`select({ownerId, conversationKey, turnId, characterId})` returns an opaque
lease. Capture this lease with the conversation's existing selection token
before asynchronous work. Only after the app accepts a live event for that
selection should it call `accept(lease, event)`. Every normalized event must
carry `live:true`, canonical `ownerId`, `conversationKey` and `turnId`. Old
leases, peer owners, foreign threads/turns, missing identity, historical replay,
prose and unknown kinds are rejected. Do not set `live:true` on replay to make
the companion appear busy.

Normalized inputs:

- `tool-start`: a stable `operationId`, plus `activity:'browsing'|'coding'|'other'`
  from actual tool/domain evidence. Generic `using_tool` is not that evidence.
- `tool-end`: the same operation ID. Unmatched receipts cannot clear other work.
- `ask-open`/`ask-close`: stable `askId`, representing a question or approval.
- `status`: working, queued, awaiting-input, stopped or interrupted.
- `turn-ended`: completed, failed, stopped, interrupted, queued or awaiting-input.
  Completed requires explicit `success:true` to produce a success accent.

Pending input overrides lingering tools and terminal presentation. Closing input
after a contradictory completion does not invent a late success. Multiple tool
operations are counted, and the most recent specific still-live operation sets
browsing/coding. Peer completion cannot finish the selected owner. Late tool
starts are rejected after completed, failed, stopped or interrupted turns;
fresh activity requires a new turn or an explicitly accepted working status.
A renderer asset failure retains the avatar fallback and reports an asset error without
turning it into a Phoenix runtime failure. Selecting a different character
does not replay a past success/error accent.

The seam observes the player's newest `ready` promise after state/character
changes. Selection changes invalidate stale readiness callbacks and destroy the
old owner's instance. A new turn for the same owner can reuse its instance.
`setMinimal(true)` supplies reduced motion; the player separately honors OS
reduced motion. `setPaused` and `setSize` pass through. `destroy` is idempotent
and releases the owned player; the caller must invoke it on header teardown.

The Node regression uses clearly labeled synthetic manifests and an API-shaped
controller with controllable ready promises. It verifies isolation, activity,
input precedence, optional character choices, loading errors and teardown. It
does not verify Blender art, Canvas playback, the real header or native runtime.
