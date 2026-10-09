# Interface crash on October 8, 2026

The interface renderer crashed at 18:33:55 MDT. The gateway and Electron main
process remained alive, and all eight existing private browser targets survived
the user's authorized interface-only reload. After reload, the UI selected Avery
(`school_coach`) and rendered animation frames normally. Saved session history
placed the user's request in Avery's session; Theo's last request was a separate
avatar task.

## Evidence and limits

- Electron reported `render-process-gone` with reason `crashed`, followed by an
  unresponsive interface. Linux recorded an `int3` trap in renderer PID 361055.
- The trap's Electron file offset was `0x3a90054`. The executable ELF segment
  maps file offsets to virtual addresses with a `0x1000` difference; the
  corresponding virtual address is `0x3a91054`, immediately after `int3` and
  before `ud2`. The preceding code reads a compressed weak array entry and traps
  when the entry equals the cleared value `3`.
- This instruction pattern is consistent with the installed V8 15.0.245.28
  [deoptimization literal guard](https://raw.githubusercontent.com/v8/v8/15.0.245.28/src/objects/deoptimization-data-inl.h)
  used by the
  [translated value provider](https://raw.githubusercontent.com/v8/v8/15.0.245.28/src/deoptimizer/translated-state.cc).
  The binary is stripped, so this is an inference, not a symbolized stack trace.
- Earlier development used debugger evaluation to replace private function
  bindings in the live interface. That is a possible trigger for this engine
  failure; causality has not been reproduced. Stop using that update approach.
- After the crash, native browser notifications attempted to send to the
  disposed interface and emitted `Render frame was disposed before WebFrameMain
  could be accessed`. This secondary failure is directly observed and covered
  by regression tests.

## Changes

Native event delivery now skips absent, destroyed, crashed, navigating, or
process-less interface frames and tolerates disposal racing delivery. Old events
are not queued for replay; the interface restores durable history and browser
state on load. Other errors remain observable.

Recovery diagnostics retain the failed renderer PID, reason, exit code, and time
after a replacement renderer loads. The recovery dialog still requires the
user's explicit reload choice and preserves other browser views.

Load frontend changes with an ordinary, user-authorized interface reload. Do not
replace optimized function bindings through debugger frame evaluation or script
replacement. Keep the runtime and private browser views running during recovery.

The shell changes require the next normal Electron launch. They were not injected
into the running main process or activated through a backend restart while Avery
was working. Six event-delivery tests and nineteen recovery tests passed.
