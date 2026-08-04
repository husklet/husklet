# AArch64 guarded cycle audit

This audit records the retained implementation studied before constraining
cyclic native chains. The imported Rust cache at `8512e5e1c` already patched
resolved cycles without qualification; `8c5b1283f` did not introduce cycle
closure. It added the guarded-safety qualification described below. The
read-only oracle revision was
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`.

## Retained implementation

The complete relevant call path is:

- `../engine/src/core/dispatch.c`: `run_guest` owns lookup, translation,
  publication, execution, reason handling, and the JIT-lock ordering around
  cache mutation. A translated return is a settled guest-state boundary.
- `../engine/src/translator/cache.c`: `map_body`, `map_put`, `add_pend3`, and
  `patch_links_to` own guest-PC identity and direct-edge lifetime. Pending
  edges become direct branches only after target publication and instruction
  cache synchronization. Generation reset, source invalidation, and retired
  arena reclamation bound their lifetime.
- `../engine/src/translator/guest/aarch64/stubs.c`:
  `emit_chain_exit_from` emits an existing direct edge or records a pending
  edge. Its normal mode permits cycles. Once self-modifying code is observed,
  it instead routes removable edges through the shared indirect cache.
- `../engine/src/translator/guest/aarch64/translate.c`: `translate_block`
  places `emit_irq_check` at chained-body entry. `emit_selfloop` and
  `tier2_promote` retain polling while folding a hot conditional back-edge.
  The tier-two mutation is single-threaded; ordinary block publication and
  chaining are serialized by the JIT lock.
- `../engine/src/translator/guest/aarch64/dispatch.h`: `G_IBTC_FILL` publishes
  indirect target/body identity. Threaded publication uses an atomic pair;
  single-threaded publication may additionally patch a per-site cache.

The retained state consists of a generation-qualified translation map, a
bounded pending-edge table, resolved executable bodies, an indirect target
cache, per-thread CPU state, and arena generation ownership. Direct edges
carry no independent lifetime: invalidation makes their source unreachable or
restores/removes ingress before reclaiming the target generation. Every
in-cache cycle remains interruptible because its backward or indirect edge
enters a polled block header. Syscalls and faults fully spill before returning
to the dispatcher.

## Native comparison

The Rust-owned native implementation uses executor-owned arena and cache state:

- `src/arch/aarch64/trace.c::trace_build` emits an interrupt-token and budget
  checkpoint at every published chained-body entry.
- `src/arch/aarch64/stub.c::hl_a64_stub_budget_begin` and
  `hl_a64_stub_budget_finish` preserve NZCV, charge the complete trace, and
  fully spill on interrupt or budget exhaustion.
- `src/executor.c::run_aarch64` authenticates mapping and direct-memory
  authority before entry. The execution admission gate prevents cache mutation
  until the translated return is fully spilled.
- `cache/relocation.c` patches resolved direct edges under exclusive cache
  write ownership and restores them during source/target invalidation.

Unqualified cyclic relocation is unsafe because the cache is also used by the
x86 frontend and by synthetic clients whose body entry need not poll. Each
published block therefore carries a monotone `cycle_safe` capability. Only the
AArch64 trace builder sets it, after placing the interrupt and budget guard at
the exact `body_offset` targeted by relocations. A candidate cycle is admitted
only when the source, target, and every entry examined on the resolved path are
qualified. A missing identity, an unsafe entry, or fixed-frontier saturation
retains the original typed dispatcher edge. Acyclic relocation behavior is
unchanged.

This does not weaken partial-result, syscall, fault, cancellation, or errno
semantics: the optimization joins only already-published control edges and
does not cross a typed non-branch exit. Direct-memory authority and mapping
incarnation remain part of cache identity. Invalidation still restores incoming
relocations before retiring their target.

## Validation scope

No performance improvement is attributed to this change. The imported cache
already closed fully resolved AArch64 cycles, including the direct loops used
by `memcpy` and `memcmp`. The new policy can only preserve that existing
closure for guarded-safe graphs or retain a typed exit for a graph that is not
proved safe.

`test/a64_cycles.c` is the focused acceptance test. It proves:

- a fully qualified two-entry cycle closes both edges;
- a mixed qualified/unqualified cycle retains exactly one typed edge;
- a real conditional self-loop runs until exact budget exhaustion without a
  branch boundary and observes an interrupt before executing another member;
- a real two-block cycle has no steady-state branch boundary; and
- invalidating one member restores its incoming edge, incurs one dispatcher
  boundary for reconstruction, and preserves the exact budget result.

These checks validate the safety classification and existing chaining behavior;
they are not benchmark evidence and do not establish a wall-time change.
