# Process lifecycle oracle

This audit covers forced container teardown and the ownership race between a
blocking process wait and concurrent signal delivery.

## Retained C implementation

The read-only retained engine was inspected at:

- `../engine/src/core/engine.c`: `hl_engine_run` publishes `engine->process`,
  waits through `host.process->wait`, and clears/closes the handle only after
  the wait returns. `hl_engine_request` reads that same published handle, so a
  concurrent force request retains signal authority while the waiter blocks.
- `../engine/src/host/linux/host.c`: `hl_linux_process_wait` increments the
  waiter count and marks `process_waiting`, but keeps the handle entry and PID
  addressable. `hl_linux_process_terminate` can therefore send `SIGKILL` until
  `process_reaped` is published. Completion stores the exit kind/value and
  broadcasts `process_changed`; close is rejected while a waiter or unreaped
  child exists. Destruction first marks the host destroying, kills every live
  child, waits for waiter ownership to drain, then reaps remaining children.
- `../engine/src/host/macos/host.c`: process wait/terminate follow the same
  retained-entry contract. Host destruction kills live children, waits for
  active waiters, and reaps any remaining child before releasing process
  storage.

The wait loops retry `EINTR`. A finite host deadline uses nonblocking wait and
returns would-block without surrendering the process record. Signal delivery
does not hold the process-table lock across `kill(2)`. Linux and macOS differ in
their process-handle backing, but not in ownership or publication ordering.

## Rust mapping

`ProcessLauncher::wait` previously removed `Child::Process` before entering
`wait_blocking`. An attachment task could therefore take sole ownership of the
handle, while concurrent force removal called `terminate`, found no published
child, and returned `StopFailed`. The child remained alive, the wait never
completed, and no terminal container state or wakeup was published.

The launcher now stores an `Arc<ProcessHandle>`, clones it while leaving the map
entry published, blocks outside the map lock, and removes the entry only after
the child is reaped. The focused blocked-wait test proves force signaling remains
available during the wait. Container force removal also stops and waits for
attached executions before deleting their records, allowing their completion
tasks to publish durable terminal state.

## Remaining gap

The current host port exposes a direct child and its initial process group. It
does not expose PID-birth identities or enumerate every member of the configured
launch domain. Consequently a descendant that creates a new session can escape
the initial process-group kill. A correct domain sweep requires a bounded,
platform-owned process inventory keyed by PID plus birth identity, membership
validation against the launch-domain identity, and two consecutive empty scans
before teardown succeeds. This patch does not claim that capability and does not
fall back to unsafe PID-only signaling.
