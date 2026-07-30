//! `membar`/`fence` — the PTX scope carried through to [`Inst::Fence`] instead of discarded as a `Nop`.

use super::*;

/// Compile a one-instruction kernel and return its lowered instruction stream.
fn lowered(body: &str) -> Vec<Inst> {
    let src = format!(".visible .entry k(.param .u64 k_param_0) {{\n{body}\n}}");
    ptx::compile(&src, "k", [1, 1, 1]).expect("compiles").insts
}

#[test]
fn each_ptx_scope_maps_onto_its_mem_scope() {
    for (body, scope) in [
        ("membar.cta; ret;", mem_scope::CTA),
        ("membar.gl; ret;", mem_scope::DEVICE),
        ("membar.sys; ret;", mem_scope::SYSTEM),
        ("fence.sc.cta; ret;", mem_scope::CTA),
        ("fence.acq_rel.gpu; ret;", mem_scope::DEVICE),
        ("fence.sc.sys; ret;", mem_scope::SYSTEM),
    ] {
        // A `Nop` lowering (what this used to be) loses the ordering entirely — the moment an executor runs
        // a block's threads concurrently that is an unfixable silent race.
        assert_eq!(
            lowered(body).first(),
            Some(&Inst::Fence { scope }),
            "`{body}` must lower to a fence at scope {scope}"
        );
    }
}

#[test]
fn an_unmodeled_fence_scope_is_rejected_and_the_modeled_ones_compile() {
    // `fence.sc.cluster` orders a cluster of blocks — between `CTA` and `DEVICE`, with no `mem_scope`
    // constant. A scopeless `membar` names nothing at all.
    for body in ["fence.sc.cluster; ret;", "membar; ret;"] {
        assert_rejected(body);
    }
    for body in ["membar.gl; ret;", "fence.sc.cta; ret;"] {
        compile(body).unwrap_or_else(|e| panic!("`{body}` must compile, got {e:?}"));
    }
}

/// A fence inside a real kernel still executes and the kernel's numbers are exact: the rejection above is
/// narrow and the new instruction is not merely accepted by the parser.
#[test]
fn a_kernel_containing_a_fence_computes_exact_results() {
    let source = r#"
    .visible .entry fenced(
        .param .u64 fenced_param_0,
        .param .u64 fenced_param_1,
        .param .u64 fenced_param_2,
        .param .u32 fenced_param_3
    )
    {
        .reg .pred %p<2>;
        .reg .f32  %f<4>;
        .reg .b32  %r<8>;
        .reg .b64  %rd<11>;

        ld.param.u64 %rd1, [fenced_param_0];
        ld.param.u64 %rd2, [fenced_param_1];
        ld.param.u64 %rd3, [fenced_param_2];
        ld.param.u32 %r2,  [fenced_param_3];
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
        add.f32      %f3, %f1, %f2;
        membar.gl;
        st.global.f32 [%rd10], %f3;
    DONE:
        ret;
    }
"#;
    let a = [-3.0f32, 0.5, 7.25, -1.75];
    let b = [1.5f32, -0.25, 2.0, -8.0];
    let got = run(
        source,
        "fenced",
        &a.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        &b.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
    );
    for i in 0..a.len() {
        assert_eq!(f32::from_bits(got[i]).to_bits(), (a[i] + b[i]).to_bits());
    }
}
