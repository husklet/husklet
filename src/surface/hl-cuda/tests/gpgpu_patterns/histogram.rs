use super::*;

// ==================================================================================================
// 3. histogram — K=16 bins over N inputs. Two variants, both exact under contention:
//    (a) GLOBAL atomics: every lane does red.global.add into its bin.
//    (b) SHARED-PRIVATIZED: each block accumulates a private histogram in `.shared` via shared atomics,
//        then merges it into the global histogram with one global atomic per bin (the standard fast path).
//    bin = value & (K-1) with K=16 a power of two (the interpreter models no integer modulo).
// ==================================================================================================

const HIST_GLOBAL_PTX: &str = r#"
    .visible .entry hist_global(
        .param .u64 p_in,
        .param .u64 p_hist,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rhist, [p_hist];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        and.b32 %bin, %v, 15;
        cvta.to.global.u64 %gh, %rhist;
        mul.wide.s32 %boff, %bin, 4;
        add.s64 %ph, %gh, %boff;
        red.global.add.u32 [%ph], 1;
    DONE:
        ret;
    }
"#;

const HIST_SHARED_PTX: &str = r#"
    .visible .entry hist_shared(
        .param .u64 p_in,
        .param .u64 p_hist,
        .param .u32 p_n
    )
    {
        .shared .align 4 .b32 sh[16];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rhist, [p_hist];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        // zero the private histogram: lanes 0..16 clear one bin each.
        setp.ge.s32 %pz, %tt, 16;
        @%pz bra AFTERZERO;
        mul.lo.s32 %zoff, %tt, 4;
        st.shared.u32 [%zoff], 0;
    AFTERZERO:
        bar.sync;
        // accumulate into the private (shared) histogram with shared atomics.
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra AFTERACC;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        and.b32 %bin, %v, 15;
        mul.lo.s32 %sboff, %bin, 4;
        red.shared.add.u32 [%sboff], 1;
    AFTERACC:
        bar.sync;
        // merge: lanes 0..16 add one private bin into the global histogram.
        setp.ge.s32 %pm, %tt, 16;
        @%pm bra DONE;
        mul.lo.s32 %moff, %tt, 4;
        ld.shared.u32 %cnt, [%moff];
        cvta.to.global.u64 %gh, %rhist;
        mul.wide.s32 %goff, %tt, 4;
        add.s64 %ph, %gh, %goff;
        red.global.add.u32 [%ph], %cnt;
    DONE:
        ret;
    }
"#;

#[test]
fn histogram_atomic_global_and_shared_exact() {
    let k = 16usize;
    let n = 5000usize;
    // Skewed distribution so several bins receive heavy contention.
    let input: Vec<i32> = (0..n).map(|i| ((i * 2718281 + 13) % 251) as i32).collect();

    // Reference bin counts: bin = value & 15.
    let mut want = vec![0i32; k];
    for &v in &input {
        want[(v as u32 & 15) as usize] += 1;
    }
    assert!(
        want.iter().filter(|&&c| c > 0).count() >= 8,
        "distribution must actually spread across bins"
    );

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module_g = ctx.load_module(HIST_GLOBAL_PTX.as_bytes()).unwrap();
    let gfn = load_module::module_get_function(&ctx, module_g, "hist_global").unwrap();
    let module_s = ctx.load_module(HIST_SHARED_PTX.as_bytes()).unwrap();
    let sfn = load_module::module_get_function(&ctx, module_s, "hist_shared").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let grid = n.div_ceil(256) as u32; // 20 blocks × 256 threads

    // (a) global-atomic histogram
    let d_hist_g = alloc_zeroed_i32(&mut sink, &mut ctx, k);
    let args_g = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_hist_g), sc(n as i32)];
    launch::launch(&mut ctx, &mut sink, gfn, (grid, 1, 1), (256, 1, 1), &args_g).unwrap();
    let got_g = bytes_to_i32s(&readback(&mut sink, &ctx, d_hist_g, k * 4));
    assert_eq!(got_g, want, "global-atomic histogram bin counts exact");
    assert_eq!(
        got_g.iter().sum::<i32>(),
        n as i32,
        "every input counted exactly once"
    );

    // (b) shared-privatized histogram
    let d_hist_s = alloc_zeroed_i32(&mut sink, &mut ctx, k);
    let args_s = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_hist_s), sc(n as i32)];
    launch::launch(&mut ctx, &mut sink, sfn, (grid, 1, 1), (256, 1, 1), &args_s).unwrap();
    let got_s = bytes_to_i32s(&readback(&mut sink, &ctx, d_hist_s, k * 4));
    assert_eq!(got_s, want, "shared-privatized histogram bin counts exact");
    assert_eq!(got_s, got_g, "shared and global histograms agree exactly");
}
