use super::*;

// ==================================================================================================
// 4. constant_memory — a `.const` global set via cudaMemcpyToSymbol, read in a kernel; both exact.
// ==================================================================================================

/// A module declaring a 4-element `.const` coefficient array plus a kernel that reads coeff[0]/coeff[1]
/// (passed as the symbol's device pointer) and applies `data[i] = coeff[0]*data[i] + coeff[1]`.
const CONST_PTX: &str = r#"
    .const .align 4 .f32 kCoeff[4];

    .visible .entry apply(
        .param .u64 ap_coeff,
        .param .u64 ap_data,
        .param .u32 ap_n
    )
    {
        ld.param.u64  %rc, [ap_coeff];
        ld.param.u64  %rd, [ap_data];
        ld.param.u32  %rn, [ap_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gc, %rc;
        cvta.to.global.u64 %gd, %rd;
        ld.global.f32 %c0, [%gc];
        ld.global.f32 %c1, [%gc+4];
        mul.wide.s32  %off, %ri, 4;
        add.s64       %pd, %gd, %off;
        ld.global.f32 %v, [%pd];
        fma.rn.f32    %r, %c0, %v, %c1;
        st.global.f32 [%pd], %r;
    DONE:
        ret;
    }
"#;

#[test]
fn constant_memory_symbol_set_from_host_read_in_kernel_exact() {
    let n = 400usize;
    let coeff = [2.0f32, 5.0f32, 0.0f32, 0.0f32]; // scale=2, bias=5
    let data0: Vec<f32> = (0..n).map(|i| (i as f32) * 0.1 - 20.0).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(CONST_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "apply").unwrap();

    // cudaGetSymbolAddress: the `.const kCoeff` symbol resolves to a real device pointer of 16 bytes.
    let (coeff_ptr, size) =
        symbol::get_symbol_address(&mut ctx, &mut sink, module, "kCoeff").unwrap();
    assert_eq!(size, 16, "kCoeff is 4 × f32 = 16 bytes");

    // cudaMemcpyToSymbol: set the constant from the host.
    symbol::memcpy_to_symbol(
        &mut ctx,
        &mut sink,
        module,
        "kCoeff",
        &f32s_to_bytes(&coeff),
    )
    .unwrap();

    // cudaMemcpyFromSymbol: the host round-trips the symbol back, bit-exact.
    let echoed = bytes_to_f32s(
        &symbol::memcpy_from_symbol(&mut ctx, &mut sink, module, "kCoeff", 16).unwrap(),
    );
    assert_eq!(
        echoed, coeff,
        "the constant reads back exactly what the host wrote"
    );

    // An unknown symbol is the honest cudaErrorInvalidSymbol analogue — never a fake pointer.
    assert!(symbol::get_symbol_address(&mut ctx, &mut sink, module, "nope").is_err());

    // Kernel reads the constant (via the symbol's device pointer) and transforms the data.
    let d_data = upload(&mut sink, &mut ctx, &f32s_to_bytes(&data0));
    let args = vec![
        KernelArg::Ptr(coeff_ptr),
        KernelArg::Ptr(d_data),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (4, 1, 1), (128, 1, 1), &args).unwrap();

    let got = bytes_to_f32s(&readback(&mut sink, &ctx, d_data, n * 4));
    let want: Vec<f32> = data0
        .iter()
        .map(|v| coeff[0].mul_add(*v, coeff[1]))
        .collect();
    assert_eq!(
        got, want,
        "kernel output uses the host-set constant, all {n} elements exact"
    );
}
