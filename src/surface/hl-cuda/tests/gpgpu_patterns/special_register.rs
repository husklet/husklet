use super::*;

// ==================================================================================================
// 8. special-register OPERANDS — the silent-wrong-result footgun (task #230). Special registers are
//    legal PTX ALU operands used DIRECTLY, with NO `mov` first (e.g.
//    `mad.lo.s32 %idx, %ntid.x, %ctaid.x, %tid.x;` computes blockDim*blockIdx + threadIdx). The old
//    front-end recognized `%ntid.x`/`%tid.x`/… only inside a `mov`; used as an operand they were
//    silently interned as fresh ZERO registers, so every thread computed global index 0. These tests
//    dispatch a multi-block grid and assert the operand form yields the exact per-thread global index,
//    bit-identical to the `mov`-first spelling — and that an UNKNOWN special register operand ERRORS
//    rather than silently zeroing.
// ==================================================================================================

// `out[gidx] = gidx`, with the global index read straight from special registers as `mad` operands.
const SREG_OPERAND_PTX: &str = r#"
    .visible .entry gidx_operand(
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %n, [p_n];
        cvta.to.global.u64 %gout, %rout;
        // NO mov first: special registers are the ALU operands directly.
        mad.lo.s32 %idx, %ntid.x, %ctaid.x, %tid.x;
        setp.ge.s32 %pdone, %idx, %n;
        @%pdone bra DONE;
        mul.wide.s32 %off, %idx, 4;
        add.s64 %addr, %gout, %off;
        st.global.u32 [%addr], %idx;
    DONE:
        ret;
    }
"#;

// Bit-exact reference: the same kernel written the old `mov`-first way.
const SREG_MOVFIRST_PTX: &str = r#"
    .visible .entry gidx_movfirst(
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %n, [p_n];
        cvta.to.global.u64 %gout, %rout;
        mov.u32 %rntid, %ntid.x;
        mov.u32 %rctaid, %ctaid.x;
        mov.u32 %rtid, %tid.x;
        mad.lo.s32 %idx, %rntid, %rctaid, %rtid;
        setp.ge.s32 %pdone, %idx, %n;
        @%pdone bra DONE;
        mul.wide.s32 %off, %idx, 4;
        add.s64 %addr, %gout, %off;
        st.global.u32 [%addr], %idx;
    DONE:
        ret;
    }
"#;

fn run_gidx(ptx_src: &str, entry: &str, grid: u32, block: u32, n: usize) -> Vec<i32> {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(ptx_src.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, entry).unwrap();
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let args = vec![KernelArg::Ptr(d_out), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid, 1, 1),
        (block, 1, 1),
        &args,
    )
    .unwrap();
    bytes_to_i32s(&readback(&mut sink, &ctx, d_out, n * 4))
}

#[test]
fn special_register_as_operand_computes_global_index() {
    let (grid, block) = (5u32, 8u32); // multi-block grid: 40 threads across 5 blocks
    let n = (grid * block) as usize;

    let got_operand = run_gidx(SREG_OPERAND_PTX, "gidx_operand", grid, block, n);
    let got_movfirst = run_gidx(SREG_MOVFIRST_PTX, "gidx_movfirst", grid, block, n);

    // The correct per-thread global index: out[i] == i for every thread in the grid.
    let want: Vec<i32> = (0..n as i32).collect();

    // The operand form must compute the REAL index — not a silent all-zero (the footgun would give
    // out == [0,0,…] since block 0 thread 0 is the only writer of slot 0).
    assert_eq!(
        got_operand, want,
        "sreg-as-operand global index, every thread exact"
    );
    assert!(
        got_operand.iter().any(|&v| v != 0),
        "guards against the all-zero silent-wrong footgun"
    );
    // …and it is bit-identical to the mov-first spelling.
    assert_eq!(
        got_operand, got_movfirst,
        "operand form == mov-first form, bit-exact"
    );
}

#[test]
fn sreg_operand_ir_matches_mov_first() {
    // Same guarantee at the IR level: both spellings compile and, run over identical launch config,
    // must be observationally identical (already asserted above), and both must actually reference the
    // special registers (a MovSReg for each of ntid/ctaid/tid appears in each program).
    let block = [8u32, 1, 1];
    let op = ptx::compile(SREG_OPERAND_PTX, "gidx_operand", block).unwrap();
    let mv = ptx::compile(SREG_MOVFIRST_PTX, "gidx_movfirst", block).unwrap();
    let count_movsreg = |p: &hl_gpu::protocol::model::kernel::KernelProgram| {
        p.insts
            .iter()
            .filter(|i| matches!(i, hl_gpu::protocol::model::kernel::Inst::MovSReg { .. }))
            .count()
    };
    // Operand form materializes its three sregs via a MovSReg prelude; mov-first via its three movs.
    assert_eq!(
        count_movsreg(&op),
        3,
        "operand form materializes ntid/ctaid/tid via MovSReg prelude"
    );
    assert_eq!(
        count_movsreg(&mv),
        3,
        "mov-first form materializes ntid/ctaid/tid via MovSReg"
    );
}

#[test]
fn unknown_special_register_operand_errors_not_silent_zero() {
    // An unknown/mistyped special register used as an operand (`%bogus.x`, `%ntid.w`, or an unmodeled
    // dotless sreg `%warpid`) must ERROR — never be silently interned as a fresh zero register.
    for bad in [
        "mad.lo.s32 %idx, %ntid.x, %ctaid.x, %bogus.x;",
        "mad.lo.s32 %idx, %ntid.w, %ctaid.x, %tid.x;",
        "add.s32 %idx, %warpid, %tid.x;",
    ] {
        let src = format!(
            ".visible .entry k(.param .u64 p_out) {{ ld.param.u64 %r, [p_out]; {bad} ret; }}"
        );
        let r = ptx::compile(&src, "k", [8, 1, 1]);
        assert!(
            r.is_err(),
            "unknown special register must error, got Ok for `{bad}`"
        );
    }

    // The same guard applies to the `mov` form: `mov %r, %ntid.w` is not a silent zero read either.
    let src = ".visible .entry k(.param .u64 p_out) { ld.param.u64 %r, [p_out]; mov.u32 %z, %ntid.w; ret; }";
    assert!(
        ptx::compile(src, "k", [8, 1, 1]).is_err(),
        "unknown sreg in mov must error too"
    );

    // Control: the well-formed operand kernel still compiles.
    assert!(ptx::compile(SREG_OPERAND_PTX, "gidx_operand", [8, 1, 1]).is_ok());
}
