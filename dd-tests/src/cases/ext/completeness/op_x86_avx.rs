use super::*;

/// AVX / AVX2 / FMA / F16C 256-bit vector lane.
pub(super) fn op_x86_avx() -> Group {
    group(
        "comp-x86-avx",
        vec![
            // VEX-encoded AVX/AVX2 256-bit ops, FMA (VFMADD/VFMSUB…) and F16C (VCVTPH2PS/VCVTPS2PH) are
            // lowered in do_avx() (byte-exact vs qemu). These formerly aborted on the first 256-bit op.
            x("avx", "completeness/x86_avx.c"),
            x("avx2", "completeness/x86_avx2.c"),
            x("fma", "completeness/x86_fma.c"),
            x("f16c", "completeness/x86_f16c.c"),
            // VEX vmovss/vmovsd reg-reg scalar merge: upper low-lane bits come from vvvv.
            x("vmov-scalar-merge", "completeness/x86_vmov_scalar_merge.c"),
        ],
    )
}
