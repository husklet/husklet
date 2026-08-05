# X86 256-bit vector-memory oracle audit

## Retained implementation studied

The read-only oracle was `../engine/src/translator/guest/x86_64/avx.c`, entry
points `avx_get`, `avx_put`, `avx_ea`, `avx_try_read`, `avx_try_write`,
`avx_memory_read`, and `avx_memory_write`, plus
`../engine/src/translator/guest/x86_64/translate.c`, entry points
`avx_cpu_ldr_q`, `avx_cpu_str_q`, `avx_zero_upper`, and the VEX memory lowering
inside `emit_avx_inline`.

The oracle owns low 128-bit state in `cpu.v`, YMM bits 128..255 in `cpu.vhi`,
and higher ZMM state in `cpu.vz`. Register state is CPU-instance-local and
survives until task teardown; these helpers neither allocate nor lock. A VEX
write commits its requested width and zeroes state above that width. The
dispatcher owns retry and fault delivery: a rejected whole-span access records
guest address, width, access type, and instruction PC, then abandons before any
architectural register or guest byte is changed. Gather's partial-result rule is
separate and does not apply to ordinary vector loads/stores.

The inline oracle validates `address + width` for overflow, active-view bounds,
and permissions before translating the guest address. The 256-bit path transfers
two 128-bit lanes. Loads stage both lanes before publishing the destination;
stores validate the full 32-byte interval before either lane is written. Both
unaligned accesses and spans crossing a page/view boundary use the same whole
span rule. AArch64 host lowering uses Q-register scratch state; no host-specific
branch changes the x86 architectural result.

## Rust-native ownership map

| Retained capability | Rust-native owner | Status |
|---|---|---|
| Low 128-bit live register | host `v0..v15`, spilled to `cpu.vectors` | implemented |
| YMM upper 128-bit state | `hl_native_x86_64_cpu.vector_upper` | implemented |
| Upper load/store/zero | `hl_x86_emit_vector_upper_{load,store,zero}` | implemented |
| Whole-span cached read admission | `hl_x86_emit_read_cache` | implemented for 16/32 |
| Whole-span active-view guard | `hl_x86_emit_vector` | implemented for 16/32 |
| Two-lane load staging | `hl_x86_emit_vector` Q16/Q17 scratch | implemented |
| Fault-before-load commit | post-guard copy plus upper store | implemented |
| Fault-before-store commit | full-width guard before both Q stores | implemented |
| Exact dirty interval | `hl_x86_emit_dirty` with width-derived end | implemented |
| VEX opcode admission and operation cases | VEX frontend | intentionally remaining |
