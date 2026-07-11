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

Status (2026-07-11): BUG 1 FIXED (cross-arch `DD_LOWER` delimiter unified); BUG 2 (escaping) STILL DEFERRED.

FIXED — the latent multi-lower mis-split on x86_64. `DD_LOWER` (singular) was split on `,` in
`linux_x86_64.c` but on `:` in `linux_aarch64.c`, while its ONLY producer — the typed-launch wire pool
(`wire.rs` `lowers.join(":")`, forwarded verbatim by `ddjit_configfd.c` `setenv("DD_LOWER", …)`) — joins with
`:`. So a typed launch (or exec-inheritance) with more than one overlay lower silently mis-parsed on x86_64:
the whole `:`-joined string was taken as a single lower path and the real layers were dropped. Fix (branch
`bugfix/launch-delim`): `linux_x86_64.c` now `strtok_r`s `DD_LOWER` on `:`, unified with `linux_aarch64.c` and
the Rust joiner. Verified the producer chain has no `,`-joined `DD_LOWER` producer (the `,`-joined `DD_LOWERS`
in `spawn_config.rs`/`jail.c` is the SEPARATE darwin-jail plural var, unaffected). `mac cargo build --release
-p dd-jit-darwin -p dd-jit` green. NOTE: the dd-tests harness passes lowers as discrete `--lower <path>` CLI
flags (`spawn_config.rs`), NOT through the `DD_LOWER` env split, so this path is exercised by the daemon
typed-launch / exec-inheritance flow rather than a harness `.lower()` gate; a runtime gate would need to set
`.env("DD_LOWER", "/a:/b")` directly against a real rootfs.

STILL DEFERRED — escaping. `DDVOL`/`DD_LOWER` remain a SHARED contract with MULTIPLE raw producers: the test
harness sets `DDVOL` directly (`dd-tests` `.env("DDVOL", …)`), the CLI sets it via `--vol` (`linux_x86_64.c`
`add_vol`), and the builder/bridge can set it too. Making the C parser escape-aware would silently corrupt
every raw (unescaped) producer; escaping only the `wire.rs` side would desync from the raw ones. A path whose
source contains the delimiter char (`:` or `,`) is still misparsed. A correct escaping fix must update every
raw producer to escape in lockstep with the parser (a backslash-escape keeps delimiter-free paths byte-
identical), then runtime-verify volume/lower mounts on BOTH engines (x86_64 needs container images). Left for
a dedicated escaping pass; do not escape one side in isolation. `publish` is numeric-only and carries no
delimiter hazard.


## Regression Gate Shape

Every completeness finding should aim for one of:

- a dd-tests guest that oracle-diffs dd vs native/qemu,
- a Rust unit test that proves a bad config transform,
- a small API scenario that fails against dd but passes against Docker/native,
- a static guard that fails when a claimed coverage source is missing or empty.
