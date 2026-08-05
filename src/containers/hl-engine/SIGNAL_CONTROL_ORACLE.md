# External signal control oracle

The retained C signal-control call graph audited for this port is
`src/core/engine.c::hl_engine_request`,
`src/host/{linux,macos}/host.c::*_process_terminate`,
`src/linux_abi/signal.c::raise_guest_signal_info`, and the pending-signal turn
in `src/core/dispatch.c`. The engine lifecycle owns the process handle and
retains a pre-run request. Host tables resolve generation-checked process
identity under their lock, release the lock before signal delivery, and map
host errors to typed status. Linux uses identical signal numbers; macOS maps
the guest number before `kill`. EINTR concerns belong to interrupted guest
syscalls, not this nonblocking control request.

Guest disposition, masks, standard-signal coalescing, handler/default behavior,
and stop/continue generations belong to the Linux signal layer. STOP parks but
does not terminate; CONT advances the process control generation and resumes
execution. Blocking syscalls and translated execution are both woken so the
pending boundary is observed.

Rust maps these owners to `Engine` lifecycle, `hl-task::TaskRegistry`, the
`ThreadSet` live process association, and the scheduler's existing
`SignalActivityEvent` bridge. The previous adapter terminally cancelled every
external signal and also moved the engine to `Stopping`, so handlers never ran
and a later CONT request was discarded. Non-force requests now enter the guest
pending queue while the engine remains runnable. Force remains terminal.
