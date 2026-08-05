# AArch64 conditional trace accounting

## Oracle and ownership

This lane inspected the retained implementation in
`../engine/src/core/dispatch.c` (`run_block`, `block_return`, and `run_guest`),
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`,
`emit_spill`, and `emit_chain_exit_from`), and
`../engine/src/translator/guest/aarch64/translate.c` (`stitch_cond`,
`emit_irq_check`, and `translate_block`). Retained conditional fall-through is
bounded and inline. Taken edges leave through a chain exit. Every in-cache
cycle retains an interrupt poll; forward edges may omit redundant polls. State
is live in host registers until a public exit spills it, and mapping/JIT
generation changes retire every dependent edge.

Rust additionally owns a request instruction budget. `trace.c` and `stub.c`
reserve translated work before execution, `executor.c::run_aarch64` owns the
request-local remaining budget and derives completed work after a fully spilled
return, and cache relocation owns target generation and cycle admission.

## Charged-prefix invariant

A stitched trace reserves its complete fall-through interval once. Each taken
edge refunds the suffix after that conditional before entering its relocation
or spill. Therefore:

```text
request budget - remaining budget = guest instructions completed before exit
```

The unsupported instruction at a fallback boundary belongs to the translation
source interval for provenance and invalidation, but is not completed work.
`instruction_count` records only the supported prefix; `source_last` continues
to cover the fallback instruction. This distinction was previously hidden
because a conditional ended the trace before the later unsupported word.

The entry interrupt and token checks remain before the single reservation.
Stitching is bounded to three conditions and 32 guest words. A backward or
relocated cycle re-enters the destination admission and polls again. An inline
fall-through performs no extra poll, matching the retained bounded-region
contract. Exclusive load/store intervals are never crossed.

## Diagnostic deltas

`completed` is explicitly native-only accounting: it is incremented after
`hl_native_aarch64_enter`, while interpreter work is not included. A stitched
trace can execute a supported prefix natively before reaching an unsupported
instruction which, without stitching, was encountered at index zero of a new
block and handled through fallback. Thus native completed work, builds, cache
hits, public branches, and fallback counts may change even with identical guest
architectural work.

On the fixed compute workload the unstiched tree reported 337,292 native
instructions, eight branch boundaries, and two fallback boundaries. The
candidate reported 349,590 native instructions, four branch boundaries, and
three fallback boundaries. Direct charged-prefix tests prove that the 12,298
additional native instructions are newly covered prefixes, not missing taken
refunds. Checksums and final per-request budget remain the acceptance signals;
these structural diagnostics are expected attribution evidence.

## Required evidence

The direct trace cases cover taken and fall-through paths, a nested call and
conditional path, constrained-budget yield, relocation, and a 100-iteration
backedge with exact `executed == 301` and zero remaining budget. Existing
asynchronous-token, forward-chain, cycle, invalidation, and fallback tests must
remain warning-strict and green. Performance acceptance additionally requires
paired same-CPU base/candidate runs with identical checksum and native-verified
mode.
