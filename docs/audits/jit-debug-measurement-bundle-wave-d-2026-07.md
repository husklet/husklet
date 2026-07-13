# JIT debug/measurement bundle audit — wave D (2026-07)

Scope: every tracked producer and consumer of `IBPROF`, `VDBETRACE`, `VTHITCOUNT`, `CTXDISP`, `MAPDUMP`, `BLKDUMP`, `T2DUMP`, `DD_FAULTCOUNT`, and `DDDBG_*`. This follows `jit-deep-audit-a-2026-07.md`; it makes no code change.

## Result

The bundle is not one coherent supported tool. It is four different classes:

1. abandoned ARM-B1 feasibility experiments (`IBPROF`, `VDBETRACE`, `VTHITCOUNT`, `CTXDISP`);
2. ad-hoc code/map dumpers (`MAPDUMP`, `BLKDUMP`, `T2DUMP`);
3. one-shot fault/layout diagnostics (`DD_FAULTCOUNT`, `DDDBG_IMGBASE`, `DDDBG_INTERPBASE`, `DDDBG_ENGFAULT`);
4. still-useful differential/policy controls (`DDDBG_GPRDUMP`, `DDDBG_NOCHAIN`, `DDDBG_DROPURG`).

No maintained test or script enables any ARM-B1 flag. `dd-tests/guests/ibtc_dispatch.c` only calls itself an IBPROF workload; nothing invokes it with `IBPROF`. `MAPDUMP` has only obsolete `scratch-t186` artifacts, and `DDDBG_ENGFAULT` has only `scratch-erl` repro scripts. Checked-in scratch binaries contain flag strings but are copies, not producers. The rebrand inventory lists names; it does not establish support.

`spawn_config.rs` forwards `IBPROF`, `MAPDUMP`, `T2DUMP`, `DDDBG_GPRDUMP`, and `DDDBG_NOCHAIN` through the legacy shell bridge. Forwarding alone makes a flag reachable, not maintained. The other audited flags are not forwarded by that bridge and have no tracked launcher.

## Exact footprint and disabled-path cost

| Control | Implementation footprint | Cost while unset | Reachability verdict |
|---|---|---|---|
| `IBPROF` | ARM dispatcher hooks, all indirect/return emitters, dump/sort code, 8,192 site records, three 524,288-entry transition tables and three key tables | Translation checks at every indirect branch/return; dispatcher hook checks every crossing. More importantly, approximately 60.9 MiB of static BSS is reserved (transition tables + keys + site/snapshot state) | Forwarded, but no maintained producer; workload comment only. Abandoned experiment |
| `VDBETRACE` | ARM jump-table recognizers, forced trace inlining, SDC emitter/fill/patch path and counters | Conditions in translation and dispatcher crossing; no extra guest instructions when off | No tracked producer. Entirely orphaned experiment |
| `VTHITCOUNT` | SDC hit-counter emission and dump | Translation-time branch; no emitted counter when off | Meaningful only with VDBETRACE, yet independently parsed; entirely orphaned |
| `CTXDISP` | 256 site records plus recognizer/emitter/fill/dump; up to 256 16-KiB in-cache stub arrays (4 MiB when fully used) | Translation and dispatcher checks; no stub arrays emitted while off | No tracked producer. Entirely orphaned experiment |
| `MAPDUMP` | map/code-cache writer plus detached Mach thread-state watcher and trigger protocol | `getenv` at run registration and exit; no steady translated-code branch and no watcher thread | Forwarded, but only obsolete scratch analysis consumes its files |
| `BLKDUMP` | x86 block-word printer | One cached condition per block translation after the first `getenv`; no guest execution cost | Entirely orphaned |
| `T2DUMP` | ARM and x86 tier-2 word printers | `getenv` on every tier-2 promotion | Forwarded generic diagnostic; no tracked producer |
| `DD_FAULTCOUNT` | ARM alternate low-address fault wrapper, 16-bin histogram and exit print | Handler selected once; exit `getenv`; normal handler has no per-fault counting | Entirely orphaned measurement |
| `DDDBG_IMGBASE`, `DDDBG_INTERPBASE` | ARM image/interpreter base overrides | One `getenv` per corresponding ELF mapping | Entirely orphaned, but useful for manual trace alignment |
| `DDDBG_ENGFAULT` | fatal-fault report and symbol lookup | `getenv` only on an already-fatal fault | Only obsolete `scratch-erl` scripts enable it |
| `DDDBG_GPRDUMP` | ARM full-register JT dump; inert compatibility variable in x86 glue | Extra branch only inside the already-enabled JT trace hook | Forwarded and useful for QEMU differential debugging; retain |
| `DDDBG_NOCHAIN` | ARM chain suppression; inert shared-dispatch variable on x86 | One branch after each newly translated block, not per guest execution | Forwarded and useful for exact JT/QEMU alignment; retain |
| `DDDBG_DROPURG` | legacy alias for `DD_SIGURG=drop` | Cached branch in SIGURG policy; environment parsed at first use | Redundant but compatibility-sensitive until SIGURG mitigation is settled |

The IBPROF BSS estimate follows the declarations: 3 × 524,288 × 32-byte transition records (48 MiB), 3 × 524,288 × 8-byte keys (12 MiB), plus site and snapshot records. This is present even when the environment variable is absent. Confirm the linked image rather than relying only on the source arithmetic.

## Maximal safe deletion groups

### Group D1 — remove as one ARM-B1 experiment (highest payoff)

Delete `IBPROF`, `VDBETRACE`, `VTHITCOUNT`, and `CTXDISP` together: state/tables, dump routines, translation recognizers and emitters, dispatcher hooks, pcache poison/id participation, initialization, forwarding, inventory entries, and the stale IBPROF claim in `ibtc_dispatch.c`. Their code is intertwined, none has a maintained producer, and partial removal would leave misleading mode interactions. Keep the production `IBSLIM` behavior; `NOIBSLIM` is a separate A/B fallback and is not authorized by this audit.

Required proof: compare both engine binaries with `size`/`nm` (or a linker map), verify the expected roughly 61-MiB ARM BSS drop, run the Rust/C JIT suites for both guest architectures, exercise persistent-cache cold/hit/fork paths, and compare cold translation plus steady interpreter-dispatch performance. Use multiple runs and report confidence/dispersion; byte equality is neither expected nor a performance proof.

### Group D2 — remove orphaned one-shot diagnostics

Delete `BLKDUMP`, `DD_FAULTCOUNT`, `DDDBG_IMGBASE`, `DDDBG_INTERPBASE`, and `DDDBG_ENGFAULT` together with obsolete scratch consumers. They add no normal-path capability and have no maintained producer. This group is low performance risk; validate both engine builds, fault/signal tests, PIE/non-PIE loading, dynamic interpreter loading, and confirm text/data size does not grow.

### Group D3 — retire the dump protocol only after archiving the workflow

`MAPDUMP` can be removed with its Mach watcher once any still-useful `scratch-t186` decoder knowledge is either discarded or converted into a maintained developer tool. `T2DUMP` can join this group if maintainers confirm it is not their current tier-2 differential aid. Required checks are tier-2 promotion tests, live multithread execution (because MAPDUMP adds Mach suspend/snapshot code), both binaries' sizes, and cold-translation timings. Removing MAPDUMP should eliminate its two disabled-path `getenv` calls; removing T2DUMP eliminates one per promotion.

Keep `DDDBG_GPRDUMP` and `DDDBG_NOCHAIN` for now: together they form a small, coherent QEMU differential-debug path and are explicitly forwarded. Keep `DDDBG_DROPURG` until the `DD_SIGURG` policy and compatibility window are decided; then remove only the alias, not the policy.

## Preprocessor and stale-comment audit

The build scripts add no `-D` for these diagnostics; all relevant selection comes from source-defined target seams and compiler platform macros. `G_OWN_TRAMPOLINES` (x86 versus shared ARM trampolines), `G_CKPT_POLL` (ARM checkpoint polling), `PCACHE_FLUSH_HOOK` (both pcache specializations), and `CANON_X86ONLY` (x86 syscall specialization) are exercised by actual target compositions. They are not unreachable branches.

`__APPLE__` paths are the shipped macOS build; their alternatives remain useful host-portability seams. `__aarch64__`/`__x86_64__` in signal handling reflect the host compiler architecture, not the emulated guest, and should not be mechanically deleted.

`DDJIT_NO_MAIN` is the only suspicious build seam: it guards `main()` in both target files and `os/darwin/jitdarwin.c`, but no tracked build defines it. That makes the suppressed-main branch unexercised in this repository, not proven unreachable for external unity embedding. Before simplifying it, produce preprocessed include graphs for every engine binary and search external/package build recipes; otherwise retain it as an explicit embedding contract.

Stale comments to remove with D1 include the pervasive “ARM-B1 feasibility/prototype” narrative and pcache poison explanations for modes that no longer exist. The `MAPDUMP` comment says “this worktree” and documents issue-specific `#186` machinery as though it were general runtime functionality; remove it with D3 rather than refreshing it. The IBPROF workload claim in `ibtc_dispatch.c` is also stale because the maintained test does not collect that profile.

## Acceptance gate

A deletion is complete only when source, environment parsing/forwarding, pcache mode identity/poisoning, docs, comments, and scratch producers disappear together; Rust/C behavior tests remain green; both guest engines build; linked segment sizes are recorded before/after; and representative cold translation, warm pcache, tier-2, interpreter-dispatch, and multithread workloads show no statistically credible regression.
