# x86 YMM state oracle audit

## Retained implementation studied

- `../engine/src/translator/guest/x86_64/cpu.h`, `struct cpu`: `v[32]`
  owns XMM0..15 and `vhi[32]` owns YMM0..15 bits 255:128. Both are
  per-CPU architectural storage, copied with the CPU on fork/checkpoint, and
  live until that CPU is torn down. No shared state or lock protects them.
- `../engine/src/translator/guest/x86_64/avx.c`, `avx_get`, `avx_put`, and the
  `0x77` VEX dispatch: reads and writes combine `v` with `vhi`; every VEX write
  zero-extends above its encoded width, so VEX.128 clears `vhi[destination]`.
  Legacy SSE paths write only `v` and preserve `vhi`.
- `../engine/src/translator/guest/x86_64/translate.c`, `avx_zero_upper` and the
  VEX lowering entry: inline translated VEX operations use the same split
  register ownership and upper-zero contract. Native faults spill live XMM
  lows; upper halves remain in their CPU-owned memory slots.
- `../engine/src/translator/guest/x86_64/interp.c`, `interp_xmm_get` and
  `interp_xmm_put`: legacy interpreter writes explicitly preserve bits 128 and
  above.
- `../engine/src/translator/guest/x86_64/signal.c`,
  `hl_x86_signal_build`, `hl_x86_signal_restore`, and
  `hl_x86_signal_capture`: signal frames save and restore `vhi`; synchronous
  host-fault capture reconstructs live XMM lows while retaining the CPU-owned
  upper halves.

These paths perform no blocking operation, cancellation, partial result, or
errno conversion. Architecture-specific behavior is x86 VEX width zeroing;
the retained implementation's host-specific AArch64 fault capture does not
replace upper halves because they are never hosted in live vector registers.

## Rust ownership mapping

| Capability | Rust owner | Status after this lane |
|---|---|---|
| XMM low halves | `hl_execution::CpuState::vectors` | implemented |
| YMM upper halves | `hl_execution::CpuState::vector_upper` | implemented |
| checkpoint/fork | `hl-execution` codec and `ExecutionMachine` clone | implemented |
| Linux signal save/restore | `hl-linux::X86SignalMachine` and engine signal-frame adapter | implemented |
| native C/Rust ABI | generated `hl_native_x86_64_cpu.vector_upper` / `X86_64Cpu::vector_upper` | implemented |
| native entry/exit transport | `NativeX86::capture` / `NativeX86::restore` | implemented |
| VEX.128 destination upper-zero | future VEX instruction owner | schema ready; opcodes deliberately outside this lane |

The ABI field is appended after all established native fields. Existing baked
emitter, trampoline, polling, dirty-publication, and fault offsets therefore do
not move. The generated C and Rust size/offset assertions cover the new tail.
