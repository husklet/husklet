use super::*;

// ---------------------------------------------------------------------------------------------------
// adapter::ptx — the parser
// ---------------------------------------------------------------------------------------------------

#[test]
fn vecadd_ptx_compiles_to_expected_program() {
    let prog = ptx::compile(ptx::VECADD_PTX, "vecadd", [256, 1, 1]).unwrap();
    assert_eq!(prog.entry, "vecadd");
    assert_eq!(prog.block, [256, 1, 1]);
    assert_eq!(prog.shared_bytes, 0);

    // 4 params: three device pointers (a, b, c) + one scalar (n). Pointer classification via the
    // forward taint pass over cvta/global accesses.
    assert_eq!(prog.params.len(), 4);
    assert_eq!(prog.num_regions, 3);
    assert!(prog.params[0].is_ptr && prog.params[0].region == 0);
    assert!(prog.params[1].is_ptr && prog.params[1].region == 1);
    assert!(prog.params[2].is_ptr && prog.params[2].region == 2);
    assert!(!prog.params[3].is_ptr);

    // natural-aligned flat layout: u64@0, u64@8, u64@16, u32@24 → 28-byte blob.
    assert_eq!(prog.params[0].offset, 0);
    assert_eq!(prog.params[1].offset, 8);
    assert_eq!(prog.params[2].offset, 16);
    assert_eq!(prog.params[3].offset, 24);
    assert_eq!(prog.param_bytes, 28);

    // the body ends in a ret, computes the global index (a mad), and does the elementwise f32 add/store.
    assert_eq!(prog.insts.last(), Some(&Inst::Ret));
    assert!(prog.insts.iter().any(|i| matches!(i, Inst::IMad { .. })));
    assert!(prog.insts.iter().any(|i| matches!(i, Inst::FAdd { .. })));
    assert!(prog
        .insts
        .iter()
        .any(|i| matches!(i, Inst::StGlobal { ty, .. } if *ty == gty::F32)));
    assert!(prog
        .insts
        .iter()
        .any(|i| matches!(i, Inst::LdGlobal { ty, .. } if *ty == gty::F32)));
}

#[test]
fn ptx_unknown_entry_errors() {
    assert!(matches!(
        ptx::compile(ptx::VECADD_PTX, "nope", [1, 1, 1]),
        Err(GpuError::Kernel(_))
    ));
}

#[test]
fn ptx_unsupported_opcode_errors() {
    let bad = ".visible .entry k() { shfl.sync.idx.b32 %r1, %r2, 0, 31, 0; ret; }";
    assert!(matches!(
        ptx::compile(bad, "k", [1, 1, 1]),
        Err(GpuError::Kernel(_))
    ));
}
