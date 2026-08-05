# AMD64 native continuation oracle audit

This category owns the end-to-end Linux witness for an AMD64 translated
backedge that crosses the native executor's internal 256-iteration polling
quantum and then completes normally.  The fixture is built from local source
for the selected guest compiler and its output is compared with QEMU; it does
not consume a retained-tree binary, generated inventory, or prebuilt artifact.

## Retained C implementation studied

The read-only oracle audit covered:

- `../engine/src/core/dispatch.c`: `run_block`, `block_return`, and
  `run_guest` own the host/guest entry, fully spilled block return, cache
  generation selection, stop-the-world boundary, reason handling, signal
  delivery, and thread registration/teardown.
- `../engine/src/translator/guest/x86_64/emit.c`: `emit_prologue`,
  `emit_spill_gpr`, and `emit_spill` define the AMD64 guest-register and XMM
  lifetime across a translated block and its typed return.
- `../engine/src/translator/guest/x86_64/translate.c`: the block translator,
  conditional-backedge lowering, and `tier2_promote` keep hot-loop batching an
  internal translation/cache mechanism.
- `../engine/src/translator/guest/x86_64/dispatch.h`:
  `G_DISPATCH_REASON` consumes typed block-return reasons and resumes the
  dispatcher loop; an internal tier-two return is not exposed as a Linux
  process exit or syscall result.

`run_guest` owns one CPU for the guest thread, registers that CPU with the
stop-the-world and thread registries before executing translated code, and
unregisters it before releasing its alternate signal stack.  Cache identity is
generation-scoped; the dispatcher resolves the current executable alias while
holding the JIT lock in threaded mode and publishes the selected generation
before entry.  Translated state is spilled before the stop-the-world ownership
is released.  The continuation itself performs no allocation, host syscall,
blocking operation, errno conversion, or cancellation.  Pending interrupts and
signals are observed only at bounded dispatcher/translated boundaries.  The
retained AMD64 frontend has architecture-specific guest register/XMM spill
layout and reason handling; the shared dispatcher also has a separate AArch64
trampoline and tier-two path.  This category is AMD64-only because its assembly
is deliberately the AMD64 `addps`/`subl`/`jne` loop.

## Capability mapping

| Retained capability | Rust-engine owner | State |
|---|---|---|
| Entry budget and interrupt check | `src/native/exec/src/arch/x86_64/run.c` | implemented |
| Bounded backedge batching | `src/native/exec/src/arch/x86_64/run.c` | implemented in at most 256 iterations |
| Register and XMM state across the internal boundary | generated CPU schema and `src/native/exec/src/arch/x86_64` frontend/output | implemented |
| Continuation identity and invalidation | native cache mapping/instruction epochs, identity token, and source interval | implemented |
| Public return only after completion or caller-visible boundary | `hl_native_x86_64_run` | implemented; an internal quantum is resumed internally |
| Linux process exit and stdout comparison | `src/containers/hl-engine` composition and runtime inventory harness | implemented by this focused case |

The broader native unit evidence and fail-first account are recorded in
`src/native/exec/REP_QUANTA.md`.  This runtime case adds the guest-visible
composition witness: 300 loop iterations necessarily cross the 256-iteration
internal quantum, preserve XMM state, fall through, print `continue`, and exit
zero.
