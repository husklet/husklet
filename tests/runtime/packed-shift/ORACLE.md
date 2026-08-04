# Legacy XMM scalar-count packed-shift oracle

## Retained implementation audit

The read-only audit covered
`../engine/src/translator/guest/x86_64/translate.c` (`e_sse_var_shift` and the
legacy SSE dispatch for `0F D1-D3`, `0F E1-E2`, and `0F F1-F3`) and
`../engine/src/translator/guest/x86_64/interp.c` (`interp_pshift` and the
matching `interp_step_sse` dispatch). The translator consumes one decoded
instruction and emits operations against CPU-owned XMM state; it introduces no
persistent identity, lock, or teardown state. Register and memory operand
identity lasts for the instruction. Translated-block ownership, publication,
locking, invalidation, and teardown remain executor responsibilities.

The legacy forms shift every word, dword, or qword lane in the destination by
the unsigned low 64 bits of an XMM or m128 source. Logical shifts at or beyond
the lane width produce zero. Arithmetic word and dword right shifts at or
beyond the width produce sign fill. The m128 source is fully read before the
destination is updated, so a source fault leaves the destination unchanged.
No form modifies flags or has partial-result, blocking, cancellation, or errno
semantics. Guest faults follow the ordinary translated memory-fault path. The
retained translator lowering is AArch64-specific; `interp_pshift` is the
host-independent fallback. There are no host-specific branches beyond that
translator/fallback selection.

## Capability mapping

| Retained capability | Husklet owner | Coverage in this row |
|---|---|---|
| PSRLW/PSRLD/PSRLQ with XMM count | native x86 frontend and packed-shift lowering | every width; zero, boundary, and oversized counts |
| PSRAW/PSRAD with XMM count | same | every width; sign-fill boundaries |
| PSLLW/PSLLD/PSLLQ with XMM count | same | every width; zero, boundary, and oversized counts |
| Low-64 count from m128 | vector operand read guard plus packed-shift lowering | every opcode and count cohort |
| Fault before destination commit | vector operand read guard | executable amd64 `PROT_NONE` source check |
| Flag preservation | packed-shift lowering | established by native unit coverage; these forms emit no flag-producing operation |
| MMX forms | not owned by the current native XMM frontend | remaining outside this row |
| VEX/AVX forms | AVX fallback/domain | covered separately by `runtime/abi-corpus-vex_sse2int` |

`scalar_count.c` emits actual legacy SSE2 XMM-register and m128-source
instructions for amd64 guests. The arm64 build executes the same bounded scalar
model so both guest targets have one byte-identical golden. The source validates
each hardware result against that model before contributing it to the digest.
