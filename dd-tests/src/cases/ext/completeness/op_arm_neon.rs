use super::*;

// ===================== OPCODE COMPLETENESS — aarch64 =====================

/// NEON/ASIMD base: int/fp arithmetic, reductions, table lookup, shifts, widen/narrow, permute,
/// compare/select, abs/neg, min/max, int<->fp convert.
pub(super) fn op_arm_neon() -> Group {
    group(
        "comp-arm-neon",
        vec![
            a("int", "completeness/arm_neon_int.c"),
            a("fp", "completeness/arm_neon_fp.c"),
            a("reduce", "completeness/arm_neon_reduce.c"),
            a("tbl", "completeness/arm_neon_tbl.c"),
            a("shift", "completeness/arm_neon_shift.c"),
            a("widen", "completeness/arm_neon_widen.c"),
            a("perm", "completeness/arm_neon_perm.c"),
            a("cmp", "completeness/arm_neon_cmp.c"),
            a("abs", "completeness/arm_neon_abs.c"),
            a("minmax", "completeness/arm_neon_minmax.c"),
            a("cvt", "completeness/arm_neon_cvt.c"),
            a("bitfield", "completeness/arm_bitfield.c"), // rbit/rev/clz (ACLE)
            a("ldpstp", "completeness/arm_ldpstp.c"),     // load-pair/store-pair forms
        ],
    )
}
