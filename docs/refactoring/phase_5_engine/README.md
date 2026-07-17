# Phase 5: portable Linux execution engine

Status: researched against main on 2026-07-13. Phase 5 is a migration design, not authorization to move
or rewrite runtime code yet.

The end state is a standalone, pure-C `engine/` project that runs Linux guests through a translator and
Linux ABI model which never name a host OS. Host-specific code implements one versioned service table per
host. The Rust `hl-engine` crate owns safe bindings, artifact discovery, launch and supervision; it does
not reimplement Linux syscall semantics.

```text
Linux ELF (aarch64 or x86_64)
        |
        v
translator: guest ISA -> internal IR -> selected host-CPU backend
        |
        v
linux-abi: Linux syscalls, errno, processes, OFDs/fds, /proc, /sys
        |
        v
host-services ABI
        +-- host-macos
        +-- host-linux
        `-- host-windows
```

## Documents

- [`current-state.md`](current-state.md) — validated implementation, coupling and compatibility surface.
- [`target-architecture.md`](target-architecture.md) — directory/library layout, dependency rules and
  platform matrix.
- [`host-services-api.md`](host-services-api.md) — concrete C ABI design for the portability seam.
- [`surface-api.md`](surface-api.md) — engine lifecycle and Rust `hl-engine` binding contract.
- [`../engine-extension-capabilities.md`](../engine-extension-capabilities.md) — product-facing high/low-level API,
  mount/volume/device/provider primitives and required Linux ABI facility matrix.
- [`api-gap-matrix.md`](api-gap-matrix.md) — current exposed API versus required API and the first migration seam.
- [`migration-plan.md`](migration-plan.md) — ordered extraction with rollback and acceptance gates.
- [`testing-and-performance.md`](testing-and-performance.md) — C/Rust test ownership and non-regression method.
- [`validation-ledger.md`](validation-ledger.md) — claims checked in code/history and unresolved decisions.

## Non-negotiable outcomes

1. `engine/` builds with a C11 compiler and no Cargo dependency. Public headers are C and C++ compatible,
   but engine implementation remains C.
2. `translator` performs no filesystem, process, event, network or host-OS calls. `linux-abi` performs no
   direct host calls. Host headers do not leak above `host/<platform>`.
3. Linux guest-visible fds, OFDs, pids, errno, time, signals, `/proc` and `/sys` remain Linux models; they
   are not aliases for host values.
4. Existing aarch64 and x86_64 Linux behavior and performance remain the migration oracle until equivalent
   engine-owned C tests exist.
5. `hl-engine` tests only the public C surface, ownership and error mapping. Linux compatibility and JIT
   performance tests live with `engine/`.
6. Each extraction step produces linkable libraries and keeps a working runner. No big-bang rewrite and no
   temporary duplicate implementation may become a second truth.

The existing native macOS guest is not part of the portable Linux personality. It remains a separately
packaged compatibility component until Phase 3/product policy explicitly retires it; Phase 5 must not delete
it incidentally.
