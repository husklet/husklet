//! `cvt` — `f32 <-> u32`, and the `.rni` (nearest, ties to even) versus `.rzi` (truncate) rounding modes.

use super::*;

/// One thread per element: load `a[i]`, apply one `cvt`, store the 32-bit result. `b` is loaded but unused,
/// so the same harness drives both directions; `store` is the destination's `st` type.
fn source(load: &str, cvt: &str, store: &str) -> String {
    format!(
        r#"
    .visible .entry conv(
        .param .u64 conv_param_0,
        .param .u64 conv_param_1,
        .param .u64 conv_param_2,
        .param .u32 conv_param_3
    )
    {{
        .reg .pred %p<2>;
        .reg .f32  %f<3>;
        .reg .b32  %r<9>;
        .reg .b64  %rd<11>;

        ld.param.u64 %rd1, [conv_param_0];
        ld.param.u64 %rd2, [conv_param_1];
        ld.param.u64 %rd3, [conv_param_2];
        ld.param.u32 %r2,  [conv_param_3];
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
        {load}
        {cvt}
        {store}
    DONE:
        ret;
    }}
"#
    )
}

fn convert(load: &str, cvt: &str, store: &str, input: &[u32]) -> Vec<u32> {
    let zero = vec![0u32; input.len()];
    run(&source(load, cvt, store), "conv", input, &zero)
}

#[test]
fn an_unsigned_source_above_i32_max_converts_to_its_unsigned_value() {
    // `0x80000000` is 2147483648 unsigned and -2147483648 signed: reusing `CVT_F32_FROM_S32` for
    // `cvt.rn.f32.u32` (or reinterpreting the bits, the old fallback) gives a different NUMBER.
    let input = [0x8000_0000, 0xFFFF_FFFF, 0x7FFF_FFFF, 0, 1, 3_000_000_000];
    let got = convert(
        "ld.global.u32 %r6, [%rd8];",
        "cvt.rn.f32.u32 %f1, %r6;",
        "st.global.f32 [%rd10], %f1;",
        &input,
    );
    for (i, &raw) in input.iter().enumerate() {
        assert_eq!(
            f32::from_bits(got[i]).to_bits(),
            (raw as f32).to_bits(),
            "cvt.rn.f32.u32({raw})"
        );
    }

    // The signed neighbour must keep reading the same bits as SIGNED.
    let got = convert(
        "ld.global.u32 %r6, [%rd8];",
        "cvt.rn.f32.s32 %f1, %r6;",
        "st.global.f32 [%rd10], %f1;",
        &input,
    );
    for (i, &raw) in input.iter().enumerate() {
        assert_eq!(
            f32::from_bits(got[i]).to_bits(),
            (raw as i32 as f32).to_bits(),
            "cvt.rn.f32.s32({raw})"
        );
    }
}

/// `2.5` and `3.5` are the decisive values: ties-to-even gives 2 and 4, truncation gives 2 and 3, and
/// round-half-up gives 3 and 4 — so no single wrong rounding reproduces both.
const TIES: [f32; 8] = [2.5, 3.5, -2.5, -3.5, 0.5, 1.5, 2.499_999_8, 4.5];

#[test]
fn rni_rounds_to_nearest_even_where_rzi_truncates() {
    let input: Vec<u32> = TIES.iter().map(|x| x.to_bits()).collect();

    let nearest = convert(
        "ld.global.f32 %f1, [%rd8];",
        "cvt.rni.s32.f32 %r7, %f1;",
        "st.global.u32 [%rd10], %r7;",
        &input,
    );
    let truncated = convert(
        "ld.global.f32 %f1, [%rd8];",
        "cvt.rzi.s32.f32 %r7, %f1;",
        "st.global.u32 [%rd10], %r7;",
        &input,
    );
    for (i, &x) in TIES.iter().enumerate() {
        assert_eq!(
            nearest[i] as i32,
            x.round_ties_even() as i32,
            "cvt.rni.s32.f32({x})"
        );
        assert_eq!(truncated[i] as i32, x as i32, "cvt.rzi.s32.f32({x})");
    }
    // The two modes really disagree on this input, so collapsing them onto one kind cannot pass both.
    assert_ne!(nearest, truncated);
    assert_eq!(nearest[0] as i32, 2);
    assert_eq!(nearest[1] as i32, 4);
    assert_eq!(truncated[1] as i32, 3);
}

#[test]
fn float_to_unsigned_keeps_values_above_i32_max() {
    // 3e9 is exactly representable in f32 and above `i32::MAX`: a signed conversion saturates to
    // 2147483647 instead.
    let values = [3_000_000_000.0f32, 2.5, 3.5, 4.5, 0.0, 4_294_967_040.0];
    let input: Vec<u32> = values.iter().map(|x| x.to_bits()).collect();

    let nearest = convert(
        "ld.global.f32 %f1, [%rd8];",
        "cvt.rni.u32.f32 %r7, %f1;",
        "st.global.u32 [%rd10], %r7;",
        &input,
    );
    let truncated = convert(
        "ld.global.f32 %f1, [%rd8];",
        "cvt.rzi.u32.f32 %r7, %f1;",
        "st.global.u32 [%rd10], %r7;",
        &input,
    );
    for (i, &x) in values.iter().enumerate() {
        assert_eq!(
            nearest[i],
            x.round_ties_even() as u32,
            "cvt.rni.u32.f32({x})"
        );
        assert_eq!(truncated[i], x as u32, "cvt.rzi.u32.f32({x})");
    }
    assert_eq!(nearest[0], 3_000_000_000);
    assert_eq!(nearest[1], 2);
    assert_eq!(nearest[2], 4);
    assert_eq!(truncated[2], 3);
}

#[test]
fn conversions_the_ir_cannot_perform_are_rejected_and_the_modeled_ones_compile() {
    for body in [
        // floor/ceil rounding — no `CVT_*` kind performs either.
        "cvt.rmi.s32.f32 %r1, %f1; ret;",
        "cvt.rpi.u32.f32 %r1, %f1; ret;",
        // int→float must be round-to-nearest; the IR has no other int→float rounding.
        "cvt.rz.f32.s32 %f1, %r1; ret;",
        // 64-bit sources and sub-word integer widths.
        "cvt.rn.f32.s64 %f1, %rd1; ret;",
        "cvt.rn.f32.u64 %f1, %rd1; ret;",
        "cvt.s32.s8 %r1, %r2; ret;",
        "cvt.u64.u32 %rd1, %r1; ret;",
        // `.sat` on a float→float conversion clamps to [0,1]; `.ftz` flushes denormal inputs.
        "cvt.sat.f32.f32 %f1, %f2; ret;",
        "cvt.rni.ftz.s32.f32 %r1, %f1; ret;",
    ] {
        assert_rejected(body);
    }
    for body in [
        "cvt.rn.f32.s32 %f1, %r1; ret;",
        "cvt.rn.f32.u32 %f1, %r1; ret;",
        "cvt.rzi.s32.f32 %r1, %f1; ret;",
        "cvt.rzi.u32.f32 %r1, %f1; ret;",
        "cvt.rni.s32.f32 %r1, %f1; ret;",
        "cvt.rni.u32.f32 %r1, %f1; ret;",
        // `.sat` is redundant on a float→int conversion, which clamps by definition.
        "cvt.rzi.sat.s32.f32 %r1, %f1; ret;",
        "cvt.s64.s32 %rd1, %r1; ret;",
        "cvt.u32.s32 %r1, %r2; ret;",
    ] {
        compile(body).unwrap_or_else(|e| panic!("`{body}` must compile, got {e:?}"));
    }
}
