# AArch64 translated instruction accounting

## Retained oracle

This lane inspected the read-only retained implementation in
`../engine/src/core/dispatch.c` (`run_block`, `block_return`, and `run_guest`),
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`,
`emit_spill`, and `emit_chain_exit_from`), and
`../engine/src/translator/guest/aarch64/translate.c` (`stitch_cond`,
`emit_irq_check`, and `translate_block`). The retained CPU record owns guest
register state for the dispatcher lifetime. Translated code borrows it during
`run_block`; a public exit spills before `block_return`. Direct chains retain
live registers, forward edges may skip the interrupt header, and every cycle
retains an interrupt poll. Mapping changes retire translations through the JIT
gate. The retained engine has no request instruction budget, so it has no
equivalent per-block completed-instruction store.

The Rust owner is `src/native/exec/src/executor.c::run_aarch64` and the emitted
admission sequence is `src/arch/aarch64/stub.c::{hl_a64_stub_budget_begin,
hl_a64_stub_budget_finish}`. Rust must additionally preserve exact bounded-run
semantics: a block compares and subtracts its complete instruction count before
entry, a rejected block executes nothing, interrupt ordering precedes budget
admission, and fallback correction restores both uncompleted budget and
instruction accounting.

## Mechanism

Before this change every admitted translated block updated both monotonic
counters independently: `budget -= count` and `executed += count`. The second
operation cost a load, add, and store at every direct-chain destination even
though `executed` is not consumed by generated code, the fault handler, or a
signal callback. For a request with immutable initial budget, the invariant is
`executed = request_budget - remaining_budget`. Fallback correction changes
remaining budget by exactly the uncompleted amount, so the same invariant
covers partial fallback and retry.

Generated code now owns only the admission-critical remaining budget. After
the fully spilled native return, while the same request and CPU are still
owned by `run_aarch64`, the dispatcher validates that remaining budget did not
grow beyond the request and derives `executed`. This removes three hot emitted
instructions without changing interrupt polling, budget rejection, register
publication, mapping identity, fault ordering, locks, or teardown. The CPU is
private to the executing request across this interval; no new shared state or
host call is introduced.

## Evidence

The warning-strict focused native executor suite passed 56/56 on exact candidate
tree. The complete `hl-engine --lib` run reached 478 passed, one pre-existing
options-registry count failure, and two ignored; the failing registry assertion
does not execute native code and also exists at the base revision.

Release artifacts were built alone from base and candidate and exercised on CPU
17 with the same static ARM64 guest (SHA-256
`521ecf12e07c68164b1c0c111eab008985a7e81ab39889301ebde182cac0f537`),
`--divisor 1000 --phase compute`, typed native execution plus diagnostics, and
11 repeats. Checksums and all execution counters matched, including 337,290
completed instructions. Base median/min/max was 1,824/1,681/2,277 microseconds;
candidate was 1,724/1,654/2,585 microseconds, a 5.48% median reduction. The
candidate and base engine hashes were respectively
`0450130cba4c1180dc59e93cbf2cacbd0df9dc54ee04b6dd0f852cd04d0567ba`
and `76b8e1c5e632db08767d1503a8257314b4e440606efa14a65b769203dc349739`.
Host load was elevated and the ranges overlap, so this is focused causal
evidence rather than a release performance claim; the exact removal of three
instructions per admitted block is independently source-verifiable.
