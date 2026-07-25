use super::*;

// ===================================================================================================
// ptx front-end — pointer classification, layout, shared memory, malformed input
// ===================================================================================================

/// saxpy with a scalar BEFORE the pointers: `y[i] = a*x[i] + y[i]`. Exercises f32 scalar + fma + the
/// natural-aligned layout `u32@0, f32@4, u64@8, u64@16` and taint classification of two pointer params.
const SAXPY_PTX: &str = r#"
    .visible .entry saxpy(
        .param .u32 saxpy_param_0,
        .param .f32 saxpy_param_1,
        .param .u64 saxpy_param_2,
        .param .u64 saxpy_param_3
    )
    {
        .reg .pred %p<2>;
        .reg .f32 %f<5>;
        .reg .b32 %r<6>;
        .reg .b64 %rd<9>;

        ld.param.u32  %r2, [saxpy_param_0];
        ld.param.f32  %f1, [saxpy_param_1];
        ld.param.u64  %rd1, [saxpy_param_2];
        ld.param.u64  %rd2, [saxpy_param_3];
        mov.u32       %r3, %ntid.x;
        mov.u32       %r4, %ctaid.x;
        mov.u32       %r5, %tid.x;
        mad.lo.s32    %r1, %r4, %r3, %r5;
        setp.ge.s32   %p1, %r1, %r2;
        @%p1 bra      DONE;
        cvta.to.global.u64 %rd3, %rd1;
        cvta.to.global.u64 %rd4, %rd2;
        mul.wide.s32  %rd5, %r1, 4;
        add.s64       %rd6, %rd3, %rd5;
        add.s64       %rd7, %rd4, %rd5;
        ld.global.f32 %f2, [%rd6];
        ld.global.f32 %f3, [%rd7];
        fma.rn.f32    %f4, %f1, %f2, %f3;
        st.global.f32 [%rd7], %f4;
    DONE:
        ret;
    }
"#;

#[test]
fn saxpy_ptx_classifies_scalar_before_pointers_with_correct_offsets() {
    let prog = ptx::compile(SAXPY_PTX, "saxpy", [128, 1, 1]).unwrap();
    assert_eq!(prog.params.len(), 4);
    // n (u32) scalar, a (f32) scalar, x/y (u64) pointers.
    assert!(!prog.params[0].is_ptr && prog.params[0].offset == 0 && prog.params[0].width == 4);
    assert!(!prog.params[1].is_ptr && prog.params[1].offset == 4 && prog.params[1].width == 4);
    assert!(prog.params[2].is_ptr && prog.params[2].offset == 8 && prog.params[2].region == 0);
    assert!(prog.params[3].is_ptr && prog.params[3].offset == 16 && prog.params[3].region == 1);
    assert_eq!(prog.num_regions, 2);
    assert_eq!(prog.param_bytes, 24);
    assert!(prog
        .insts
        .iter()
        .any(|i| matches!(i, hl_gpu::protocol::model::kernel::Inst::FFma { .. })));
    assert!(prog.insts.iter().any(|i| matches!(i, hl_gpu::protocol::model::kernel::Inst::StGlobal { ty, .. } if *ty == gty::F32)));
}

/// A kernel using `.shared` memory reports the (word-rounded) static shared-byte budget.
const SHARED_PTX: &str = r#"
    .visible .entry red(.param .u64 red_param_0) {
        .shared .align 4 .b8 scratch[100];
        .reg .b64 %rd<2>;
        ld.param.u64 %rd1, [red_param_0];
        bar.sync 0;
        ret;
    }
"#;

#[test]
fn shared_memory_declaration_is_accounted_and_dynamic_shared_is_rejected() {
    let prog = ptx::compile(SHARED_PTX, "red", [64, 1, 1]).unwrap();
    assert_eq!(
        prog.shared_bytes, 100,
        "100 bytes of static shared, word-rounded"
    );

    // Dynamic (extern, unsized) shared is outside the statically-sized subset → a typed Kernel error.
    let dynamic = ".visible .entry k(.param .u64 p) { .extern .shared .align 4 .b8 s[]; ret; }";
    assert!(matches!(
        ptx::compile(dynamic, "k", [1, 1, 1]),
        Err(GpuError::Kernel(_))
    ));
}

#[test]
fn ptx_rejects_array_param_and_bad_type() {
    // struct/array parameters are unsupported.
    let arr = ".visible .entry k(.param .align 8 .b8 k_param_0[64]) { ret; }";
    assert!(matches!(
        ptx::compile(arr, "k", [1, 1, 1]),
        Err(GpuError::Kernel(_))
    ));
    // an unknown scalar param type is rejected.
    let bad = ".visible .entry k(.param .v4 k_param_0) { ret; }";
    assert!(ptx::compile(bad, "k", [1, 1, 1]).is_err());
}

#[test]
fn ptx_module_entry_scan_finds_multiple_entries_in_order() {
    let src = ".visible .entry first() { ret; }\n.entry second(.param .u64 p) { ret; }\n";
    let m = PtxModule::parse(src);
    assert_eq!(m.entries, vec!["first".to_string(), "second".to_string()]);
    // a floating-point atomic is honestly rejected (WGSL has no f32 atomics) rather than mis-lowered.
    let fatom = ".visible .entry k(.param .u64 p) { red.global.add.f32 [%rd1], %f1; ret; }";
    assert!(matches!(
        ptx::compile(fatom, "k", [1, 1, 1]),
        Err(GpuError::Kernel(_))
    ));
}
