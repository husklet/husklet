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
| fcntl | `F_SETLEASE` lease-break signal not delivered (residual; `F_NOTIFY` now kqueue-backed, `F_SETLEASE` state/validation fixed) | a lease holder is not notified of a conflicting open | [syscall-compat.md](syscall-compat.md#f_setlease-lease-break-signal-not-delivered-residual) |
| exec env | `envp=NULL` leaks defaults/stale env (FIXED 2026-07-10) | empty-env execs receive unexpected variables | this file |
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
- `S3DB_DURABILITY=none|fast|strict` changes `fsync` durability semantics behind a cached runtime env read. It is a real performance/correctness switch. PINNED 2026-07-10 by `completeness/s3db-durability-{default,none,fast,strict}` (`dd-tests/guests/completeness/s3db_durability.c`): the default path is oracle-diffed byte-identical to native Linux fsync, and each explicit mode is golden-pinned — coherence holds in every mode, `none` is a genuine no-op (returns success without issuing the real fsync), fast/strict issue the real syscall.

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

DISTINCT from Wall 7 (do not conflate). This is `DDJIT_UNTRUSTED`-only and reproduces solely on **x86_64**
(`dd-tests -e x86_64 scm-eventfd-dense-untrusted` fails ~2/3 of runs; aarch64 and BOTH trusted engines pass —
so the trusted cross-fork eventfd counter is fine). The Chrome rendering blocker ("Wall 7", `rendering-engine.md`)
runs in TRUSTED mode and was a different mechanism entirely — a cross-mapping **futex** key bug, now fixed. The
eventfd loss here rides the sentry's SCM_RIGHTS copyback, not the futex path, and is untouched by that fix.


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

Status (2026-07-10): DEFERRED — not minimally safe here. `DDVOL`/`DD_LOWER` are a SHARED contract with
MULTIPLE raw producers, not just `wire.rs`: the test harness sets `DDVOL` directly (`dd-tests` `.env("DDVOL",
…)`), the CLI sets it via `--vol` (`linux_x86_64.c:482` `add_vol(argv[…])`), and the builder/bridge can set it
too. Making the C parser escape-aware would silently corrupt every raw (unescaped) producer; escaping only the
`wire.rs` side would desync from the raw ones. The parse sites are also already INCONSISTENT across arches —
`DD_LOWER` is split on `:` in `linux_aarch64.c:573` but on `,` in `linux_x86_64.c:185`, while `wire.rs:72`
joins lowers with `:` (so a multi-lower typed launch already mis-splits on x86_64, independent of escaping).
A correct fix must first unify the split delimiter across both arch targets AND update every raw producer to
escape in lockstep, then runtime-verify volume/lower mounts on BOTH engines (x86_64 needs container images).
That is too wide to verify safely on this shared engine gate in a minimal pass. `publish` is numeric-only and
carries no delimiter hazard. Left for a dedicated launch-wire pass; do not escape one side in isolation.

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

Status (2026-07-10): BLOCKED — not minimally fixable. Unlike the ns directory (a container shares one
namespace set, so peer ns links are identical to self — now FIXED), a peer's `fd` directory requires the
LIVE per-process fd table of ANOTHER dd worker: each guest process is its own macOS process with a private
fd table, and the cross-process proc registry only publishes comm+argv, not fds. Serving peer `/proc/<pid>/fd`
would need each worker to publish its full guest-fd→target map (updated on every open/close/dup) and the
reader to translate host fds back into guest fd numbers/paths — a new cross-process fd-mirroring subsystem,
not a localized change. Left for a dedicated cross-process-fd pass.



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
