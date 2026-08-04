# Packed shift oracle audit

## Retained implementation studied

The read-only implementation audit covered
`../engine/src/translator/guest/x86_64/translate.c` (`e_sse_var_shift` and the
legacy SSE dispatch arms for `0F 71/72/73`, `0F D1-D3`, `0F E1-E2`, and
`0F F1-F3`) and `../engine/src/translator/guest/x86_64/interp.c`
(`interp_pshift`, `interp_pshift_bytes`, and the matching `interp_step_sse`
arms). The translator owns no persistent object: it reads one decoded
instruction and emits a block against the CPU-owned XMM state. Register and
memory operand identity lasts for that instruction; normal block/cache lifetime,
locking, publication, and teardown remain owned by the native executor.

The immediate groups destructively update the ModRM r/m XMM register. Logical
word/dword/qword shifts use `/2` right and `/6` left; arithmetic word/dword
right shifts use `/4`. `66 0F 73 /3` and `/7` shift the complete XMM value by
bytes. Counts at or beyond the lane/128-bit width produce zero, except arithmetic
right shifts produce sign fill. Scalar-count forms read the complete low 64 bits
of the register or m128 count operand before updating the destination. A memory
fault therefore occurs before destination commit. Packed shifts do not modify
x86 flags and have no partial result, blocking, cancellation, signal, or errno
contract. The retained lowering is AArch64-specific NEON; its interpreter is
the host-independent exact fallback. There is no MMX lane in Husklet's current
native XMM frontend, and VEX/AVX packed shifts are a separate domain.

## Capability mapping

| Retained capability | Husklet owner | State |
|---|---|---|
| Immediate PSRLW/D/Q, PSRAW/D, PSLLW/D/Q | `frontend.c` decode and `frontend/memory.c` NEON lowering | implemented |
| Immediate PSRLDQ/PSLLDQ | same native frontend | implemented |
| Scalar low-64 count from XMM | same native frontend | implemented |
| Scalar low-64 count from m128 | generic vector address/read guard plus packed-shift lowering | implemented |
| Count saturation and arithmetic sign fill | packed-shift lowering | implemented |
| Destination and flags preserved on memory fault | generic vector guard, operation emitted only after load | implemented |
| MMX forms | native frontend | remaining gap; outside current XMM-only boundary |
| VEX/AVX forms | native frontend | remaining separate AVX domain |

`test/packed_shift.c` covers every legacy XMM opcode, legal immediate subopcode,
register and memory scalar-count decode, invalid/truncated encodings, immediate
saturation, byte shifts, arithmetic sign fill, and flag preservation. Executed
scalar-count memory-fault differential evidence remains a focused gap for the
later exact-tree compatibility gate; the implementation uses the already tested
generic vector read guard rather than a shift-specific memory path.
