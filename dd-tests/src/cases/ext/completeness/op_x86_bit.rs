use super::*;

/// Bit-manipulation extensions: BMI1/BMI2, POPCNT, LZCNT, ADX, plus bit-test / double-shift inline asm.
pub(super) fn op_x86_bit() -> Group {
    group(
        "comp-x86-bit",
        vec![
            x("bmi1", "completeness/x86_bmi1.c"),
            x("bmi2", "completeness/x86_bmi2.c"),
            x("popcnt", "completeness/x86_popcnt.c"),
            x("lzcnt", "completeness/x86_lzcnt.c"),
            x("adx", "completeness/x86_adx.c"),
            x("bittest", "completeness/x86_bittest.c"), // bt/bts/btr/btc
            x("shld", "completeness/x86_shld.c"),       // shld/shrd
        ],
    )
}
