use super::*;

// ===================== OPCODE COMPLETENESS — x86-64 =====================

/// SSE/SSE2/SSE3/SSSE3/SSE4.1/SSE4.2 packed-int / packed-fp / shuffle / string ops.
pub(super) fn op_x86_sse() -> Group {
    group(
        "comp-x86-sse",
        vec![
            x("sse2", "completeness/x86_sse2.c"),
            x("sse3", "completeness/x86_sse3.c"),
            x("ssse3", "completeness/x86_ssse3.c"), // jit86 UNIMPL 0F 38 1C (PABSB) abort
            x("shuffle", "completeness/x86_shuffle.c"), // pshufb/palignr/pblend*/pmovsx/zx inline NEON (byte-exact)
            x("sse41", "completeness/x86_sse41.c"),     // jit86 UNIMPL 0F 3A 40 (DPPS) abort
            x("sse42", "completeness/x86_sse42.c"),     // jit86 UNIMPL 0F 38 F1 (CRC32 r/m) abort
        ],
    )
}
