use super::*;

/// Crypto / checksum: AES-NI, PCLMULQDQ, SHA-NI, CRC32 (SSE4.2).
pub(super) fn op_x86_crypto() -> Group {
    group(
        "comp-x86-crypto",
        vec![
            x("aesni", "completeness/x86_aesni.c"), // jit86 UNIMPL 0F 38 DC (AESENC) abort
            x("pclmul", "completeness/x86_pclmul.c"), // jit86 UNIMPL 0F 3A 44 (PCLMULQDQ) abort
            x("sha", "completeness/x86_sha.c"), // full SHA-NI surface -> ARM SHA ext (incl. mem/alias/xmm0 shapes)
            x("sha-kat", "completeness/x86_sha_kat.c"), // FIPS-180 KATs (self-assert) + random-length msgs, SHA-1+SHA-256
            x("crc32", "completeness/x86_crc32.c"),     // jit86 UNIMPL 0F 38 F0 (CRC32 r/m8) abort
            // sse4x: the inline 0F38/0F3A GPR+lane glue (MOVBE/CRC32/PEXTR/PINSR/INSERTPS/AESKEYGENASSIST)
            // + the constant-hoist (v26 zero / v27 mask) and PMULL2 / PSHUFD fast-path regressions.
            x("sse4x", "completeness/x86_sse4x.c"),
        ],
    )
}
