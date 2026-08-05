# x86 writable projection cache audit

The retained C implementation was inspected read-only across the complete
corresponding write/projection path:

- `../engine/src/translator/guest/x86_64/emit.c`: `emit_memory_guard`,
  `emit_soft_guard`, `emit_soft_store_observe`, and
  `emit_soft_store_commit`;
- `../engine/src/translator/guest/x86_64/translate.c`: `rm_load_access`,
  `rm_store`, and `rm_store_after_guard`, plus their scalar, SIMD, x87,
  atomic, and string-operation callers;
- `../engine/src/translator/guest/x86_64/rep_runtime.c`: scalar and pinned
  `MOVS`/`STOS`, partial completion, fault publication, and alias observation;
- `../engine/src/translator/guest_memory.c`: data resolution, read/write, and
  pin lifetime forwarding;
- `../engine/src/linux_abi/logical_vma.c`: `hl_logical_vma_resolve_data`,
  `hl_logical_vma_pin_data`, and `hl_logical_vma_unpin`; and
- `../engine/src/core/target/x86_64.c`: guest-memory adapters,
  `jit86_store_alias_changed`, and direct-access admission.

The retained logical-VMA table owns mapping identity and backing lifetime
under its mutex. Pins retain a backing reference while host bytes are used;
the lock is not held across the copy. Permission and span checks precede every
store. A successful store is observed afterward so executable aliases can be
retired; faults publish the exact first incomplete address and no completion
is claimed for the failed element. REP operations preserve partial progress.
Ordinary direct mappings bypass the logical indirection but still use the
same pre-store admission and post-store alias observer when required. There
is no cancellation or errno conversion inside emitted code; resolver errors
are converted at the dispatcher boundary. AArch64-host lowering is the native
x86 implementation, while other hosts use the interpreter path.

The current implementation maps those capabilities as follows:

| Capability | Current owner | State |
|---|---|---|
| Mapping lifetime and generation | Rust `ProjectionLease` and x86 `view_publish` | Implemented |
| Bounded direct view lookup | `read_views` publication in `run.c` | Implemented for reads and writes |
| Permission/span proof before mutation | `frontend/memory.c` guards | Implemented |
| Exact dirty owner/range journal | x86 CPU dirty fields and records | Implemented |
| Archive before projection-owner change | x86 writable-view cache | Implemented |
| Capacity rejection before mutation | x86 writable-view cache and dispatcher | Implemented |
| Successful-store publication | scalar, vector, and RMW emitters | Implemented |
| Executable-write sticky latch | scalar, vector, and RMW emitters | Implemented |
| REP partial completion | `run.c::rep_execute` | Implemented separately |

Previously, only reads consumed the four already-authenticated run views.
Alternating writes therefore returned to the dispatcher on every view change,
despite the synchronous lease proving all views for the whole run. The x86
emitter now selects a writable cached view without a host callback. If the
selected view differs from the active dirty owner, it archives the prior exact
record before changing `memory_first`, `memory_last`, `memory_delta`, or
permissions. A full journal deliberately falls through to the existing miss
exit before the guest store. Dirty range, written state, and executable-write
state remain published only after the host mutation succeeds.

The adoption covers the complete emitted writer family: scalar widths 1/2/4/8,
write-qualified loads used by arithmetic RMW, XCHG, XADD, CMPXCHG, and vector
stores. The REP bulk path keeps its existing separately preflighted projection
and exact post-success publication.
