# x86 REP bulk migration audit

This audit records the retained-C oracle studied before changing the Rust/native
REP path. The retained tree is read-only at `/Users/x/dd/engine`.

## Retained implementation studied

- `src/translator/guest/x86_64/rep_runtime.c`: `hl_x86_rep_movs`,
  `hl_x86_rep_stos`, the scalar and pinned MOVS/STOS loops,
  `rep_element_read`, `rep_element_write`, and `rep_fault`.
- `src/translator/guest/x86_64/lower/repstr.c`:
  `hl_x86_lower_repstr` and `emit_rep_string`.
- `src/translator/guest/x86_64/rep.c`: `hl_x86_rep_compare`.
- `src/translator/guest/x86_64/interp.c`: string instruction cases and
  `run_block` dispatch.
- `src/translator/guest_memory.c`: indirect read, write, pin, and unpin seams.
- `src/core/target/x86_64.c`: guest-memory adapters, validator binding,
  soft-TLB misses, and store observation.
- `src/linux_abi/logical_vma.c`: `hl_logical_vma_pin_data` and
  `hl_logical_vma_unpin`.
- `src/translator/guest/x86_64/emit.c`: deferred executable-store observation
  and drain.

## Oracle contract

The AArch64-host translator lowers REP MOVS and STOS, opcodes A4/A5 and AA/AB,
at widths 1, 2, 4, and 8 into bulk helpers when there is no segment override or
32-bit address size. Non-REP forms and LODS remain scalar. CMPS and SCAS have a
separate flag- and early-stop-aware helper and are not blind bulk copies.
Non-AArch64 hosts execute MOVS/STOS/LODS elementwise.

The emitted helper spills architectural state and passes original RDI, RSI or
RAX, RCX, direction, and the faulting RIP. It returns completed elements. The
epilogue advances RDI and RSI by signed `completed * width`, subtracts completed
from RCX, and leaves flags unchanged. A partial fault records the current guest
address, width, access, REP RIP, and soft-miss reason before those exact register
updates. Retry therefore resumes the same instruction with residual RCX. MOVS
orders each element's source read before destination write. Zero count completes
without touching memory.

Forward indirect MOVS pins source READ before destination WRITE, copies only the
minimum contiguous whole-element span, then unpins destination and source.
Forward STOS similarly pins a write span. Pin lookup holds the VMA mutex only
while validating and acquiring a backing reference; copy/fill runs unlocked;
unpin reacquires the mutex and releases the reference. Every error path unpins.
Pins stop at a VMA boundary and the outer loop reacquires the next span. Unsafe
overlap and backward indirect operations use exact scalar element order.

The direct bulk path validates the complete source range before the destination
range. Guest-memory indirection performs permission checks in the resolver;
otherwise the direct validator runs before raw access. A malformed or denied
pin faults at the current element. Forward overlap deliberately smears at the
architectural element width rather than using `memmove`; backward operations
copy or fill highest-to-lowest. STOS writes the low width bytes of RAX.
Non-PIE rebasing affects host dereferences only, never guest register values.

Successful scalar stores are observed per element. Bulk stores publish one
range only after the memory operation and after pins are released. Executable
alias work is drained only after RDI, RSI, and RCX describe completed progress,
so an SMC exit cannot replay or skip stores.

The retained helper has no instruction-budget accounting and does not poll
signals or cancellation inside a large operation. Signals are handled at block
or dispatcher boundaries. Rust intentionally improves bounded responsiveness:
it caps chunks, charges completed elements to the budget, and observes interrupt
state before each chunk while preserving the same partial-progress contract.

## Rust ownership and capability map

`src/native/exec/src/arch/x86_64/run.c` owns the bounded MOVS/STOS fast path.
`rep_decode` admits the relevant REP widths, `rep_span` bounds work to the current
view and one MiB, `rep_copy` and `rep_fill` preserve overlap and direction, and
`hl_x86_projection_resolve` plus `hl_x86_projection_written` own permission and
dirty publication. The synchronous Rust `ProjectionLease` holds authenticated
mapping state stable for the run; this replaces retained repeated VMA pinning.
No allocation or lock acquisition occurs in the native bulk loop.

The four-entry `x86_run_views` table is a generated-code locality cache, not a
mapping-capability boundary. The request projection is already validated,
bounded to `HL_X86_PROJECTION_MAX_VIEWS`, incarnation-checked, and held by the
lease. Bulk lookup must therefore search the cache first and then the complete
projection. Resolver-acquired dynamic views remain reachable through the cache.
Missing or denied views still fail closed to the scalar path, which owns precise
resolver and fault exits.

MOVS/STOS widths 1/2/4/8, zero count, both direction values, overlap semantics,
view splitting, exact partial progress, budget charging, permission ordering,
and post-write dirty publication are implemented. LODS remains scalar. CMPS and
SCAS remain with their distinct compare/scan semantics and must not be folded
into this copy/fill mechanism.

## Performance attribution

The checksum benchmark's measured phase performs 90 one-MiB copies. With a
65,536-element run budget, each copy requires 16 native runs: 1,440 REP quanta.
The measured Rust proof reported 1,489 native runs, so budget boundaries explain
96.7% of run entries. Against the retained C result, the remaining gap is about
15.82 microseconds per quantum. This projection-cache correctness change does
not remove that scheduler/run-boundary cost; changing the architectural budget
or public-exit contract is a separate lane requiring explicit evidence.

