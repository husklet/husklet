# x86 continuation audit

The retained implementation was inspected read-only in
`../engine/src/core/dispatch.c` (`run_block`, `block_return`, `run_guest`),
`../engine/src/translator/guest/x86_64/emit.c` (`emit_prologue`,
`emit_spill_gpr`, `emit_spill`), and
`../engine/src/translator/guest/x86_64/translate.c` (block entry interrupt and
budget checks, conditional backedges, and typed returns). The dispatcher owns
CPU and cache lifetime. A translated block retains guest GPR and XMM state in
host registers, observes interrupts at bounded entry points, and returns
completed work before the dispatcher services a boundary. These operations
allocate no memory, acquire no lock, invoke no host service, and have no errno
or cancellation behavior. Cache generation and source identity determine
whether a continuation remains executable; invalidation restores a typed
dispatcher edge before retirement.

The current owners are `src/arch/x86_64/run.c` (`emit_block` and
`hl_native_x86_64_run`), `src/arch/x86_64/frontend/output.c`
(`hl_x86_checkpoint`, `hl_x86_finish_chain`, `hl_x86_emit_exit`), and the
generic cache relocation layer. Their capability comparison is:

| Capability | Retained C | Native implementation |
|---|---|---|
| Entry budget and interrupt check | Before translated entry | Implemented |
| Exact instruction charging | At dispatcher return | Implemented |
| Backedge batching | Internal, not guest-visible | Implemented in 256-iteration quanta |
| Interrupt and executable-write visibility | Bounded translated boundary | Implemented at every quantum |
| Continuation identity | Live translation generation | Mapping epoch, instruction epoch, identity token, and source interval |
| Invalidation | Retire matching translation and links | Implemented by generic cache relocation |
| Public yield | Only when the caller's budget is exhausted | Divergent: leaked every internal 256-iteration quantum |

The divergence made an internal polling quantum observable as a public yield.
For a three-instruction backward loop with a 1,000-instruction budget, the
first quantum returned with 232 instructions still available, register state
at iteration 256, and `executed=768`. The run loop now continues internally
after a valid quantum. Its next ordinary loop iteration checks the remaining
budget and interrupt before re-entry, so the 256-iteration bound is unchanged;
only the premature public boundary is removed.

`test/x86_continue.c` covers register and projected-memory vector operations,
finite fallthrough, exact counter and completion accounting, exact budget
exhaustion at one quantum, interrupt-before-entry, and translation
invalidation. The fail-first state was `kind=YIELD`, `pc=0x8000`, `rax=44`,
`executed=768`, `budget=232` for the finite 300-iteration control. After the
change, warning-strict exact-source builds of `x86_continue`, `x86_translation`,
and `x86_budget` pass.
