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

### `/proc/self/limits` Disagrees With `getrlimit(RLIMIT_CORE)`

Priority: P2
Impact: procfs-scraping tools think core dumps are enabled
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-K-sparse`.

Evidence:

- `getrlimit(RLIMIT_CORE)` returns soft limit `0`: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:190`.
- `/proc/self/limits` reports core size as `unlimited`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2806`.

Why this is bad:

Tools that read `/proc/self/limits` can believe core dumps are enabled while syscalls and wait/core behavior follow soft limit `0`.

Isolated proof:

```sh
mac bash -lc 'E=/Users/x/dd/dd/target/release/build/dd-jit-darwin-16122afd27b6bb64/out/ddjit-linux_aarch64; "$E" target-slot-k/slot_k_rlimit_proc_core_static'
```

Observed: `rlimit_core get=0 soft=0 proc_zero=0`; native oracle passed with `proc_zero=1`.

### `sysinfo(2)` Ignores Container Memory Cap

Priority: P1
Impact: runtimes can oversize heaps/workers under `--memory`
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-K-sparse`.

Evidence:

- `sysinfo.totalram` is hardcoded to 8 GiB: `dd-jit-darwin/src/runtime/os/linux/syscall/misc.c:51`.
- cgroup `memory.max` reports `g_mem_max`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3351`.

Why this is bad:

Some runtimes size memory from `sysinfo`, while others read cgroups. Under a memory cap, those two views disagree and can cause excessive heap sizing.

Isolated proof:

```sh
DD_MEM_MAX=536870912 "$E" target-slot-k/slot_k_sysinfo_memcg_static
```

Observed: `sysinfo total=8589934592 memory.max=536870912`.

### `/proc/version` Is Guest-ISA Blind

Priority: P2
Impact: contradictory platform metadata for x86_64 guests
Confidence: Medium

Evidence:

- `uname.machine` is per guest ISA: `dd-jit-darwin/src/runtime/os/linux/syscall/misc.c:19`.
- `/proc/version` always says `aarch64`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3250`.

Why this is bad:

x86_64 guests can see `uname -m` as `x86_64` while `/proc/version` contains `aarch64`, confusing platform probes and diagnostic tooling.

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

### `/proc/self/environ` Omits Guest Defaults

Priority: P2
Impact: procfs-scraping tools see a different environment than `getenv`
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-R-envproc-20260710`.

Evidence:

- Stack setup merges default guest entries such as `HOME` and `LANG`: `dd-jit-darwin/src/runtime/os/linux/elf.c:831`.
- `DD_GUEST_ENV` entries are merged later in stack construction: `dd-jit-darwin/src/runtime/os/linux/elf.c:868`.
- `/proc/self/environ` is generated from raw `DD_GUEST_ENV` instead of the final stack environment: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1644`.

Why this is bad:

Programs that compare `getenv` with `/proc/self/environ`, or helper processes that inspect procfs, can miss default variables that are actually present in the process environment.

Isolated proof:

```sh
make test FILTER=pf-environ-defaults
```

Result: failed on linux/aarch64 and linux/x86_64; observed `home=1/0 lang=1/0`.

### Guest Exec Truncates Argv At 255 Args

Priority: P1
Impact: silent argument loss across exec and stale `/proc/self/cmdline`
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-R-envproc-20260710`.

Evidence:

- Exec forwarding uses `char *argv[256]` and stops while `ac < 255`: `dd-jit-darwin/src/runtime/os/linux/syscall/proc.c:1618`.
- ELF stack construction also uses fixed argv/envp arrays: `dd-jit-darwin/src/runtime/os/linux/elf.c:861`.
- Procfs command-line state is recorded separately in the proc registry: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1908`.

Why this is bad:

Linux supports far more than 255 arguments within `ARG_MAX`. dd silently drops arguments, so tools with large generated argv lists can execute a different command while procfs metadata can disagree with the expected argv.

Isolated proof:

```sh
make test FILTER=pf-exec-manyargs
```

Result: failed on both Linux engines; observed `argc=255 last=arg252 proc_count=1`, expected `argc=302 last=arg299 proc_count=302`.

### `/proc/self/status` Reports Root Uid/Gid

Priority: P2
Impact: procfs identity disagrees with guest uid/gid
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-R-envproc-20260710`.

Evidence:

- `/proc/self/status` formats `Uid` and `Gid` as zero: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1605`.
- The Linux target runtime reads configured guest uid/gid from `DD_UID` and `DD_GID`: `dd-jit-darwin/src/runtime/targets/linux_aarch64.c:592`.

Why this is bad:

Tools that read `/proc/self/status` for privilege or ownership checks can think the process is root even when guest uid/gid syscalls report a configured non-root identity.

Isolated proof:

```sh
make test FILTER=pf-status-uidgid
```

Result: failed on both Linux engines. aarch64 observed `uid=1001/0 gid=1002/0`.

### Hidden Proc Switches Change Peer Procfs

Priority: P2
Impact: environment-only behavior can hide process files from peers
Confidence: Medium

Evidence:

- `DD_HIDE_CHROME_PROCFILES` changes procfs visibility behavior: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3075`.
- `DD_PROC_CHROME_MODE` changes Chrome-specific procfs handling: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3085`.

Why this is bad:

These switches alter procfs compatibility without an obvious typed launch contract or documented test gate. They can make process discovery and diagnostics differ between otherwise identical runs based only on inherited environment.

### `/sys/fs/cgroup` Root Is Advertised But Not Listable

Priority: P1
Impact: cgroup discovery by directory walk fails
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-W-envproc-20260710`.

Evidence:

- Mountinfo advertises `/sys/fs/cgroup`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1805`.
- Synthetic open/stat paths match children under `/sys/fs/cgroup/` but not the root itself: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1968`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4670`.

Why this is bad:

Runtimes commonly discover cgroup layout by statting and listing the hierarchy root. dd says the mount exists but makes the root unusable, so directory-walk discovery fails even though direct child paths may work.

Isolated proof:

```sh
FILTER=cgroup-root-dir
```

Result: failed on both Linux engines. Actual `cgroup_root stat=0 list=0`; expected `stat=1 list=1`.

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

### `/proc/self/task` Enumeration Omits Live Guest Threads

Priority: P1
Impact: thread enumerators miss live guest TIDs
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AF-envproc-src-20260710`.

Evidence:

- `proc_task_dir_open()` materializes only the main pid entry: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2422`.
- Direct task TID visibility recognizes spawned guest threads: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2456`.
- Guest threads receive TIDs in the runtime thread path: `dd-jit-darwin/src/runtime/os/linux/thread.c:1184`.

Why this is bad:

Linux tools enumerate `/proc/self/task` to discover threads. dd can make direct task paths visible while omitting the same TIDs from directory enumeration, confusing profilers, debuggers, and runtimes.

Isolated proof:

```sh
cargo run -q -p dd-tests -- -e aarch64 audit-af-taskdir-threads
cargo run -q -p dd-tests -- -e x86_64 audit-af-taskdir-threads
```

Observed dd: `entries=1 expected=4 all_listed=0`; Linux oracle listed all four threads.

### `/proc/stat processes` Is Live Count Instead Of Cumulative Forks

Priority: P2
Impact: fork churn and process creation telemetry are wrong
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AF-envproc-src-20260710`.

Evidence:

- `/proc/stat` formats `processes` from proc registry count: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3156`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3189`.

Why this is bad:

Linux reports cumulative forks since boot. dd reports the live registry count, so the value does not increase with fork churn and cannot be used for process creation telemetry.

Isolated proof:

```sh
cargo run -q -p dd-tests -- -e aarch64 audit-af-procstat-processes
cargo run -q -p dd-tests -- -e x86_64 audit-af-procstat-processes
```

Observed dd: `before=1 after=1 delta=0 ge_forks=0`; Linux oracle increased by the fork count.

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

### Closed `/proc/self/fd/N` Reports Stale Existence

Priority: P1
Impact: fd lifecycle probes see closed descriptors as live
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AM-envproc-20260710`.

Evidence:

- `/proc/self/fd/N` readlink checks whether the fd is still open: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4756`.
- Synthetic stat/access treats numeric proc-fd paths as live without the same fd validity check: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2553`.

Why this is bad:

After close, Linux makes `/proc/self/fd/N` disappear. dd returns `ENOENT` for readlink but success for lstat/access, creating stale fd state for runtime probes and cleanup logic.

Isolated proof:

```sh
cargo run -p dd-tests -- -e aarch64 proc-fd-closed-stat
cargo run -p dd-tests -- -e x86_64 proc-fd-closed-stat
```

Observed dd: `proc_fd_closed lstat=1:0 readlink=0:2 access=1:0`; native Linux returned `ENOENT` for all three.

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

### CPU Topology Sysfs Is Direct-Readable But Not Listable

Priority: P1
Impact: topology walkers see an incomplete sysfs tree
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AR-procfs-sysfs-20260710`.

Evidence:

- Sysfs CPU directory materializes `/sys/devices/system/cpu` and `cpuN` but not `topology`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2650`.
- Direct topology file reads are served: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2001`.
- Stat advertises the topology dir/files: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4631`.

Why this is bad:

Tools such as `lscpu` enumerate topology directories before opening direct files. dd reports direct files as readable while hiding them from readdir, so topology discovery is incomplete.

Observed proof:

```text
native: cpu_topology topo_stat=1 topo_readdir_core=1 core_read=1
dd:     cpu_topology topo_stat=1 topo_readdir_core=0 core_read=1
```

### `/proc/self/ns` Is Missing While Namespace Links Work

Priority: P1
Impact: namespace tools fail enumeration before readlink
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AR-procfs-sysfs-20260710`.

Evidence:

- Self pid directory materialization omits `ns`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2388`.
- `readlink("/proc/self/ns/<name>")` is synthesized separately: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2603`.

Why this is bad:

Namespace tools enumerate `/proc/self/ns` and then read each link. dd supports the direct `net` readlink but makes the containing directory absent, causing tools to fail or miss namespace metadata.

Observed proof:

```text
native: proc_ns ns_stat=1 ns_readdir_net=1 net_readlink=1
dd:     proc_ns ns_stat=0 ns_readdir_net=0 net_readlink=1
```

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

### Cgroup Controllers Advertised But Required Files Missing

Priority: P1
Impact: cgroup v2 controllers look available but standard files are absent
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BE2-clean-20260710`.

Evidence:

- `cgroup.controllers` advertises `cpuset cpu io memory pids`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3374`.
- `/sys/fs/cgroup/*` paths route to synthetic proc open: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1968`.
- The cgroup file list omits `cpuset.cpus.effective`, `cpuset.mems.effective`, and `pids.peak`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3351`.

Why this is bad:

Runtimes read advertised controller files to configure and inspect resource state. Advertising controllers while omitting required files causes feature probes and cgroup walkers to fail.

Observed proof:

```text
Linux: advertises=1 cpu_eff=1 mem_eff=1 pids_peak=1 ok=1
dd:    advertises=1 cpu_eff=0:2:- mem_eff=0:2:- pids_peak=0:2:- ok=0
```

### `/proc/self/status` Threads Is Hardcoded To One

Priority: P1
Impact: status summary hides live pthreads
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-BE2-clean-20260710`.

Evidence:

- Self status is rendered in procfs: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1589`.
- `Threads:\t1` is hardcoded: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1608`.

Why this is bad:

Tools read `/proc/self/status` for thread counts even when they do not enumerate `/proc/self/task`. dd reports one thread while pthreads are live, hiding concurrency from diagnostics and runtimes.

Observed proof:

```text
Linux: status_threads threads=4 expected_ge=4 ok=1
dd:    status_threads threads=1 expected_ge=4 ok=0
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

### `/proc/net` Direct Leaves Exist But Directory Is Not Enumerable

Priority: P1
Impact: network diagnostics miss supported procfs leaves during directory walks
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bk2-audit-20260710`.

Evidence:

- `/proc` directory opening only materializes numeric pid directories: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1920`.
- Proc directory listing omits `net`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2472`.
- Direct `/proc/net/tcp` and `/proc/net/dev` leaves are still synthesized: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3253`.

Why this is bad:

Tools usually enumerate `/proc/net` before opening individual network files. dd exposes direct leaves but hides the containing directory, producing contradictory feature detection.

Observed proof:

```text
dd:    procnet_dir dir=0 tcp=0 dev=0 sockstat=0 direct_tcp=1 direct_dev=1
Linux: procnet_dir dir=1 tcp=1 dev=1 sockstat=1 direct_tcp=1 direct_dev=1
```

### `/proc/net/sockstat` And `sockstat6` Are Missing

Priority: P1
Impact: socket accounting probes and diagnostics fail
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bk2-audit-20260710`.

Evidence:

- `/proc/self/net/*` mirrors fold to `/proc/net/*`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3007`.
- The synthesized net leaves omit `sockstat` and `sockstat6`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3253`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3281`.

Why this is bad:

Linux provides socket accounting through both `/proc/net/sockstat` and `/proc/self/net/sockstat`. dd reports neither, so runtimes and diagnostics can mis-detect socket pressure or feature availability.

Observed proof:

```text
dd:    procnet_sockstat sockstat=0 self_sockstat=0 sockstat6=0
Linux: procnet_sockstat sockstat=1 self_sockstat=1 sockstat6=1
```

### Cgroup V2 Omits Additional Standard Controller Files

Priority: P1
Impact: advertised controllers still lack standard inspection/control files
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bk2-audit-20260710`.

Evidence:

- `/sys/fs/cgroup/*` paths route through synthetic proc open: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1968`.
- The cgroup v2 file set omits `memory.oom.group`, `pids.events`, `pids.events.local`, `memory.swap.events`, `memory.swap.peak`, and `cpu.stat.local`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3374`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3398`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3444`.

Why this is bad:

This is separate from the earlier missing `cpuset.*.effective` and `pids.peak` cases. Tools that inspect cgroup v2 controller state expect these files when the controllers are advertised.

Observed proof:

```text
dd:    cgroup_more controllers=1 oom_group=0 pids_events=0 pids_events_local=0 swap_events=0 swap_peak=0 cpu_stat_local=0
Linux: cgroup_more controllers=1 oom_group=1 pids_events=1 pids_events_local=1 swap_events=1 swap_peak=1 cpu_stat_local=1
```

### `/proc/self/task/<tid>` Lists Files Direct Lookup Cannot Open

Priority: P1
Impact: task procfs walkers see entries that cannot be stat/opened
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-audit-BN`.

Evidence:

- Task TID directories are materialized: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2424`.
- `task/<tid>` directory listing reuses the generic per-pid list containing `status`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2388`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2506`.
- Direct proc open handles only `task/1/maps`, not `task/<tid>/status`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3038`.
- Stat fallback depends on `proc_open`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4771`.

Why this is bad:

Linux lets tools open files listed under each task directory. dd lists `status` under `/proc/self/task/<tid>` but fails direct stat/open, creating a broken enumerable tree.

Observed proof:

```text
Linux: task_listed=1 dir_stat=1 dir_open=1 status_listed=1 status_stat=1 status_open=1
dd:    task_listed=1 dir_stat=1 dir_open=1 status_listed=1 status_stat=0 status_open=0
```

### `/proc/self` Readdir Omits Direct-Supported Proc Files

Priority: P1
Impact: procfs feature discovery misses files that direct lookup supports
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-audit-BN`.

Evidence:

- `/proc/<pid>` listing is a hard-coded set omitting `mountinfo`, `limits`, `environ`, `smaps`, and `pagemap`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2388`.
- Direct open supports those files: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3031`.
- Synthetic stat confirms direct-supported proc files by opening them: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4771`.

Why this is bad:

Tools commonly discover proc files via readdir. dd supports direct access but hides the same files from `/proc/self`, so directory-based discovery and direct probing disagree.

Observed proof:

```text
Linux: mountinfo/limits/environ/smaps/pagemap listed=1 stat=1 open=1
dd:    mountinfo/limits/environ/smaps/pagemap listed=0 stat=1 open=1
```

### `/proc/self/fdinfo` Is Missing

Priority: P2
Impact: fd diagnostics and event-loop introspection lose per-fd metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-audit-BN`.

Evidence:

- Per-pid listing creates `fd` but not `fdinfo`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2396`.
- Synthetic stat special-cases `fd` and `fd/N`, not `fdinfo`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4748`.
- Direct proc open has no `fdinfo` handling: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3027`.

Why this is bad:

Linux exposes `/proc/self/fdinfo` and per-fd entries. Runtimes use it for descriptor flags, positions, eventfd counters, and epoll details. dd exposes `/proc/self/fd` but omits fdinfo entirely.

Observed proof:

```text
Linux: fdinfo root_listed=1 root_stat=1 root_open=1; fdinfo_entry listed=1 stat=1 open=1
dd:    fdinfo root_listed=0 root_stat=0 root_open=0; fdinfo_entry listed=0 stat=0 open=0
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

### `/proc/self/smaps` Can Hang On Read

Priority: P1
Impact: memory diagnostics can block indefinitely
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-procfs-20260710-130826`.

Evidence:

- Smaps generation walks synthetic mapping data: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1388`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1493`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1555`.
- Direct proc open serves `smaps`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3038`.

Why this is bad:

Opening and reading `/proc/self/smaps` should return promptly. Memory profilers, Redis/kernel COW checks, JVM/native diagnostics, and opportunistic probes can hang under dd.

Observed proof:

```text
dd:    timeout after 10 seconds reading /proc/self/smaps
Linux: smaps_read n=3991
```

### `/sys/class/block` And `/sys/block` Are Absent

Priority: P2
Impact: storage diagnostics and installers see missing block sysfs
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-procfs-20260710-130826`.

Evidence:

- Sysfs directory materialization covers only selected classes/devices: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2577`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2650`.
- Synthetic stat has no block sysfs support: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4607`.

Why this is bad:

Linux exposes `/sys/class/block` and `/sys/block` inside containers. Tools such as `lsblk`, storage diagnostics, and installers that test block sysfs get `ENOENT` under dd.

Observed proof:

```text
dd:    sysblock class_dir=0 class_n=-2 block_dir=0 block_n=-2
Linux: sysblock class_dir=1 class_n=13 block_dir=1 block_n=12
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

### `statfs` Is Wrong For Synthetic Proc/Sys Leaves

Priority: P1
Impact: filesystem probes misclassify procfs/sysfs leaves as missing or tmpfs
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-statfs-audit-20260710`.

Evidence:

- `statfs` and `fstatfs` route through Linux syscall glue: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1360`, `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1377`.
- Synthetic proc/sys leaf resolution lives in the VFS layer: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1192`.

Why this is bad:

Linux reports procfs/sysfs magic and pseudo-fs block data for paths such as `/proc/meminfo` and `/sys/class/net/lo/mtu`. dd returns `ENOENT` for path `statfs` and reports tmpfs-like host block data for fd `fstatfs`, so tools that detect pseudo-filesystems by magic or mount flags silently take the wrong path.

Observed proof:

```text
dd:    statfs(/proc/meminfo)=-1 errno=2; fstatfs(open)=0 type=1021994
Linux: statfs(/proc/meminfo)=0 type=9fa0; fstatfs(open)=0 type=9fa0
```

### `statfs.f_flags` Is Always Zero

Priority: P1
Impact: mount flags disappear from filesystem compatibility probes
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-statfs-audit-20260710`.

Evidence:

- `statfs` fills the returned flags field with zero: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:1414`.

Why this is bad:

Linux exposes meaningful flags for procfs, sysfs, devtmpfs, shm, and tmpfs. dd reports zero for `/proc`, `/sys`, `/dev`, `/dev/shm`, and `/tmp`, so software that checks `ST_NOSUID`, `ST_NODEV`, `ST_NOEXEC`, or read-only status gets a false mount view.

Observed proof:

```text
dd:    /proc flags=0 /sys flags=0 /dev flags=0 /dev/shm flags=0 /tmp flags=0
Linux: /proc flags=102e /sys flags=1020 /dev flags=1020 /dev/shm flags=26
```

### `/proc/self/io` Is Missing

Priority: P1
Impact: IO accounting probes and language runtimes see absent Linux procfs data
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-statfs-audit-20260710`.

Evidence:

- Direct proc file dispatch does not include `io`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3027`.

Why this is bad:

Linux containers expose `/proc/self/io` with fields such as `rchar` and `read_bytes`. dd returns `ENOENT`, breaking monitoring agents and libraries that use procfs IO counters opportunistically.

Observed proof:

```text
dd:    open(/proc/self/io)=-1 errno=2
Linux: open(/proc/self/io)=3 contains rchar/read_bytes fields
```

### `/dev/fd` Symlink Cannot Be Enumerated

Priority: P2
Impact: POSIX fd-discovery paths fail through a standard compatibility symlink
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-statfs-audit-20260710`.

Evidence:

- `/dev/fd` resolves to `/proc/self/fd`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:4111`.
- Directory support does not make that target enumerable through the symlink path: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3030`.

Why this is bad:

Linux lets applications enumerate `/dev/fd` as a standard alias for `/proc/self/fd`. dd resolves the symlink but `opendir("/dev/fd")` fails, breaking shell, libc, and scripting-language fd discovery paths.

Observed proof:

```text
dd:    readlink(/dev/fd)=/proc/self/fd; opendir(/dev/fd)=-1 errno=2
Linux: readlink(/dev/fd)=/proc/self/fd; opendir(/dev/fd)=ok with fd entries
```

### `/proc/self/maps` Omits RELRO Mapping Detail

Priority: P2
Impact: memory layout diagnostics see an impossible executable image shape
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-statfs-audit-20260710`.

Evidence:

- Maps rendering is synthesized from internal mapping data: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1407`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:1444`.

Why this is bad:

Static PIE binaries on Linux expose read-only RELRO/load segments and vdso mappings. dd reports only executable and writable rows for the executable, so crash reporters, profilers, and hardening probes cannot reconstruct the real layout.

Observed proof:

```text
dd:    executable rows include r-xp and rw-p only
Linux: executable rows include r--p plus vdso mapping
```

### `/proc/meminfo` And `/proc/stat` Are Sparse

Priority: P2
Impact: common procfs consumers see missing or zeroed accounting fields
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-procfs-statfs-audit-20260710`.

Evidence:

- Synthetic procfs emits short meminfo/stat content: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3150`, `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3189`.

Why this is bad:

Linux exposes many standard `/proc/meminfo` fields and live `/proc/stat` counters. dd emits a sparse meminfo and zeroes key stat fields such as `intr` and `ctxt`, which can disable monitoring heuristics or feed bad capacity data to applications.

Observed proof:

```text
dd:    meminfo has 9 lines and omits Active/Inactive/Dirty/AnonPages; proc/stat has intr 0 ctxt 0
Linux: meminfo includes those fields and proc/stat counters are nonzero
```

### `/dev/urandom` Writes Fail With EPERM

Priority: P1
Impact: entropy seeding probes see a false permission failure
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-devfs-procfs-20260710`.

Evidence:

- `/dev/urandom` is represented by the synthetic devfs table: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3881`.
- Device writes return through syscall write handling: `dd-jit-darwin/src/runtime/os/linux/syscall/fs.c:2073`.

Why this is bad:

Linux accepts writes to `/dev/urandom` as seed input. dd returns `EPERM`, so compatibility probes and libraries that opportunistically mix seed material can fail or switch to degraded behavior.

Observed proof:

```text
Linux/qemu: w1=1 wv=2
dd:         w1=-1 ew1=1 wv=-1 ewv=1
```

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

### `/proc/tty` Surface Is Absent

Priority: P2
Impact: tty discovery tools see missing kernel metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-devfs-procfs-20260710`.

Evidence:

- `/proc` root static entries omit `tty`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:2527`.

Why this is bad:

Linux exposes `/proc/tty` and readable metadata such as `/proc/tty/drivers`. dd returns `ENOENT`, so procfs walkers and tty discovery tools underreport terminal support.

Observed proof:

```text
dd:    /proc/tty, /proc/tty/drivers, /proc/tty/driver, /proc/tty/driver/serial -> ENOENT
Linux: /proc/tty exists and /proc/tty/drivers is readable
```

### `/proc/devices` Has Empty Block Device Section

Priority: P2
Impact: device-major discovery underreports block device classes
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-devfs-procfs-20260710`.

Evidence:

- `/proc/devices` content is synthesized with an empty block-device section: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3481`.

Why this is bad:

Linux and qemu expose block majors such as `loop` in `/proc/devices`. dd prints `Block devices:` with no entries, so installers and diagnostics that inspect major numbers get a false device surface.

Observed proof:

```text
Linux/qemu: has_block_loop=1
dd:         has_block_loop=0
```

## Regression Gate Shape

Every completeness finding should aim for one of:

- a dd-tests guest that oracle-diffs dd vs native/qemu,
- a Rust unit test that proves a bad config transform,
- a small API scenario that fails against dd but passes against Docker/native,
- a static guard that fails when a claimed coverage source is missing or empty.
