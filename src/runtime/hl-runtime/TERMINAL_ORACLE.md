# Terminal lifecycle oracle audit

> **Historical ownership:** Old `ffi/linux/execution` paths below preserve the
> deleted Rust-engine audit. The selected C engine currently supplies guest
> terminal behavior through the Rust product boundary.

## Retained C implementation studied

- `../engine/src/linux_abi/syscall/fs.c`: `svc_fs` ioctl switch, pty master
  termios/window cache (`ptm_*`), `TIOCSPGRP`, `TIOCSCTTY`, `TIOCNOTTY`,
  `TIOCGSID`, and window ioctl forwarding.
- `../engine/src/linux_abi/signal.c`: guest signal-number disposition mapping,
  including default-ignore `SIGWINCH`.
- `../engine/src/linux_abi/host_tty.h`: host-independent tty ioctl numbers and
  by-value `termios`/`winsize` ABI shapes.
- `../engine/src/core/activation.c`: controlling-terminal setup and
  `hl_terminal_resize` / `hl_activation_terminal_resize` window propagation.
- `../engine/tests/compat/posix/tty_notty.c`, `tty_leaderhup.c`, `pty.c`, and
  `apt_pty.c`: differential acceptance contracts.

The retained engine delegates controlling-terminal identity and lifecycle to a
real host pty. `svc_fs` enters the host ioctl synchronously; the host kernel's
tty/session and signal locks own identity, foreground membership, publication,
wakeup, and teardown. `TIOCNOTTY` is forwarded after an `isatty` check. For a
session leader the kernel removes the tty-to-session binding and publishes
`SIGHUP` followed by `SIGCONT` to the old foreground group. For a nonleader it
clears only that process's controlling-tty association. Session-leader exit
publishes `SIGHUP` without `SIGCONT` and releases the session binding.

`TIOCSWINSZ` operates on the selected tty pair, including a pty master. The
retained macOS path recognizes masters from the devpts registry, caches the
pair's window, and live-pushes it through a transient slave. Signal routing is
therefore a property of the tty's generation-qualified foreground group, not
the ioctl caller's session. Linux emits `SIGWINCH` only when dimensions change.
Standard-signal coalescing and stopped-task wakeup are kernel signal semantics.
There is no partial result or cancellable wait in these ioctls. Bad tty
endpoints return `ENOTTY`; host ioctl errno otherwise passes through. The ABI
constants and eight-byte window layout are common to both guest architectures;
master retargeting/cache mechanics are the host-specific branch.

## C-to-Rust capability matrix

| Capability | Rust owner | Status |
|---|---|---|
| Generation-safe session and foreground group | `hl-task::TaskRegistry` | implemented |
| Same-session foreground assignment validation | `TaskRegistry::set_foreground_group` | implemented |
| `TIOCNOTTY` HUP then CONT publication | `TaskRegistry::prepare_terminal_transition` + `hl-runtime` ioctl adapter | implemented after successful catalog detach |
| Session-leader exit HUP without CONT | `hl-engine::release_terminal` + prepared task transition | implemented after successful catalog detach |
| Changed-window `SIGWINCH` publication | `Pair::set_window` + `TaskRegistry::terminal_window_changed` | implemented; unchanged windows are suppressed |
| Master/other-session window routing | terminal foreground wire identity + task generation validation | implemented |
| Per-process nonleader ctty detachment | `hl-task::Process::terminal_detached` | implemented, forked and checkpointed |
| `setsid` starts without a controlling tty | `TaskRegistry::create_session` | implemented; explicit acquisition reattaches the exact session identity |
| `/dev/tty` after nonleader detach | engine path adapter + task terminal association | implemented as `ENXIO`/`NoDevice` |
| Failed cross-domain acquisition | terminal `acquire_changed` + task exact-session attach | implemented with compensation only for newly created catalog bindings |
| Host pty termios/window forwarding | `hl-terminal::Pair` | implemented as memory-safe pair state |

The typed detach/exit transition captures the exact `SessionId`, leader scope,
generation-qualified foreground group, and recipients under the task lock. The
runtime uses that prepared session for the terminal mutation, eliminating the
old second session lookup. Commit updates per-process associations under the
task lock and then enqueues signals after releasing it, avoiding task/terminal
lock nesting and signal-wakeup callbacks under either state lock. The portable
task checkpoint owns the per-process association; the terminal catalog retains
the separate session-to-pair binding and clears foreground state on session-wide
detach. A stale tty foreground identity never blocks detachment: it produces an
empty recipient set and cannot be redirected to a recycled or cross-session
group.

## Acceptance ownership

This document belongs to `hl-runtime`, not to a standalone repository runtime
case. Controlling-terminal lifecycle is a cross-domain operation joining
`hl-terminal` pair/catalog state, `hl-task` session and signal state, and the
filesystem ioctl adapter. The focused public contracts remain beside those
owners:

- `hl-terminal/src/pty.rs` covers changed-only window publication, foreground
  identity, catalog acquisition/detachment, and foreground clearing;
- `hl-task/src/registry/job/test.rs` covers sessions, same-session foreground
  validation, leader/nonleader detach, signal ordering, stale generations, and
  checkpointed associations;
- `hl-runtime/src/filesystem/syscalls_test.rs` covers `TIOCNOTTY`, `TIOCSCTTY`,
  mutation-before-signal ordering, stale foregrounds, and master-side window
  routing;
- `hl-engine/src/ffi/linux/execution/{exit.rs,path.rs,path/device.rs}` covers
  launch/path integration, session-leader exit, reacquisition, and external
  resize against the current generation-qualified foreground group.

The retained executable acceptance cases are already self-contained in
`tests/runtime/posix/test.yaml`: `ctty-session`, `tty-notty`, `tty-leaderhup`,
`pty`, `apt-pty`, `pty-jobsig`, `pty-ctl`, and the explicitly classified
`tty-bg-deviation` row. The application-facing PTY and job-control workflows live
separately in `tests/scenarios/terminal/test.yaml` with their local golden
files. Creating another `tests/runtime/terminal/test.yaml` would duplicate
those contracts rather than add coverage, so removing the former ORACLE-only
directory is the complete ownership correction.
