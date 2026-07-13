use super::*;

/// aarch64 extensions: crypto (AES/SHA1/SHA256), CRC32, LSE atomics, FP16, dot-product, i8mm, bf16.
pub(super) fn op_arm_ext() -> Group {
    group(
        "comp-arm-ext",
        vec![
            a("crypto-aes", "completeness/arm_crypto_aes.c"),
            a("crypto-sha1", "completeness/arm_crypto_sha1.c"),
            a("crypto-sha256", "completeness/arm_crypto_sha256.c"),
            a("crc32", "completeness/arm_crc32.c"),
            a("lse", "completeness/arm_lse.c"), // LDADD/SWP/CAS/LDSET/LDCLR
            a("fp16", "completeness/arm_fp16.c"),
            a("dotprod", "completeness/arm_dotprod.c"),
            a("i8mm", "completeness/arm_i8mm.c"), // SMMLA matrix-multiply
            a("bf16", "completeness/arm_bf16.c"), // BFDOT
        ],
    )
}
