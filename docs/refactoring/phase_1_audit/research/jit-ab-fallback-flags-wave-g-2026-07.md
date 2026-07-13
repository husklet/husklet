# JIT A/B fallback flags — deep audit wave G (2026-07)

Scope: translator/runtime performance fallbacks, their tracked launchers and tests, default/fallback footprints, history, and persistent-cache identity. No code was changed.

## Executive result

Most switches are not supported compatibility modes. They are week-old performance-development scaffolding introduced during the July 3–8 optimization waves. Only `NOSTITCH`, `DDJIT_NOFASTSYS`, `NOSHIFTFLAGELIDE`, `NOXBLOCKFLAGS`, and `NOLAZY` have Rust test producers. The legacy shell bridge forwards `NOSTEAL1617`, `NOSTEALFAST`, `NOIBSLIM`, `NOFUTEXQ`, `NOSTITCH`, `NOSMC`, `NOSMCHASH`, and ARM `NOTIER2`; it does not forward most x86 fallbacks. Documentation inventory entries are not producers.

No inspected default/fallback pair is source-equivalent. Some flags are ineffective in one engine: `NOTIER2` is the ARM gate and is forwarded, whereas x86 uses the distinct, unforwarded `NOTIER2X`; `DDDBG_NOCHAIN` similarly is inert in x86 but is outside this wave. Delete these seams rather than pretending that a generic switch controls both architectures.

The most urgent defect is cache identity. The ARM pcache hashes its major codegen gates. The x86 pcache hashes `NOLAZY`, direct ALU/shift, `NOSTITCH`, `NOREPCMP`, `NOSSEOPT`, `NOEAOPT`, guest-fold, and IRQ layout, but omits code-changing `DDJIT_NOSLIMSYS`, `DDJIT_NOFASTSYS`, `NOTIER2X`, `NOFLAGELIDE`, `NOSHIFTFLAGELIDE`, `NOXBLOCKFLAGS`, and `NOX87OPT`. Therefore a warm arena can make a requested fallback silently reuse default-mode translations (or the reverse). Differential results for those flags are valid only with pcache disabled or separate empty cache directories until the flag is removed or keyed.

## Individual disposition

| Flag/family | Default versus fallback footprint | Producer/test and age | Rank |
|---|---|---|---|
| `NOSTEAL1617`, `NOSTEALFAST` | ARM register allocation, indirect-entry, spill, call/return and scratch sequences differ per translated block | Forwarded; no test producer. Present in current split history on July 4, described as A1 legacy | Differential correctness/perf, then delete together |
| `NOIBSLIM` | ARM call-link and recognized interpreter-dispatch lowering differ; changes emitted pointers/layout and pcache identity | Forwarded; pcache separation test only. July 3 perf wave | Differential correctness/perf |
| `NOIRQSLIM`, `NOIRQCHECK` | Both engines change block entry from inline polling to forward-entry layout (`body+8`); ARM steal state can imply fallback | No launcher; pcache separation test covers `NOIRQSLIM`. July 3 | Keep emergency compatibility until signal/checkpoint/thread stress proves slim entry |
| `DDJIT_NOSLIMSYS` | Both engines choose selective syscall spills versus full register/vector spill at every syscall exit | No producer/test; not forwarded; missing x86 pcache key. Current split history July 4 | Differential correctness first; strong deletion candidate afterward |
| `DDJIT_NOFASTSYS` | x86 inline clock/gettimeofday versus dispatcher syscall; runtime calibration can also disable default | Three Rust slow-path oracle cases. July 3 | Keep emergency compatibility; it is an actively tested safety path |
| `NOFUTEXQ` | ARM per-address wait queues versus one global mutex/condvar; behavior and contention differ, not codegen | Forwarded; no test producer. Current split history July 4 | Keep emergency compatibility until futex race/cancellation/fork stress exists |
| `NOSTITCH` | Both translators choose trace/superblock formation versus single-block/chained fallback; affects flags and tiering | Forwarded; four Rust correctness variants plus scratch decoder artifacts. July 3 | Keep for now; actively finds cross-block bugs |
| `NOTIER2`, `NOTIER2X` | ARM/x86 hot-loop promotion versus tier-1-only execution; separate gates | ARM forwarded; neither tested; x86 gate missing pcache key. Current split history July 4 | Differential correctness/perf, then delete as one cross-arch group or replace by one supported diagnostic control |
| `NOSMC` | Disables translation invalidation for self-modifying guests; fallback can execute stale translations | Forwarded; no producer. Existing comments cite Erlang/Elixir compatibility | Keep emergency compatibility until all JIT-language/SMC regressions are closed; never classify as harmless A/B |
| `NOSMCHASH` | Reverts content-aware invalidation to always-drop behavior | Forwarded; no test; added July 8 | Safe fallback deletion after targeted same-content and changed-content SMC tests; the fallback is only less selective |
| `NOEAOPT` | x86 effective-address folding versus baseline address materialization | No producer/test; missing neither pcache identity (it is keyed). Current split history July 4 | Differential correctness/perf, then delete |
| `NOFLAGELIDE`, `NOSHIFTFLAGELIDE`, `NOXBLOCKFLAGS`, `NOXALUFLAGELIDE` | Several nested x86 flag-liveness optimizations versus eager flag production | Rust tests cover shift-specific and block/lazy masters, not every master; several omitted from pcache key. July 3–4 | Keep tested masters until one consolidated differential suite proves all flag consumers; then delete redundant subordinate gates together |
| `NOSSEOPT` | x86 shuffle/crypto/pmovmskb fast lowering versus helper/block-exit baseline | Benchmark source mentions manual comparison; no Rust producer. Keyed in pcache. July 3 | Differential crypto/SIMD correctness and perf, then delete |
| `NOX87OPT` | x86 statically tracked x87 stack versus helper baseline for uncertain operations | No producer/test; missing pcache key. Current split history July 4 | Differential x87 correctness first; keep until broad x87 oracle exists |
| `NOXALUDIRECT`, `NOXSHIFTDIRECT`, `NOREPCMP`, `NOLAZY` | x86 direct lowering, REP compare, and lazy-flag model revert to older emitted sequences | `NOLAZY` has Rust flag tests; others have no explicit producer; keyed in pcache | Keep `NOLAZY` emergency path; differential-test then delete unproduced subordinate switches |
| `NOGUESTFOLD` | Both engines revert address-bias folding/fixups; changes low-address fault behavior and emitted bytes | Forwarded only indirectly in inventories, no focused producer; pcache-keyed | Keep emergency compatibility for non-PIE/address-layout failures |
| `NOLSE` | ARM atomic lowering avoids LSE instructions | Forwarded; pcache-keyed; hardware/guest compatibility effect | Keep emergency compatibility |
| `NOSTWRECLAIM`, `NOMTIBTC` | ARM disables generation reclamation or threaded IBTC publication | Forwarded; runtime concurrency behavior | Keep emergency compatibility until sanitizer/stress proof, not a performance cleanup target |

Other `NO*` environment variables in filesystem, network, loader, and container code (`NOTMPFS`, `NOSOCKADDR`, `NORELRO`, and similar) are subsystem behavior controls, not translator A/B fallbacks, and are outside this deletion proposal.

## Maximal deletion groups

### G1 — measurement scaffolding removable after one controlled matrix

Remove `NOSTEAL1617` + `NOSTEALFAST` + `NOIBSLIM` as one ARM register/indirect-dispatch legacy group. Their fallback paths overlap and retaining only one creates combinations nobody tests. Before deletion run default and every currently representable combination with pcache off across the ARM C/Rust correctness suite, dynamic loader, signals, clone/fork, computed-goto interpreters, and call/return recursion; measure cold translation, code-cache bytes, dispatcher crossings, and steady interpreter performance. Then run default with warm pcache. Any fallback-only success blocks removal.

### G2 — x86 mature lowering scaffolding

After adding temporary differential coverage, remove `NOEAOPT`, `NOSSEOPT`, `NOXALUDIRECT`, `NOXSHIFTDIRECT`, `NOREPCMP`, and subordinate flag-elision switches. Retain one master (`NOLAZY` or `NOXBLOCKFLAGS`) only while it continues to diagnose correctness. Oracles must compare default and fallback to native/QEMU for integer flags, by-CL shifts, signals between producer/consumer, SSE/AES/crypto, REP termination, unaligned/page-edge addressing, and memory faults. Record translation time, emitted bytes, code-cache pressure, and representative CPython/crypto/string throughput.

### G3 — tiering controls

Unify the proof for ARM `NOTIER2` and x86 `NOTIER2X`, then delete both and their parsing, counters/branches, pcache identity, forwarding, and docs. Required measurements: tier-1-only versus default results for self-loops, nested loops, signals, SMC, threads, pcache cold/warm and code-cache flush; report promotion count, translation pause, cache size, and steady throughput. Until then, fix/avoid the x86 pcache identity hole.

### G4 — low-risk obsolete fallback

`NOSMCHASH` is the best immediate deletion candidate. It was introduced only to compare the content gate with always-invalidating legacy behavior, has no producer, and does not restore missing correctness. First add Rust/C behavioral cases proving same-content writes preserve progress and changed-content writes invalidate/retranslate, including multithreaded execution; compare invalidation counts and throughput. Then delete the flag and legacy always-drop branch while retaining `NOSMC`.

`DDJIT_NOSLIMSYS` is the next candidate, but not “safe now”: syscall spill correctness needs default-versus-full-spill oracles on both architectures, including SIMD registers, errors, signals, clone/fork, and repeated syscalls. Once proved, delete both duplicate parsers and fallback emitters together.

## Acceptance measurements

For every group, use isolated empty pcache directories (or disable pcache), then repeat default warm-cache runs. Build both guest engines and record Mach-O text/data/BSS with `size`, symbol deltas with `nm`, and emitted code-cache bytes. Correctness gates stay in Rust/C: native or QEMU output parity, signal/fault state, threads/futexes, dynamic loading, SMC, and tiering. Performance gates report distributions over repeated runs for cold translation and steady state, not a single elapsed time.

After deletion, grep must find no parser, forwarding entry, pcache bit/hash entry, stale fallback comment, test-only environment producer, or rebrand-inventory promise. If a retained fallback changes emitted bytes, it must have a pcache identity bit and a Rust/C test that demonstrates the fallback branch actually ran.
