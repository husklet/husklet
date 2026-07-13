# JIT static memory and hot-state audit — wave M (2026-07)

Scope: linked global/BSS, fixed tables, per-thread storage, default-off machinery, duplicated architecture state, and hot-path branches outside the measurement bundle covered by wave D. Figures are source-layout estimates for 64-bit builds; confirm from a linker map because the available Mach-O binaries could not be decoded by the host GNU `size`/`nm`.

## Executive result

Before the wave-D IBPROF reservation, each Linux engine already links roughly 55 MiB of core translation/SMC/pcache tables, excluding the 64-MiB dynamically mapped code cache and the large container/syscall state. Most is justified by correctness or throughput, but it should not all be unconditional BSS:

- translation map: 524,288 × 24 bytes = 12 MiB;
- SMC line set + parallel content hashes: 2 × 2,097,152 × 8 = 32 MiB;
- SMC page set: 262,144 × 8 = 2 MiB;
- shared ARM-style IBTC: 65,536 × 16 = 1 MiB;
- pcache relocation table: 1,048,576 × 8 = 8 MiB in ARM; x86’s packed declaration is also nominally 8 MiB after alignment unless verified otherwise;
- tier-2 counters/owners: 2 × 8,192 × 8 = 128 KiB.

The known IBPROF experiment adds about 60.9 MiB to ARM and remains the first deletion. Removing it would more than halve the linked static reservation. The next safe wins are lazy allocation of pcache-only relocation state and default-off checkpoint/sentry buffers, not shrinking the correctness-critical translation map blindly.

## Core engine tables

| State | Approximate reservation | Hot-path role | Classification |
|---|---:|---|---|
| `g_map[JIT_MAP_N]` | 12 MiB | lookup/insert on dispatch/translation; also crash reverse lookup and persistence | Required performance/correctness cache. Capacity comment gives a concrete 64-MiB arena/load-factor proof |
| `g_txln` | 16 MiB | insert for each translated 64-byte source line; query on guest icache flush | Required SMC correctness/performance for large JIT guests |
| `g_txlh` | 16 MiB | hash read/write only on SMC flush slow path, cleared with `g_txln` | Required to avoid Chromium/V8 unchanged-line invalidation livelock, but can be allocated with SMC tracking |
| `g_txpg` | 2 MiB | insert on translation and coarse query on SMC flush | Potentially redundant now that line tracking is authoritative; needs measurement/proof before deletion |
| `g_ibtc` | 1 MiB | every indirect branch | Required hot cache; do not lazy-allocate or shrink without associativity/miss measurements |
| `g_t2cnt` + `g_t2gpc` | 128 KiB | translation emits counter pointers; hot-loop updates | Default-on tiering state; removable only with tier-2 itself |
| STW registry, retired and freed arrays | implementation-dependent, at least hundreds of KiB | thread registration, generation publication, cache reclamation | Correctness state, but fixed 4,096-thread sizing is duplicated and over-broad |

`g_txpg` and `g_txln` are both populated from translation. The page table was the original precise SMC gate; the newer line table is finer and its saturated lookup already degrades conservatively. Audit call sites to prove the page result is not independently required, then compare translation cost and SMC invalidation counts with page tracking removed. If equivalent, deleting `g_txpg` saves 2 MiB and one insert/probe per newly translated page/line path—both memory and speed improve.

The `TXLN_N` comment is current about the Chromium saturation incident and bounded 512-slot probe, but “~1M lines = 64MB guest code keeps load factor low” no longer describes the cited >2M-line workload. The table is deliberately allowed to saturate and degrade conservatively; rewrite the comment as a bounded-memory tradeoff, not adequate capacity.

## Pcache state

Both engines reserve a one-million-entry relocation table even when persistent cache is disabled. Relocation recording is only useful for persistence; the normal emitter currently also consults/updates bookkeeping to preserve the option. Allocate the table when `DDJIT_PCACHE` is active, or use a chunked vector that is absent otherwise. This saves about 8 MiB RSS/address space for ordinary runs and retains sequential append locality when enabled. The capacity/poison correctness rule must remain: allocation failure or cap exhaustion makes the arena unsavable, never partially relocatable.

The x86 and ARM tables encode the same conceptual record with different declarations and duplicated counters/poison state. Consolidate the record type and allocator in shared pcache support only if it does not disturb target-specific relocation kinds. This is a code-size/maintenance win, not a reason to unify cache file formats.

Required gate: pcache disabled/default, cold-save, warm-load, ASLR-slide, fork/exec, cache flush, and overflow/fault-injection tests; record disabled-run RSS, cold translation time, save time, and warm time. Allocation must occur before the first recorded emitter, never inside the per-instruction hot path.

## Container/syscall and per-thread state

`DD_NFD` is 65,536 and fans out into many always-linked arrays: fd metadata in container state, pipes, leases/signals/dnotify, PTY termios/winsize, epoll pointers/counts/flags/owners/events/udata, signalfd reverse slots, and lock identity. Even just the visible primitive arrays consume multiple MiB. Yet `svc_fill_rlimit` advertises a default `RLIMIT_NOFILE` of 20,480. The comment “far beyond any test/jail guest” attached to sentry’s 1,024 virtual-fd limit is stale relative to the engine-wide 65,536 contract and does not establish compatibility.

Do not simply lower `DD_NFD`: high guest fd numbers are observable ABI. Replace sparse per-fd feature tables with allocation per live OFD/epoll/PTY, or size dense arrays to the configured hard rlimit at container initialization. A single compact per-fd struct can also reduce duplicated indexing and clears. Gate with high-number `dup2`, rlimit raise/lower, epoll/signalfd/PTY/lease/dnotify, fork/exec, and fd-reuse tests; measure syscall latency and RSS at 0, 1K, 20K and 65K fds.

Per-thread fixed state is duplicated: `STW_MAXTHREAD=4096` in cache reclamation and `THREAD_REG_MAX=4096` in thread emulation, plus futex shared-key tables and thread-local iovec/scratch buffers (notably 1,024-iovec and 64-KiB I/O buffers). Use one lifecycle-owned thread record or an indexed shared registry so registration cannot succeed in one table and fail in another. TLS buffers are committed per touching thread; convert 64-KiB syscall scratch buffers to bounded stack/chunked copies or one lazily allocated TLS buffer. Verify signal-stack safety and concurrent vectored I/O before changing them.

## Always-linked default-off machinery

| Machinery | Reservation/cost while off | Proposed treatment |
|---|---|---|
| untrusted sentry | substantial text; fixed process/ring tables; per-thread 1,024-iovec and pselect scratch when touched; 1-MiB protocol buffer constants | Keep capability, but split/link as a feature object or allocate process/ring/buffer state only when sandbox wire flag is set |
| checkpoint/restore | static 64-KiB zero page, 1,024 fd records, 512 restore process records and scratch arrays | Lazy heap allocation when checkpoint/restore directory is set; no normal hot-path allocation |
| forkserver | two function-static 256-KiB request buffers plus live-launch table and path stores | Allocate on server entry; ordinary direct engine should reserve none |
| SysV IPC/message/AIO | fixed metadata tables even when unused; payloads partly dynamic | Initialize metadata lazily on first relevant syscall; keep Linux limit/error semantics |
| x86 AVX trace state | opcode tables and 4,096 RIP records used for optional diagnostics | Delete with the diagnostic if no maintained producer; otherwise lazy allocate trace records |
| GPU IOSurface | mostly dynamic pools, but cached gate branches in VFS/syscall paths | Product feature; keep. Compile-time splitting would risk branch/layout drift for negligible static-data gain |

Feature-object linking is only safe if all engine variants still export the required syscall surface. Prefer lazy data allocation over compile-time omission where a disabled feature must return Linux-compatible errors.

## Duplicated architecture globals

Unity composition forces shared names such as pcache state, trace gates, syscall slimming, tiering and debug compatibility globals to be redeclared per target. Some x86 variables exist solely so shared ARM-oriented dispatcher code compiles and are inert. Delete those with the wave-D/G feature removals. For live shared concepts, move ownership into a small shared engine-state struct or explicit target hook rather than duplicate globals with subtly different semantics (`NOTIER2` versus `NOTIER2X`, differing relocation records, separate syscall-slim parsers).

Do not combine hot ARM and x86 state into one maximal struct merely for aesthetics: target binaries contain one specialization, and direct globals can produce better addressing. Consolidation is justified when it deletes duplicate parsing/state or prevents inconsistent limits, and must be checked at generated assembly/code size.

## Maximal cuts

1. **M1, behavior-neutral after wave D:** remove IBPROF’s predictor/site/snapshot arrays and ARM-B1 hooks together. Expected static drop ~60.9 MiB and fewer translation/dispatcher checks.
2. **M2, pcache-lazy allocation:** replace both one-million-entry static relocation arrays with absent-when-disabled chunked storage. Expected ordinary-run drop ~8 MiB per engine with no hot-path regression.
3. **M3, prove and remove redundant page SMC set:** if line-table-only behavior matches, delete `g_txpg`. Expected drop 2 MiB plus less translation bookkeeping.
4. **M4, lazy default-off subsystems:** checkpoint, forkserver, sentry and optional trace storage allocate only on activation. This preserves external ABI and should improve default RSS/startup.
5. **M5, fd/thread table compaction:** dynamic/sparse per-feature fd state and one thread registry. Highest implementation risk, largest container-state maintenance win.

## Acceptance

For each cut record linked `__TEXT`, `__DATA`, `__BSS`, TLS and file size from a macOS/LLVM linker map before/after; measure launch RSS before guest execution, peak RSS, cold translations/sec, steady dispatch, syscall latency and multithread scaling. Run Rust/C behavior suites for both guest architectures, high-fd/thread limits, SMC/Chromium/V8/Erlang, pcache cold/warm/fork, sandbox, checkpoint and forkserver. A default-off refactor must also exercise allocation failure and repeated fork/exec cleanup. Remove or update the fixed-limit comments in the same change; no source-text-only test is a substitute for these behaviors.
