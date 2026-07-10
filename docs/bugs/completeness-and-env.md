# Completeness, Silent Corruption, and Env-Var Features

Date: 2026-07-10

This is the standing hunt for unhandled subcases, silent corruption, badly handled edge conditions, and features controlled only by environment variables.

## What To Hunt

- Syscall cases that return fake success, wrong errno, stale data, or partial support where real software expects a clean failure.
- Opcode families where one subform is implemented but flags, rounding, scalar merge, memory operand behavior, fault order, or VEX/legacy distinctions are missing.
- Runtime behavior that changes only through undocumented or lightly tested environment variables.
- Xfail comments or docs claiming a gap exists after code appears to implement it, or claiming coverage where the probe only tests a happy path.
- Completeness reports that count implemented handlers without proving semantic coverage.

## Initial High-Value Leads

| Area | Lead | Why it matters | Current evidence |
|---|---|---|---|
| AVX scalar moves | `vmovss`/`vmovsd` VEX register-source merge semantics | silent vector-lane corruption | [jit-and-opcodes.md](jit-and-opcodes.md#vex-vmovssvmovsd-register-source-merge-semantics) |
| F16C | `vcvtps2ph` ignores immediate rounding | wrong math results under non-default rounding | [jit-and-opcodes.md](jit-and-opcodes.md#f16c-vcvtps2ph-ignores-rounding-immediate) |
| SSE4.2 | string compare leaves AF stale | flags consumers get wrong state | [jit-and-opcodes.md](jit-and-opcodes.md#sse42-string-compare-leaves-af-stale) |
| JIT cache | executable VA unmap/remap stale translation | old code can run after remap | [jit-and-opcodes.md](jit-and-opcodes.md#stale-translation-after-unmapremap) |
| coverage | static coverage scans wrong tree and exits green | completeness report is false | [daemon-tests-docs.md](daemon-tests-docs.md#coverage-tool-uses-stale-engine-paths-and-exits-green) |
| tests | completeness suite is curated, not exhaustive | hidden subform bugs pass | [jit-and-opcodes.md](jit-and-opcodes.md#opcode-completeness-is-still-curated-not-exhaustive) |
| sentry/ipc | `DDJIT_UNTRUSTED` SCM_RIGHTS + eventfd loses wakeups (SKIPPED — deep, cross-process) | dense fd passing can drop events or leave child failure | [this file](#ddjit_untrusted-scm_rights-eventfd-loses-events) |
| auxv/page | aarch64 `AT_PAGESZ` exposes host 16K page size | allocators and page-size probes see non-Linux ABI | [syscall-compat.md](syscall-compat.md#aarch64-at_pagesz-exposes-host-page-size) |
| fcntl | `F_SETLEASE` / `F_NOTIFY` fake success | callers believe coordination/invalidation was armed | [syscall-compat.md](syscall-compat.md#f_setlease-f_notify-return-success-without-arming-anything) |
| exec env | `envp=NULL` leaks defaults/stale env (SKIPPED — default-injection entanglement) | empty-env execs receive unexpected variables | this file |
| exec env | newline-containing env values split across exec (FIXED 2026-07-10) | silent environment corruption | this file |

## Env-Var Inventory Targets

The next verification wave should inventory `getenv`, `DD_*`, `DDJIT_*`, and feature-kill switches, then classify each as:

- documented and tested,
- documented but untested,
- hidden debug/perf switch,
- feature behavior switch with no test,
- compatibility hazard if set in host environment,
- stale variable read that no launch path can set.

Known lead:

- `DD_EGRESS_SOCKS` is set by the builder but dropped by typed launch config, so the engine never sees it in normal typed launch. This is now proven in [daemon-tests-docs.md](daemon-tests-docs.md#workspace-vpn-egress-is-dropped).
- `DDJIT_SANDBOX` is set by public runtime/builder paths but intentionally avoided by the harness, leaving a public mode with weak coverage.
- `S3DB_DURABILITY=none|fast|strict` changes `fsync` durability semantics behind a cached runtime env read. It is a real performance/correctness switch and needs explicit tests.

## Confirmed Env And Completeness Findings

### `DDJIT_UNTRUSTED` SCM_RIGHTS + Eventfd Loses Events

Priority: P1
Impact: IPC race and lost wakeups under sentry/untrusted mode
Confidence: High

Evidence:

- Existing test case is registered in `dd-tests/src/cases/ext/ipc.rs:121`.
- SCM_RIGHTS translation runs in the sentry fd path: `dd-jit-darwin/src/runtime/os/linux/sentry.c:628`.
- Recvmsg copyback notes that SCM_RIGHTS fds remain sentry fds in the guest control payload: `dd-jit-darwin/src/runtime/os/linux/sentry.c:1876`.

Observed isolated result from `/Users/x/dd/dd-verify-4b`:

```text
expected: woke=48 read=48 sum=1176 child=0
observed: woke=46 read=46 sum=1081 child=4
```

Why this is bad:

Dense fd passing plus eventfd wakeups should be deterministic. Lost events mean IPC workloads can hang, under-count work, or propagate child failures only intermittently in untrusted mode.

Status (2026-07-10): SKIPPED for now — deep. The loss is in the sentry SCM_RIGHTS fd-translation + eventfd
copyback path across processes (`sentry.c:628`/`:1876`), a cross-process delivery race rather than a local
one-line bug. Reproduction is non-deterministic (`woke=46/48`), so a fix needs instrumenting the sentry
recvmsg copyback to prove where wakeups are dropped before changing the delivery protocol. Left for a focused
sentry pass; not a minimal-fix candidate.

### `DDJIT_SANDBOX` Public Mode Is Intentionally Avoided By Tests

Priority: P2
Impact: public runtime mode can drift without matrix coverage
Confidence: High

Evidence:

- Public `.sandbox(true)` sets `DDJIT_UNTRUSTED` and `DDJIT_SANDBOX`: `dd-jit/src/runtime/container/builder.rs:148`.
- Runtime defaults can also inject sandbox behavior: `dd-jit/src/runtime/runtime.rs:60`.
- The test harness deliberately sets only `DDJIT_UNTRUSTED`: `dd-tests/src/harness/run/config.rs:18`.
- Sentry comments say the stronger sandbox is only sound once syscall forwarding is complete: `dd-jit-darwin/src/runtime/os/linux/sentry.c:436`.

Why this is bad:

Users can enable a mode that the main matrix avoids. Bugs that only appear when both sentry and sandbox behavior are enabled will not be caught by normal tests.

### `S3DB_DURABILITY` Hidden Fsync Semantics

Priority: P2
Impact: silent durability/performance tradeoff controlled only by env
Confidence: High

Evidence:

- The runtime reads and caches `S3DB_DURABILITY=none|fast|strict`: `dd-jit-darwin/src/runtime/os/linux/syscall/helpers.c:467`.
- `none` returns success without a real sync, while `strict` uses the expensive host full-sync path.

Why this is bad:

This env var changes the correctness contract for `fsync`. `none` is useful for ephemeral workloads, but if inherited accidentally it can make software believe data is durable when it is not. `strict` can also create a large performance cliff.

Suggested gate:

Add a small fsync/fdatasync policy test that verifies each mode is selected only through explicit launch config or documented env setup, and that the mode is visible in test output.

### `execve(..., envp=NULL)` Leaks A Default Or Stale Environment

Priority: P1
Impact: empty-env execs receive unexpected variables
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-slot-e`.

Evidence:

- `exec_forward_env` returns immediately for `envp_guest == NULL`: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:18`.
- The exec path calls it before redirecting to the new image: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1632`.
- `build_stack` then reads `DD_GUEST_ENV` and default env entries: `dd-jit-darwin/src/runtime/os/linux/elf.c:868`.

Why this is bad:

On Linux, `execve(path, argv, NULL)` produces an empty environment. dd leaves the previous/default guest environment intact, so programs launched as empty-env can see variables they should not.

Status (2026-07-10): SKIPPED — entangled with the engine's default-env injection. The observed `envc=4` is
exactly the `g_guest_env` defaults (`PATH`,`HOME`,`LANG`,`GLIBC_TUNABLES`) that `build_stack` merges
unconditionally on EVERY launch/exec, not stale container env. Matching native `envc=0` requires suppressing
those defaults on a guest-initiated exec, but `GLIBC_TUNABLES=glibc.cpu.aarch64_gcs=0` is load-bearing for the
re-exec'd aarch64 image (disables Guard Control Stack) — dropping it risks crashing real workloads on a shared
engine gate. A safe fix needs an exec-vs-initial-launch marker plus a decision on which defaults are
engine-internal necessities vs guest-visible env; deferred. NOTE: this default-injection also perturbs
non-NULL execs (a minimal `execve(path,argv,["FOO=bar"])` yields `envc=5` on dd vs `1` on native), so the fix
should address the general case, not just NULL.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-slot-e-target cargo run -p dd-tests -- -e aarch64 exec-null-env
```

Observed: dd prints `envc=4`; native prints `envc=0`.

### Newline-Containing Env Values Split Across Exec — FIXED (2026-07-10)

Fixed on branch bugfix/completeness-env-v2. `exec_forward_env` now escape-encodes each record
(`'\\'`->`"\\\\"`, `'\n'`->`"\\n"`) and sets `DD_GUEST_ENV_ESC=1`; both `build_stack` implementations
(`os/linux/elf.c` for aarch64 and `translate/x86_64/elf.c` for x86_64) unescape when the marker is present,
so a value's own newline never masquerades as a record separator. The daemon-launch path (plain
`DD_GUEST_ENV`, no marker) is byte-for-byte unchanged. Test: `comp-sys-proc/exec-newline-env`
(`exec_newline_env.c`) — passes on both engines (`value_ok=1 split_entry=0`, matching native).

### aarch64 Pcache Key Omits `NOSTEALFAST`

Priority: P2
Impact: warm persistent cache can hide codegen/perf mode changes
Confidence: Medium

Evidence:

- `NOSTEALFAST` is forwarded into the runtime environment: `dd-jit-darwin/src/spawn_config.rs:181`.
- It changes emitted aarch64 codegen behavior: `dd-jit-darwin/src/runtime/translate/aarch64/translate.c:126`.
- The aarch64 pcache mode-key list omits it: `dd-jit-darwin/src/runtime/translate/aarch64/pcache.c:231`.

Why this is bad:

Perf/debug A/B runs can load code emitted under a previous mode, making the toggle appear ineffective or producing mixed-mode measurements.

Verification:

Run an aarch64 guest twice with persistent cache enabled, toggling `NOSTEALFAST`, and assert the cache file identity or emitted bytes differ.

### Per-Container `DDJIT_NOPCACHE` Is Dropped By Typed Launch

Priority: P2
Impact: cache kill-switch is hard to apply to one container through the typed runtime path
Confidence: Medium

Evidence:

- `launch_config` explicitly drops engine tuning env vars including `DDJIT_NOPCACHE`: `dd-jit/src/runtime/container/mod.rs:37`.
- Runtime defaults can still inject `DDJIT_PCACHE` / `DDJIT_PCACHE_DIR`: `dd-jit/src/runtime/runtime.rs:48`.

Why this is bad:

Operators can enable persistent cache defaults globally, but a per-container cache disable knob in the container env is not carried through typed launch. Debug/perf A/B runs can accidentally stay warm-cached.

Verification:

Launch two containers through the typed runtime with global pcache defaults and per-container `DDJIT_NOPCACHE`, then assert no pcache file is loaded or written for the opted-out container.

### Typed Launch Path Lists Still Use Delimiter Env Strings

Priority: P2
Impact: paths containing `:` or `,` can be misparsed or dropped
Confidence: Medium

Evidence:

- Typed launch serializes lower dirs with `:`: `dd-jit-darwin/src/launch/wire.rs:72`.
- Volumes are serialized into delimiter strings: `dd-jit-darwin/src/launch/wire.rs:83`.
- Configfd rehydrates those strings into env vars: `dd-jit-darwin/src/runtime/os/ddjit_configfd.c:123`.
- Linux container parsing splits volume specs at delimiters: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:716`.

Why this is bad:

The typed launch path avoids a shell, but path lists still pass through delimiter-encoded env strings. Host paths with delimiters can be silently split wrong.

Verification:

Create a bind source path containing `:` or `,` and assert the guest sees the correct mounted content.

### Hidden Proc Switches Change Peer Procfs

Priority: P2
Impact: environment-only behavior can hide process files from peers
Confidence: Medium

Evidence:

- `DD_HIDE_CHROME_PROCFILES` changes procfs visibility behavior: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3075`.
- `DD_PROC_CHROME_MODE` changes Chrome-specific procfs handling: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3085`.

Why this is bad:

These switches alter procfs compatibility without an obvious typed launch contract or documented test gate. They can make process discovery and diagnostics differ between otherwise identical runs based only on inherited environment.

### Cgroup Membership Omits Forked Children

Priority: P1
Impact: supervisors see inconsistent task membership and pids usage
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-W-envproc-20260710`.

Evidence:

- `cgroup.procs` and `cgroup.threads` print only `container_pid()`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3382`.
- `/proc/stat` already uses the process registry count: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3189`.
- Fork children are registered in the proc registry: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:343`.
- `pids.current` reads process-private `g_pids_cur`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3363`.

Why this is bad:

After fork, cgroup membership and pids accounting should include child processes. dd can report a child in process stats while omitting it from cgroup membership and current pids usage.

Isolated proof:

```sh
FILTER=cgroup-members
```

Result: failed on both Linux engines. Actual `procs_child=0 threads_child=0 pids_ge2=0`; expected all `1`.

### `DD_PIDS_MAX` Is Not Enforced For Forked Processes

Priority: P1
Impact: advertised pids limit can be exceeded by fork/clone3
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-W-envproc-20260710`.

Evidence:

- Guest thread creation checks `g_pids_max`: `dd-jit-darwin/src/runtime/os/linux/thread.c:1167`.
- Fork paths call host `fork()` without the same limit check: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1478`, `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1979`.

Why this is bad:

The cgroup pids limit is a process/thread resource contract. Enforcing it for guest threads but not processes lets workloads exceed the reported limit and makes pids accounting unreliable.

Isolated proof:

```sh
FILTER=pids-limit-fork
```

Result: failed on both Linux engines. Actual `blocked=0`; expected `blocked=1`.

### Cgroup Memory Usage Is Process-Local

Priority: P1
Impact: parent processes cannot see child memory usage in cgroup files
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AF-envproc-src-20260710`.

Evidence:

- Memory charging is held in process-local `g_mem_charged`: `dd-jit-darwin/src/runtime/os/linux/container/state.c:60`.
- `memory.current` and related cgroup files read that local value: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3356`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3410`.

Why this is bad:

Cgroup memory files report cgroup-wide usage. After fork, dd lets the parent observe zero usage while the child has allocated memory, so supervisors and runtimes under-size or miss memory pressure.

Isolated proof:

```sh
cargo run -q -p dd-tests -- -e aarch64 audit-af-cgroup-mem-fork
cargo run -q -p dd-tests -- -e x86_64 audit-af-cgroup-mem-fork
```

Observed dd: `before=0 during=0 delta=0 ge32m=0`. Linux oracle showed a positive child allocation delta above 32 MiB.

### Network-None Hides `eth0` In Readdir But Direct Lookup Exposes It

Priority: P1
Impact: network isolation probes see contradictory sysfs state
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AM-envproc-20260710`.

Evidence:

- Sysfs network directory listing hides `eth0` when isolation is enabled: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2595`.
- Direct synthetic stat still accepts `eth0`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4610`.
- Direct open/read paths also expose sysfs network files: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1977`.

Why this is bad:

`--network none` should not expose `eth0` through direct sysfs paths if readdir hides it. Tools that probe direct paths can think an interface exists despite isolation.

Isolated proof:

```sh
cargo run -p dd-tests -- -e aarch64 sysnet-none-direct
cargo run -p dd-tests -- -e x86_64 sysnet-none-direct
```

Observed dd: `sysnet_none eth0_list=0 eth0_stat=1:0 eth0_addr=1:0`; expected direct lookups to return `ENOENT`.

### Peer `/proc/<pid>/fd` Is Advertised But Not Openable

Priority: P2
Impact: peer process fd inspection is inconsistent
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AM-envproc-20260710`.

Evidence:

- Per-pid directory materialization includes an `fd` placeholder: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2400`.
- Proc directory open handling only serves pid/task directories: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2472`.
- Peer proc open supports a limited set of files, excluding `fd`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3066`.

Why this is bad:

Tools inspecting peer processes expect `/proc/<pid>/fd` to be listable and direct fd links to be available subject to permissions. dd advertises enough structure to suggest support but then returns `ENOENT`.

Isolated proof:

```sh
cargo run -p dd-tests -- -e aarch64 proc-peer-fd-dir
cargo run -p dd-tests -- -e x86_64 proc-peer-fd-dir
```

Observed dd: `proc_peer_fd dir=0:2 lstat=0:2 readlink=0:2`; native Linux succeeded.

### Peer `/proc/<pid>/ns` Is Absent

Priority: P2
Impact: peer namespace diagnostics cannot inspect live children
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AR-procfs-sysfs-20260710`.

Evidence:

- Peer procfs only serves selected leaves such as `stat`, `status`, `maps`, `cmdline`, and `comm`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3066`.
- Namespace readlink uses self-leaf handling that excludes peer pids: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1347`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2591`.

Why this is bad:

`lsns`, `nsenter`, and diagnostics inspect `/proc/<pid>/ns` for peer processes. dd omits the directory and direct namespace links for live child pids.

Observed proof:

```text
native: peer_ns ns_stat=1 ns_readdir_net=1 net_readlink=1
dd:     peer_ns ns_stat=0 ns_readdir_net=0 net_readlink=0
```

### `/proc/net/unix` Ignores Live AF_UNIX Sockets

Priority: P2
Impact: socket diagnostics miss Unix-domain listeners
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BE2-clean-20260710`.

Evidence:

- dd supports AF_UNIX path sockets: `dd-jit-darwin/src/runtime/os/linux/container/netns.c:1841`.
- `/proc/net/unix` returns only the header: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3490`.

Why this is bad:

Software and diagnostics use `/proc/net/unix` to discover Unix-domain sockets. dd can create a live socket but omit it from procfs, causing stale or empty socket inventory.

Observed proof:

```text
Linux: proc_net_unix has_path=1 ok=1
dd:    proc_net_unix has_path=0 lines=1 ok=0
```

### Bind Mounts Are Missing From Mount Tables

Priority: P1
Impact: mount discovery sees a false namespace
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-procfs-20260710-130826`.

Evidence:

- Bind volume parsing handles guest mount resolution: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:716`.
- `/proc/mounts` and `/proc/self/mountinfo` are generated from synthetic tables: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1795`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3192`.

Why this is bad:

If `/mnt` resolves to a bind mount, `/proc/mounts` and `/proc/self/mountinfo` should expose it. Tools such as `findmnt`, `df`, JVM/container mount discovery, and bind-option checks otherwise see the wrong mount namespace.

Observed proof:

```text
dd:    bind content=1 mounts=0 mountinfo=0
Linux: bind content=1 mounts=1 mountinfo=1
```

### Futex-Blocked Processes Report Running In Procfs

Priority: P2
Impact: process state diagnostics miss sleeping futex waiters
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-wait-futex-20260710`.

Evidence:

- `/proc/<pid>/stat` and `/proc/<pid>/status` render process state from synthetic procfs: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2183`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2236`.
- Other blocking syscalls stamp wait state: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1860`.
- Futex wait does not publish sleeping state: `dd-jit-darwin/src/runtime/os/linux/thread.c:760`.

Why this is bad:

Linux reports a futex-blocked child as sleeping. dd reports `R`, hiding blocked threads from process monitors and deadlock diagnostics.

Observed proof:

```text
qemu: stat_state=S status_state=S sleeping=1 waited=1 exit_ok=1
dd:   stat_state=R status_state=R sleeping=0 waited=1 exit_ok=1
```

### Stale Passing Xfail Comments

Priority: P3
Impact: wrong triage and unnecessary avoidance of working paths
Confidence: Medium-high

Observed in isolated `/Users/x/dd/dd-verify-4b`:

Targeted filters for `seqpacket`, `lockf-fork`, `mincore`, `membarrier`, `close-range`, `adjtimex`, `openat2`, and `bitchurn` now passed even though some comments still imply open gaps.

Why this is bad:

Stale xfail or gap comments hide real regressions and waste agent time. Passing cases should either move to normal coverage or keep an explicit reason for remaining quarantined.

### `/dev/tty` Nonblocking Read Reports EOF Instead Of EAGAIN

Priority: P1
Impact: terminal event loops can treat no input as terminal closure
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-devfs-procfs-20260710`.

Evidence:

- `/dev/tty` is represented by synthetic devfs: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3883`.
- Read handling returns through Linux fs syscall glue: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2076`.

Why this is bad:

With a controlling tty and no available input, Linux nonblocking reads return `EAGAIN`. dd returns zero bytes and no errno, which is EOF semantics; readline, TUI, and event-loop code can exit or tear down the terminal.

Observed proof:

```text
Linux/qemu: tty nb_read=-1 errno=11
dd:         tty nb_read=0 errno=0
```

## Regression Gate Shape

Every completeness finding should aim for one of:

- a dd-tests guest that oracle-diffs dd vs native/qemu,
- a Rust unit test that proves a bad config transform,
- a small API scenario that fails against dd but passes against Docker/native,
- a static guard that fails when a claimed coverage source is missing or empty.
