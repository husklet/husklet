# ISA oracle audit

This category owns guest-instruction translation and ELF-address-model compatibility. Architecture is a
case target, not a directory owner. The nine stable legacy case identities are preserved in one manifest;
the source and expected stdout bytes are unchanged apart from semantic filenames.

## Retained C implementation studied

The read-only retained engine was audited at these implementation entry points:

- `../engine/src/translator/guest/aarch64/translate.c`: `try_lse_atomic`,
  `emit_lse_status_zero`, `emit_casp`, the exclusive load/store lowering, `emit_fold_mem`, and
  `gpc_unbias`. The translator recognizes complete exclusive retry loops before replacing them with one
  LSE operation, writes the architecturally successful zero status, preserves acquire/release ordering,
  relocates overlapping CASP register pairs, and folds only low ET_EXEC image addresses through the
  non-PIE bias.
- `../engine/src/translator/guest/aarch64/cpu.h`: `guestbase_on` and CPU ownership. Each guest thread owns
  its `struct cpu`; non-PIE image bounds and bias belong to the loaded engine image. Stolen registers are
  spilled in CPU storage and architecture-specific exclusions protect SP, exclusive-monitor, pair, and
  vector forms.
- `../engine/src/translator/guest/x86_64/address.c`: `hl_x86_address_emit`,
  `hl_x86_address_fold`, and `emit_bias`. Effective addresses retain guest identity; only an address in the
  active ET_EXEC image interval is translated to host storage. PIE leaves the mechanism inert.
- `../engine/src/translator/guest/x86_64/avx.c`: `do_avx`, `do_sse3b`, scalar/packed floating-point and
  horizontal SSE3 paths, including NaN selection and upper-lane preservation.
- `../engine/src/translator/guest/x86_64/cmpxchg.c`: `hl_x86_cmpxchg16`. The 16-byte operation is guarded
  by a bounded hashed atomic-lock table; memory comparison/update is indivisible and flags and observed
  accumulator values are published before unlock.
- `../engine/src/translator/guest/x86_64/translate.c`: the `0F C0/C1` XADD dispatch and its `emit_ea`,
  `emit_memory_guard`, `e_lse(LSE_LDADD)`, `do_alu`, `byte_val`, and `byte_wb` call graph; and
  `../engine/src/translator/guest/x86_64/interp.c`: the `0F C0/C1` interpreter dispatch,
  `interp_locked_rmw`, `interp_rm_read`, `interp_reg_write`, `interp_rm_write`, and `interp_alu_add`.
  The register pre-image is captured before either destination changes, the source register is written
  before the sum so aliases retain the sum, and memory admission precedes the indivisible update.
- `../engine/src/translator/guest/x86_64/x87state.c`, `x87math.c`, and `lower/x87_stack.c`: x87 TOP,
  emptiness tags, exceptions, indefinite values, save/restore, and materialization ownership.
- `../engine/src/translator/guest/x86_64/legacy.c`, `rep_runtime.c`, and `translit/translit.c`: legacy
  syscall pointer rebasing, repeated-memory operation rebasing, and the explicit rejection of direct
  transliteration for biased non-PIE images.
- `../engine/src/translator/cache.c`: per-thread CPU lifetime through TLS, serialized translation through
  `g_jit_lock`, cache-metadata locking after guest threading begins, immutable published code generations,
  and bounded translation/provenance tables.

Instruction execution has no Linux partial-result or errno policy of its own. Faulting memory instructions
must publish the precise guest PC/address and leave syscall conversion to the Linux personality. Atomic
operations preserve the requested memory order; exclusive failure reports status without a partial store.
Thread CPU state is destroyed with the guest thread, while translation indexes live for an engine
generation and are invalidated on code mutation or wholesale cache reset.

Host and architecture branches are explicit: the AArch64 guest path uses native AArch64 LSE/CASP when
safe and a monitor-preserving fallback otherwise; x86 guest SIMD/x87 is lowered or interpreted on an
AArch64 host; PIE/static-PIE bypass non-PIE rebasing; macOS protection/fault mechanics remain below the
same guest-address contract.

## Rust ownership comparison

| Retained capability | Rust owner | State |
|---|---|---|
| Exclusive reservation generations, invalidation, ordering | `hl-memory/src/atomic_access.rs`, `reservation.rs` | implemented |
| Per-task AArch64 reservation lifetime and fork/reset clearing | `hl-execution/src/aarch64/state.rs`, `hl-engine/src/ffi/linux/execution/fork.rs` | implemented |
| x86 compare-exchange decode/lowering | `hl-execution/src/x86`, `native/exec/src/arch/x86_64/frontend/memory.c` | implemented |
| x86 XADD widths, byte-register identity, ADD flags, dual destinations | `native/exec/src/arch/x86_64/frontend/memory.c` | implemented |
| x86 XADD memory R/W admission, aligned LDADDAL, split-lock fallback, dirty/executable publication | `native/exec/src/arch/x86_64/frontend/memory.c` | implemented |
| x87 control/status/value/class state | `hl-execution/src/x86`, Linux signal-frame adapter | implemented |
| Native AArch64 provenance for exclusive/LSE/CASP | `native/exec/test/aarch64_provenance.c` | divergent: fallback-only until monitor state is ported |
| Exact ET_EXEC guest-address identity with internal host placement | loader/memory/native execution composition | acceptance coverage retained by the non-PIE cases |
| Per-case deterministic Go cross-build environment | testing folder runner | missing; two cases are visibly `unsupported` |

## Oracle and divergence evidence

The C cases build from source for their declared guest ISA and compare exact stdout and exit status under
QEMU. The AArch64 regression was originally derived from native ARM64 differential fuzzing; QEMU provides
the portable folder-runner reference. The x86 regression uses `qemu-x86_64`. The historical Go GC case
cannot use QEMU as a reliable high-address oracle because Go pointer packing depends on placement, but its
deterministic golden remains migration evidence. Neither Go case is silently dropped: both enumerate as
typed unsupported until the runner owns cross-build environment configuration.

The retired checked-in executables were deleted under the repository source-only artifact policy. Their
last recorded SHA-256 provenance is retained here without retaining compiled bytes: `hello`
`418696b900a842033e475278b170b9697a95782aa6a13ba4445079a2cd6bbc6e`, `ctest`
`5574f534dd9c894b11505683ab544d4f20e2e89c0faffe541956c60985e0fb19`, `hx`
`be4776713d332ecaf0230aba81289660d3922d3e981699abbded6645826eaae6`, `glibc`
`5ecbf7981b6a41ae46f402583e0c0560ff5f48b32666a0c77afee17ed1ee46b7`, `glibc-min`
`f4cea2e8fc7fcf727697e141e408384e2569133130fe819194bbd5224bce4e58`, `go-static-goro`
`fa3d0d34e8b84b98f8705ad1688b7d9508e28566a86442ef91ff0aec48d2327e`, and `go-static-heapgc`
`8d5fdf390c0043f9beabc3ab56051419b30362d94b00b3f797766d938e93853b`.
