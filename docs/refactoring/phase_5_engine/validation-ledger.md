# Validation ledger

Checked against main on 2026-07-13. “Validated” means code/build/history evidence exists; it does not mean the target
architecture has been implemented.

| Claim | Evidence | Disposition |
|---|---|---|
| current engine implementation is C | runtime inventory contains C/headers and no C++ implementation | keep C11; public headers wrap `extern "C"` for C++ consumers |
| current engine is already portable | build invokes macOS Clang/frameworks/codesign; targets include Mach, kqueue and cache-control APIs | false; portability requires service extraction |
| `dd-jit` is host-agnostic | manifest and runtime directly depend on/use `dd-jit-darwin` | false; replace low-level dependency with `hl-engine` |
| there is only one guest OS | public `Guest` exposes Linux and Darwin personalities | false today; make Linux the portable target and quarantine Darwin compatibility |
| Linux ABI is shared across guest ISAs | both Linux unity targets include common container/syscall/thread/signal files | substantially true; preserve and formalize architecture hooks |
| translator is host-agnostic IR today | both frontends directly depend on ARM64 emission and host/macOS mechanisms; no complete neutral IR exists | false; migrate IR incrementally with direct adapter |
| existing C surface is enough | only spawn/config plus target-specific `dd_run`/`main`; no engine instance or host-service ABI | reuse skew-safe wire ideas, design lifecycle/services explicitly |
| engine can safely link in-process now | globals, process-wide handlers, fork/TLS and `_exit` exist | false; library-backed runner first |
| tests already belong to engine | most compatibility orchestration/fixtures remain under `dd-tests`; crate tests cover wire/selection | false; migrate behavior with all fixtures and runners |
| old audit/WIP branches can be merged | `codex/jit-deep-audit-a` contains useful docs but diverges broadly from current main | evidence only; do not cherry-pick wholesale |

## Existing branch work reviewed

- `codex/jit-deep-audit-a` and its audit commits document unity reachability, environment/wire ownership, static state,
  syscall dispatch, fallback flags and unsafe FFI ownership. The relevant documents are already present under Phase 1.
- `bugfix/jit-*`, syscall/completeness branches and current main history contain compatibility fixes that define the
  baseline: signal frames, SMC, pcache identity, high-fd events, OFD sharing, process namespaces and error behavior.
- `epoll-multiproc-fix`/Chrome-related branches demonstrate why event readiness must be a semantic host boundary and
  carry a cross-process live gate.

## Decisions required before implementation

1. Confirm whether native Darwin guest support is retained, separately packaged or explicitly retired.
2. Choose the authoritative standalone C build (CMake recommended; a small Make wrapper is acceptable).
3. Set initial supported host matrix. Recommended first proof is Linux/arm64 because it reuses the current host-CPU
   code generator while exercising a second host-services backend.
4. Decide whether `hl-engine` is the final rebranded public Rust runtime name or only the low-level binding crate.
5. Define release performance machines/workloads and numeric budgets before structural code changes.
6. Decide the minimum engine configuration payload for ABI v1; preserve the existing wire during transition rather
   than redesigning launch and portability simultaneously.

## Completion condition

Phase 5 planning becomes implementation-ready when these decisions are recorded and Wave 0 has executable baseline
commands/artifacts. The phase itself completes only when the old runtime source/build can be deleted, `engine/` builds
and tests independently, `hl-engine` is the sole Rust binding, and at least macOS plus Linux host backends pass the
same Linux guest compatibility suite.
