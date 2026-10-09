# Conversation stability and the October 8 memory shutdown

At 19:44:57 America/Edmonton, the user service journal reported that the kernel
OOM killer killed processes in `phoenix-monocode.service`. The unit ended with
`result='oom-kill'`; systemd reported a 3 GiB memory peak and a 4.5 GiB swap peak.
The gateway socket and native interface subsequently disappeared. This is a
process shutdown, not evidence that the last conversation answer was delivered.
The journal does not identify a single allocation responsible for memory pressure.

Before that shutdown, a read-only live observation found 13,246 elements in the
conversation for 64 display rows. During that 5.5-second sample, the feed itself
was not replaced, but one frame interval reached 567 ms. Several collapsed tool
clusters contained more than 1,000 elements each. Another observation found the
Passes primary button text resolving to transparent `--mc-bg`.

The repair keeps collapsed tool bodies empty until opened, including large file
diffs and image previews. Late receipts rebuild only their own turn and leave
other messages and focused question controls attached. Unchanged work avatars
are reused instead of replacing their markup on every tool event. Primary labels
use an opaque contrast color, and delete controls have a reserved column.

Ordinary avatar consumers preload the four eye expressions used by automatic
activity animation. The underlying player retains its complete default expression
API. Reading the actual round/lemon atlas dimensions gives 64 versus 34 unique
layers, approximately 110.0 versus 85.5 MiB of uncompressed RGBA pixels per full
seven-state palette. These are atlas estimates, not a measured post-repair process
memory result. Artwork and activity frames remain unchanged.

Focused verification:

- `node scripts/check-question-submission.mjs`: real Chromium Enter/form events,
  owner routing, queue deduplication, manual browser opening, delegated activity,
  large diff expansion, delete geometry, both-theme primary labels, and focused
  unsent question preservation through late receipt repairs.
- `node scripts/check-fluffy-completion.mjs`: matching execution completion,
  delegated returns, late event fencing, fresh work, and pending questions.
- `node scripts/check-fluffy-image-budget.mjs`: actual player on inert surfaces,
  scoped asset loading, activity poses, full expression API, and image cleanup.
- `node monocode/ui/check-repair-flows.mjs`: captured tab navigation/reload and
  inline question ownership, resolution, and final response.

These checks passed. The live runtime was not restarted during verification.
Post-relaunch behavior and process memory still need live verification.
