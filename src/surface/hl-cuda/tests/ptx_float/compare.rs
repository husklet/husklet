//! `setp.<cmp>.f32` — the ordered and unordered PTX comparison families, and the `num`/`nan` tests the IR
//! cannot express.

use super::*;

/// One thread per element: `out[i] = (a[i] <cmp> b[i]) ? 1 : 0` with the comparison done in f32. The
/// predicate is consumed by `@p bra`, the one predicated form the IR models.
fn source(cmp: &str) -> String {
    format!(
        r#"
    .visible .entry cmp(
        .param .u64 cmp_param_0,
        .param .u64 cmp_param_1,
        .param .u64 cmp_param_2,
        .param .u32 cmp_param_3
    )
    {{
        .reg .pred %p<3>;
        .reg .f32  %f<3>;
        .reg .b32  %r<8>;
        .reg .b64  %rd<11>;

        ld.param.u64 %rd1, [cmp_param_0];
        ld.param.u64 %rd2, [cmp_param_1];
        ld.param.u64 %rd3, [cmp_param_2];
        ld.param.u32 %r2,  [cmp_param_3];
        mov.u32      %r3, %ntid.x;
        mov.u32      %r4, %ctaid.x;
        mov.u32      %r5, %tid.x;
        mad.lo.s32   %r1, %r4, %r3, %r5;
        setp.ge.s32  %p1, %r1, %r2;
        @%p1 bra     DONE;

        cvta.to.global.u64 %rd4, %rd1;
        cvta.to.global.u64 %rd5, %rd2;
        cvta.to.global.u64 %rd6, %rd3;
        mul.wide.s32 %rd7, %r1, 4;
        add.s64      %rd8,  %rd4, %rd7;
        add.s64      %rd9,  %rd5, %rd7;
        add.s64      %rd10, %rd6, %rd7;
        ld.global.f32 %f1, [%rd8];
        ld.global.f32 %f2, [%rd9];
        setp.{cmp}.f32 %p2, %f1, %f2;
        mov.u32      %r6, 0;
        @%p2 bra     TRUE;
        bra          STORE;
    TRUE:
        mov.u32      %r6, 1;
    STORE:
        st.global.u32 [%rd10], %r6;
    DONE:
        ret;
    }}
"#
    )
}

/// Operand pairs where a wrong lowering is a different answer. The negatives are the decisive ones: an
/// integer compare of the bit patterns reports `-3.0 < -1.0` as FALSE, because IEEE-754 magnitude ordering
/// runs backwards once the sign bit is set. The NaN rows separate the ordered family from the unordered one.
const PAIRS: [(f32, f32); 12] = [
    (-3.0, -1.0),
    (-1.0, -3.0),
    (-2.0, -2.0),
    (-5.5, 3.25),
    (3.25, -5.5),
    (1.0, 2.0),
    (2.0, 1.0),
    (0.0, -0.0),
    (f32::NAN, 1.0),
    (1.0, f32::NAN),
    (f32::NAN, f32::NAN),
    (f32::NEG_INFINITY, -3.0e38),
];

/// The comparison itself, ignoring NaN: PTX `eq`/`ne`/`lt`/`le`/`gt`/`ge` on two non-NaN operands.
fn holds(cmp: &str, a: f32, b: f32) -> bool {
    match cmp {
        "eq" => a == b,
        "ne" => a != b,
        "lt" => a < b,
        "le" => a <= b,
        "gt" => a > b,
        _ => a >= b,
    }
}

fn inputs() -> (Vec<u32>, Vec<u32>) {
    (
        PAIRS.iter().map(|(a, _)| a.to_bits()).collect(),
        PAIRS.iter().map(|(_, b)| b.to_bits()).collect(),
    )
}

#[test]
fn the_ordered_family_is_false_at_nan_and_orders_negative_operands() {
    let (a, b) = inputs();
    for cmp in ["eq", "ne", "lt", "le", "gt", "ge"] {
        let got = run(&source(cmp), "cmp", &a, &b);
        for (i, &(x, y)) in PAIRS.iter().enumerate() {
            // PTX: the ordered family is FALSE if either operand is NaN — including `ne`.
            let want = u32::from(!x.is_nan() && !y.is_nan() && holds(cmp, x, y));
            assert_eq!(got[i], want, "setp.{cmp}.f32({x}, {y})");
        }
    }
}

#[test]
fn the_unordered_family_is_true_at_nan() {
    let (a, b) = inputs();
    for (cmp, base) in [
        ("equ", "eq"),
        ("neu", "ne"),
        ("ltu", "lt"),
        ("leu", "le"),
        ("gtu", "gt"),
        ("geu", "ge"),
    ] {
        let got = run(&source(cmp), "cmp", &a, &b);
        for (i, &(x, y)) in PAIRS.iter().enumerate() {
            // PTX: the unordered family is TRUE if either operand is NaN, otherwise the comparison. This is
            // what a compiler emits for a negated source comparison such as `!(x < y)`.
            let want = u32::from(x.is_nan() || y.is_nan() || holds(base, x, y));
            assert_eq!(got[i], want, "setp.{cmp}.f32({x}, {y})");
        }
    }
}

#[test]
fn nan_tests_and_fused_predicates_are_rejected_and_a_plain_compare_still_compiles() {
    // `setp.num`/`setp.nan` test whether the operands ARE numbers; neither family reproduces them and
    // `Inst::FSetp` carries a comparison only.
    for body in [
        "setp.num.f32 %p1, %f1, %f2; ret;",
        "setp.nan.f32 %p1, %f1, %f2; ret;",
    ] {
        assert_rejected(body);
    }
    // The fused-predicate form carries a fourth operand that `Inst::FSetp` has no room for.
    for body in [
        "setp.lt.and.f32 %p1|%p2, %f1, %f2, %p3; ret;",
        "setp.lt.or.f32 %p1, %f1, %f2, %p3; ret;",
        "setp.eq.and.s32 %p1, %r1, %r2, %p3; ret;",
    ] {
        assert_rejected(body);
    }
    // The unordered family is floating-point-only.
    assert_rejected("setp.ltu.s32 %p1, %r1, %r2; ret;");

    // Valid neighbours: both families on f32, and the integer compare.
    for body in [
        "setp.lt.f32 %p1, %f1, %f2; ret;",
        "setp.geu.f32 %p1, %f1, %f2; ret;",
        "setp.lt.s32 %p1, %r1, %r2; ret;",
        "setp.lt.u32 %p1, %r1, %r2; ret;",
    ] {
        compile(body).unwrap_or_else(|e| panic!("`{body}` must compile, got {e:?}"));
    }
}
