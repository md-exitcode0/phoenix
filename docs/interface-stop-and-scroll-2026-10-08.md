# Stop, saved question replies, and reading position

The live gateway journal showed a targeted Avery cancellation at 20:26:46,
followed by a new turn startup at 20:26:49. A saved answer continuation to
`unlock-d7fcc4fc` was waiting behind the foreground run. Stop released that lane,
allowing the pending continuation to start. The current execution was explicitly
canceled at 20:29:25; the gateway confirmed cancellation and retired its running
queue receipt. Subsequent authoritative snapshots showed Avery idle with an
empty queue.

The interface now reads the selected conversation's queue and retires pending
question-answer continuations before canceling the foreground execution. It
uses the captured conversation owner and session throughout, and repeated Stop
clicks share the same operation. Authored queued prompts and other owners are
not silently removed.

Visual preferences publish a before-change event so the conversation can capture
its reading position before CSS hides or reveals historical tools. The detail
renderer restores the message anchor or latest-message position synchronously;
animation frames refine it when available. A message anchor survives both modes.

New question replies are saved as authored rows at the time of submission rather
than replacing the original question's historical position. Wake acknowledgements
reuse the existing bubble without detaching or moving it. Legacy history that
omits an ask id is matched only against an unambiguous saved question in the same
conversation; copies of that one answer do not add new prompt rows. Different
identified questions remain distinct even when their answer text is identical.

Verification: `node scripts/check-question-submission.mjs` passed real Chromium
regressions for pending-answer Stop ordering and single flight, new reply order,
wake bubble identity, repeated legacy recovery, and tool-mode toggles while pinned
to latest and while reading an older message. Existing question, vault, browser,
draft, activity, large-diff, delete-control, and theme assertions also passed.
`node monocode/ui/check-repair-flows.mjs`, JavaScript syntax checks and
`git diff --check` passed.

With all agents idle, the interface alone was reloaded after flushing its public
presentation state. Seven existing browser tabs remained attached. The loaded
conversation script was `20261008t`. Live compact/chat/compact switches stayed
at the latest message (zero bottom slack after both switches); an older message
kept an identical 120 px viewport offset through both switches. Avery remained
idle. No agent messages were sent by verification.

A later screenshot exposed two historical queue-failure notices even though the
live queue was empty and Avery was idle. Queue notices now refresh the queue on
live delivery instead of creating transcript warning rows. Only actual failed
entries appear in the labelled Needs review drawer, with their original failure
reason and removal control. Replay cannot create stale actionable warnings;
refresh also removes any existing obsolete warning nodes without deleting the
saved transcript or error records. The real Chromium fixture verifies repeated
notice replay, a single real failed entry, retained failure details, and clearing
both warning nodes and the drawer when the queue becomes empty.
