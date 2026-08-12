# AArch64 fallback boundary audit

> Historical Rust-executor audit. The paths and ownership in “Rust ownership
> and ordering” were deleted with the Rust guest executor. Production now runs
> the retained C closure through `hl-engine/src/execution/`; the evidence below
> applies only to the unselected `exec/` replacement candidate.

## Retained oracle

The retained engine was inspected read-only at:

- `../engine/src/translator/guest/aarch64/translate.c`: `emit_mangled_x18`,
  load/store folding, and final instruction dispatch;
- `../engine/src/translator/guest/aarch64/interp.c`: `interp_exec_load_store`,
  `interp_step`, `translate_block`, and `run_block`;
- `../engine/src/translator/guest/aarch64/abi.h`,
  `../engine/src/translator/guest/aarch64/stubs.c`, and
  `../engine/src/core/target/aarch64.c`: CPU ownership and host-entry ABI.

On the AArch64 diagonal the retained translator keeps guest registers in host
registers inside a block and restores the authoritative `struct cpu` image on
every dispatcher exit. The non-AArch64 interpreter instead mutates that same
CPU image one instruction at a time. A store reads all source registers before
the memory access, commits no register or PC change on an access fault, and
advances PC only after the write. NZCV is unchanged by loads and stores.

## Historical Rust ownership and ordering

- `src/runtime/native/exec/src/arch/aarch64/guard.c` owns projection checks, write
  reservation, dirty-range publication, and cold exits.
- `src/runtime/native/exec/src/arch/aarch64/stub.c` spills the live host register image
  into `hl_native_aarch64_cpu` on exit.
- `src/runtime/native/exec/src/executor.c` returns the fully spilled native boundary.
- `src/containers/hl-engine/src/native/executor.rs` converts between the C CPU
  frame and `Aarch64CpuState`.
- `src/containers/hl-engine/src/ffi/linux/execution/scheduler.rs` services one
  fallback instruction with `run_step` and suppresses the failed native entry.
- `src/runtime/hl-execution/src/aarch64/memory/{decode,interpreter}.rs` decodes
  register-offset addressing, stages memory effects, preserves NZCV, and
  advances PC after a successful commit.

State is thread-owned from scheduler admission through native return and the
single fallback step. Projection identity is generation-qualified and remains
owned by the scheduler lease. The dirty journal is bounded to 16 records; an
overflow exits before executing the guest store so the coordinator can publish
the accumulated ranges and retry from the same guest PC.

## Capability matrix

| Boundary capability | Status | Evidence |
|---|---|---|
| Register-offset store decode and effective address | implemented | retained and Rust decoders agree for UXTW/LSL/SXTW/SXTX |
| Store source staging and PC commit ordering | implemented | both interpreters read source first and advance PC only after commit |
| Native fallback PC, budget, and executed count | implemented | VLA localized fallback at `0x400654`/`0x400684` without premature retirement |
| Ordinary guard exit restores x9 and NZCV | implemented | existing guard paths and focused tests |
| Dirty-journal overflow restores x9 and NZCV | fixed | fail-first test observed x9 corruption; focused regression crosses count 16 |
| Stolen-register source/destination aliases | implemented | 25-case x16/x17/x18/x28/x30 field matrix |
| VLA native execution | fixed | typed native run prints diagnostics and passes |
| Deep recursion native execution | fixed | typed native run prints diagnostics and passes |

## Defect and repair

`hl_a64_guard_write_begin` used x9 and NZCV as guard scratch. Its ordinary paths
restored both, but the `dirty_count >= 16` branch jumped directly to
`hl_a64_stub_exit`. The common spill therefore published guard scratch as guest
x9 and guard comparison flags as guest NZCV. In `runtime/abi-vla`, x9 became
`dirty_last` (`sp + 8`); `sxtw x5,w9` then converted that corrupted value into
the read-loop bound and the loop reached the top of the stack mapping.

The repair restores saved NZCV and guest x9 before the pre-store epoch exit.
It adds three emitted instructions only on the cold journal-overflow exit; the
successful memory hot path and emitted guest operation are unchanged.
