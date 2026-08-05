# Terminal scenario oracle

These 48 end-to-end cases preserve the IDs, images, shell or argv commands,
timeouts, class, targets, expected failures, resources, environments, exit
status, and substring contracts from
the retired legacy terminal fixture. The 36 cases that used the
legacy terminal executor declare `pty`; the remaining seven retain ordinary
pipe-backed execution. The existing Bash argv case remains an argv action.

The directory-owned `test.yaml` is now the sole executable definition. Before
removing the legacy fixture and unreachable runner, all 43 rows were compared
mechanically for ordered ID, image, command shape and bytes, timeout, class,
targets, expected failure, resources, environment, exit status, and literal
golden output.

Each expected substring is stored under `golden/` with no trailing line feed.
That detail is load-bearing: terminal output commonly contains CRLF, while the
legacy inline marker matched only its literal bytes. Adding LF to a golden file
would turn a representation migration into a different output contract.

The legacy terminal runner joined stdout and stderr before matching. Commands
whose expected text can originate on stderr already perform their own explicit
redirection; no migrated marker requires an added channel bridge. The repository
runner checks expected markers on stdout. Its typed PTY adapter supplies a PTY,
default `TERM=xterm`, and the 24x80 initial window required by the 36 `pty`
cases.

The legacy image fixture applied OCI-configured environment and working-directory
metadata before case overrides. In particular, it injected `TERM=xterm` only
when neither the image environment nor the case environment defined `TERM`.
`TestImage` currently materializes only the root filesystem, so the repository
runner checks only case environment before supplying that default and cannot yet
inherit either OCI environment or working directory. These cases use explicit
commands, `/` as their effective directory, and only the declared `TERM=screen`
override, but restoring generic OCI metadata inheritance remains a runner
capability gap and is not papered over in YAML.

No case contains a C, Rust, or heredoc source payload, so this category needs no
`source/` directory. This is a representation and ownership migration only. It
changes no engine runtime behavior, so the retired C implementation was not used
as an implementation oracle and `/Users/x/dd/engine` was not modified.

## Integrated engine PTY audit

The migrated scenarios exposed a separate production gap: the container adapter
propagates `ProcessConfig.terminal`, but its integrated Rust engine ignores that
field. This section records the required retained-engine audit before that runtime
domain is changed. The retained tree was read only.

Retained implementation studied:

- `../engine/src/core/activation.c`: `activation_prepare`, POSIX
  `activation_start`, `hl_activation_start_terminal`,
  `hl_activation_start_with_channels`, `hl_terminal_resize`, process wait/kill,
  and the Windows `activation_start` refusal. POSIX launch owns one `openpty`
  pair, moves a master allocated as descriptor zero, applies the initial
  `winsize`, forks, then makes the child a session leader with `setsid`, acquires
  the slave with `TIOCSCTTY`, and duplicates that one open description onto
  descriptors 0, 1, and 2. The parent closes the slave and returns the master.
  Handshake failure and allocation failure close the master, kill the entire
  child process group, and reap it. Dropping the last master closes the host
  terminal; a resize is `TIOCSWINSZ` on that same master. Windows rejects the
  terminal form before spawning because its activation layer has no equivalent
  controlling-terminal object.
- `../engine/src/host/linux/host.c` and
  `../engine/src/host/macos/host.c`: `*_terminal_probe`, `*_terminal_get_mode`,
  `*_terminal_set_mode`, `*_terminal_get_size`, `*_terminal_set_size`,
  `*_terminal_read`, `*_terminal_write`, and
  `*_terminal_size_change_event`. The host handle table lock protects lookup;
  blocking terminal operations run after lookup without holding that table lock.
  Modes map canonical input, echo, signal characters, flow control, and output
  processing. Reads and writes preserve host partial-result and errno behavior.
  Resize validates the complete dimensions and uses the live terminal object.
  Linux and macOS deliberately expose no separate size-change event.
- `../engine/src/linux_abi/syscall/fs.c`: `tty_ctl_block`, the terminal ioctl
  cases in the filesystem syscall dispatcher, and PTY master/slave bookkeeping.
  It preserves termios and window state, master/slave identity, devpts numbering,
  `TIOCGPTPEER`, packet mode, controlling-terminal acquisition/detachment,
  foreground process groups, and `SIGWINCH`. Terminal-control operations briefly
  block host `SIGTTOU` so a shell cannot stop halfway through foreground-group
  handoff. Closing the final slave drives master EOF/HUP behavior.
- `../engine/src/linux_abi/syscall/signal.c`: `rt_sigprocmask` mirrors only
  `SIGTSTP`, `SIGTTIN`, and `SIGTTOU` into the host mask, retaining the ordering
  needed by job-control handoff. `../engine/src/linux_abi/syscall/rare.c` and
  `syscall/proc.c` implement `setsid`, process-group changes, and init-ID
  translation. `../engine/src/linux_abi/fork.c` forwards terminal-relevant
  signals, including `SIGWINCH`, across its process boundary.
- `../engine/src/linux_abi/host_tty.h`: the host split. Linux and macOS use real
  termios/PTY operations. Windows defines ABI shape but explicitly refuses the
  line-discipline, PTY, controlling-session, and foreground-group operations;
  ConPTY is documented as non-equivalent.

The retained implementation uses host-kernel PTY state, process groups, and
locking; the Rust implementation instead has host-neutral state and therefore
must preserve the same externally visible lifecycle through its own owners:

| Capability | Rust owner | Status at container launch |
|---|---|---|
| bounded PTY identity and generation | `hl-terminal::Catalog` | implemented but not allocated |
| canonical/raw discipline, echo, CR/LF processing, bounded queues | `hl-terminal::Pair` | implemented but unreachable |
| master/slave OFD sharing, blocking, readiness, last-slave close | `hl-terminal::Description` | implemented but not installed on 0/1/2 |
| devpts, `/dev/tty`, terminal ioctl and packet mode | `hl-engine` path adapter plus `hl-runtime` filesystem | implemented for guest-created PTYs |
| sessions, process groups, foreground ownership, stop signals | `hl-task` plus `hl-runtime` terminal signal adapter | implemented for guest-created PTYs |
| initial controlling terminal and foreground group | engine launch composition | missing |
| cancelable merged host input/output transport | engine consumer port plus container adapter | implemented, not wired to a PTY master |
| initial window and later resize through the same pair | engine/container adapter and `Running` | missing; `Running::resize` always returns `NoTerminal` |
| deterministic bridge cancellation, close, and teardown | engine/container adapter | missing |
| Windows host behavior | engine/container adapter | undecided; retained engine explicitly refuses |

The missing mechanism crosses the engine construction API, initial descriptor
table construction, task/session attachment, host input/output pumping, running
process ownership, resize, and teardown. Installing a terminal-shaped flag on
the existing `StandardIo` objects would make `isatty` pass while leaving line
discipline, `/dev/tty`, job control, input, resize, and close semantics false.
The implementation must instead allocate one `hl-terminal` pair, install the
slave open description on descriptors 0/1/2, bind it into the process terminal
namespace, attach it to init's session and foreground group, retain the master
in the running-process owner, and expose bounded input/output pumps plus resize
and cancellation. That is a coherent follow-up runtime lane, not a scenario
runner exception.

The first composition prerequisite is now explicit. `hl-engine::composition`
owns the narrow `TerminalPort` contract because the engine consumes terminal
input and produces the single merged terminal-output stream. `hl-container`
implements that port over its bounded input and log channels. Closing the port
wakes a blocked reader within a bounded polling interval even if a client keeps
its input sender alive; a backpressured writer observes the same close as
`BrokenPipe`. The adapter preserves partial reads, caps each accepted output
write to the existing log chunk bound, and labels all terminal output as the
merged stdout stream. This does not yet allocate or attach a PTY.

## Interactive-input workflows

The five former `workflows/pty.rs` sessions are now directory-owned YAML cases.
A `terminal` action defines one argv process, its initial dimensions, and an
ordered bounded sequence of `write`, `resize`, `await_output`, `reject_output`,
and `close` operations. `await_output` has an explicit deadline and consumes the
same durable ordered session stream used for final output verification;
`reject_output` checks the completed raw transcript. This preserves DEL editing,
canonical carriage returns, raw-mode synchronization, explicit EOF, and the
absence-of-DEL assertion without sleeping or invoking a host command.

The runner creates one additional process through `Containers::executions`,
opens stdin explicitly, starts the terminal at its declared size, and performs
resizes through the execution API. Input uses the session's bounded writer and
output remains subject to the runner's one-MiB capture bound. Schema validation
limits each terminal action to 64 steps, each text field to 64 KiB, each wait to
60 seconds, nonzero dimensions, one close, and no write after close.

Each operation also emits a bounded durable ledger record after container
startup: ordered step index and operation, elapsed microseconds, bytes written,
bytes read from the transcript, and success or failure. The final transcript
drain is named separately. These values exclude image, provider, and container
startup and preserve failed resize, timed wait, write, close, and negative
assertion evidence in the partial result journal as well as the finalized TSV.
Scenarios without terminal actions retain an empty `terminal_steps` field.
