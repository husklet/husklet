//! Truthful-failure coverage for the PTX front-end: every construct the modeled 32-bit kernel IR cannot
//! represent must produce a typed [`GpuError::Kernel`] (→ `CUDA_ERROR_INVALID_PTX`), never a silently
//! mis-lowered instruction that computes a plausible wrong number.
//!
//! Each case below was previously accepted and lowered onto a DIFFERENT operation. The companion
//! `executes_the_modeled_equivalent_exactly` test runs the semantically-equivalent kernel that stays
//! inside the subset through the real executor and asserts the exact computed bytes, so these rejections
//! are proven to be narrow rather than a blanket refusal.

use hl_cuda::adapter::ptx;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, KernelArg};

use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::capability::{
    shader_payload, Capabilities, COLOR_FORMATS,
};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{
    CommandSink, CpuExecutor, FeatureRequest, GpuError, InProcessCommandSink, WIRE_VERSION,
};

/// The encoder commands the CUDA lowering actually emits: a compute pass with a dispatch, plus the
/// on-device buffer copy `cuMemcpyDtoD` lowers to. A CUDA driver encodes no render pass and no texture
/// copy, so negotiating the full command set would claim a surface this driver never uses — and would be
/// refused by any executor that honestly advertises less than everything.
const CUDA_COMMANDS: &[u8] = &[
    etag::BEGIN_COMPUTE_PASS,
    etag::END_COMPUTE_PASS,
    etag::DISPATCH,
    etag::COPY_B2B,
];


/// Wrap a kernel body in a one-entry PTX module and compile it.
fn compile(body: &str) -> Result<(), GpuError> {
    let src = format!(".visible .entry k(.param .u64 k_param_0) {{\n{body}\n}}");
    ptx::compile(&src, "k", [1, 1, 1]).map(|_| ())
}

/// Assert `body` is rejected with a typed kernel error rather than compiled.
fn assert_rejected(body: &str, what: &str) {
    match compile(body) {
        Err(GpuError::Kernel(_)) => {}
        other => panic!("{what} must be a typed kernel error, got {other:?}"),
    }
}

#[test]
fn predicated_non_branch_instructions_are_rejected() {
    // `@%p1 st.global.f32` stores ONLY when the predicate holds. `Inst` carries a predicate on `Bra`
    // alone, so the guard used to be parsed and discarded — the store then ran for every thread.
    assert_rejected(
        "ld.param.u64 %rd1, [k_param_0]; \
         cvta.to.global.u64 %rd2, %rd1; \
         setp.ge.s32 %p1, %r1, %r2; \
         @%p1 st.global.f32 [%rd2], %f1; \
         ret;",
        "predicated store",
    );
    assert_rejected(
        "setp.ge.s32 %p1, %r1, %r2; @!%p1 mov.u32 %r3, 7; ret;",
        "predicated mov",
    );
    assert_rejected(
        "setp.ge.s32 %p1, %r1, %r2; @%p1 add.s32 %r3, %r4, %r5; ret;",
        "predicated add",
    );

    // The one predicated form the IR DOES model still compiles.
    assert!(
        compile("setp.ge.s32 %p1, %r1, %r2; @%p1 bra DONE; DONE: ret;").is_ok(),
        "`@p bra` is modeled and must keep working"
    );
}

#[test]
fn floating_point_comparisons_are_rejected() {
    // `Inst::Setp` compares 32-bit integers. A signed-integer compare of f32 bit patterns agrees with
    // IEEE-754 ordering only while both operands are non-negative; with a negative operand it inverts.
    for cmp in ["lt", "le", "gt", "ge", "eq", "ne"] {
        assert_rejected(
            &format!("setp.{cmp}.f32 %p1, %f1, %f2; ret;"),
            &format!("setp.{cmp}.f32"),
        );
    }
    // Integer compares are unaffected.
    assert!(compile("setp.lt.s32 %p1, %r1, %r2; ret;").is_ok());
    assert!(compile("setp.lt.u32 %p1, %r1, %r2; ret;").is_ok());
}

#[test]
fn double_and_half_precision_operations_are_rejected() {
    // The register file and every arithmetic `Inst` are 32-bit. `add.f64` used to fall through to the
    // INTEGER add (no `f32` token in the opcode), so a double add returned integer-added mantissa bits.
    for body in [
        "add.f64 %fd1, %fd2, %fd3; ret;",
        "sub.f64 %fd1, %fd2, %fd3; ret;",
        "mul.f64 %fd1, %fd2, %fd3; ret;",
        "fma.rn.f64 %fd1, %fd2, %fd3, %fd4; ret;",
        "add.f16 %h1, %h2, %h3; ret;",
        "add.bf16 %h1, %h2, %h3; ret;",
        "ld.param.f64 %fd1, [k_param_0]; ret;",
        "ld.global.f64 %fd1, [%rd1]; ret;",
    ] {
        assert_rejected(body, body);
    }
    // f32 arithmetic is the modeled width and still compiles.
    assert!(compile("add.f32 %f1, %f2, %f3; fma.rn.f32 %f4, %f1, %f2, %f3; ret;").is_ok());
}

#[test]
fn unmodeled_conversions_are_rejected_instead_of_reinterpreted() {
    // `cvt.rn.f32.u32` is an unsigned-int → float conversion that nvcc emits constantly. It used to fall
    // into `CVT_IDENTITY`, i.e. the integer's BITS were handed on as an f32.
    for body in [
        "cvt.rn.f32.u32 %f1, %r1; ret;",
        "cvt.rzi.u32.f32 %r1, %f1; ret;",
        "cvt.rn.f32.s64 %f1, %rd1; ret;",
        "cvt.s32.s8 %r1, %r2; ret;",
        "cvt.u64.u32 %rd1, %r1; ret;",
    ] {
        assert_rejected(body, body);
    }
    // The three conversions the executor really performs, plus a same-width integer reinterpretation.
    for body in [
        "cvt.rn.f32.s32 %f1, %r1; ret;",
        "cvt.rzi.s32.f32 %r1, %f1; ret;",
        "cvt.s64.s32 %rd1, %r1; ret;",
        "cvt.u32.s32 %r1, %r2; ret;",
    ] {
        assert!(compile(body).is_ok(), "`{body}` is modeled");
    }
}

#[test]
fn wide_and_high_multiplies_are_rejected() {
    // `IMad` is the 32-bit `.lo` form and `IMul` is `.lo` or 32x32->64 `wide`; every other shape used to
    // silently report the low 32 bits of a different product.
    for body in [
        "mad.hi.s32 %r1, %r2, %r3, %r4; ret;",
        "mad.wide.s32 %rd1, %r2, %r3, %rd4; ret;",
        "mad.lo.s64 %rd1, %rd2, %rd3, %rd4; ret;",
        "mul.hi.s32 %r1, %r2, %r3; ret;",
        "mul.lo.s64 %rd1, %rd2, %rd3; ret;",
    ] {
        assert_rejected(body, body);
    }
    assert!(compile("mad.lo.s32 %r1, %r2, %r3, %r4; ret;").is_ok());
    assert!(compile("mul.wide.s32 %rd1, %r1, 4; ret;").is_ok());
    assert!(compile("mul.lo.s32 %r1, %r2, %r3; ret;").is_ok());
}

#[test]
fn wrapping_atomics_are_rejected() {
    // `atom.global.inc.u32` wraps at the operand value; it used to be mapped onto a plain atomic add,
    // which agrees only until the counter reaches the wrap point.
    assert_rejected("atom.global.inc.u32 %r1, [%rd1], %r2; ret;", "atom.inc");
    assert_rejected("red.global.dec.u32 [%rd1], %r2; ret;", "red.dec");
    assert!(compile("atom.global.add.u32 %r1, [%rd1], %r2; ret;").is_ok());
    assert!(compile("red.global.max.s32 [%rd1], %r2; ret;").is_ok());
}

#[test]
fn a_kernel_exceeding_the_register_file_is_rejected() {
    // Register indices are `u16`. More than `u16::MAX` distinct names used to truncate, aliasing two
    // registers onto one index and reporting a `reg_count` below an already-issued index.
    let mut body = String::new();
    for i in 0..70_000u32 {
        body.push_str(&format!("mov.u32 %r{i}, {i}; "));
    }
    body.push_str("ret;");
    assert_rejected(&body, "a 70000-register kernel");
}

/// The launch geometry limits must match what `cuDeviceGetAttribute` advertises: a grid or block axis past
/// the advertised maximum is `CUDA_ERROR_INVALID_VALUE`, not an accepted dispatch the described device
/// could never perform.
#[test]
fn launch_geometry_past_the_advertised_per_axis_limits_is_rejected() {
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let mut sink = hl_gpu::RecordingSink::with_full_caps();
    let module = ctx.load_module(ptx::VECADD_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "vecadd").unwrap();

    // `maxThreadsDim` is [1024, 1024, 64]; z = 65 is over the limit while the thread product stays legal.
    assert!(matches!(
        launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (1, 1, 65), &[]),
        Err(GpuError::Invalid(_))
    ));
    // `maxGridSize` is [2^31-1, 65535, 65535].
    assert!(matches!(
        launch::launch(&mut ctx, &mut sink, func, (1, 65_536, 1), (1, 1, 1), &[]),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        launch::launch(&mut ctx, &mut sink, func, (1, 1, 65_536), (1, 1, 1), &[]),
        Err(GpuError::Invalid(_))
    ));
    assert!(
        sink.commands().count() == 0,
        "a rejected launch must emit nothing"
    );
}

/// The configured device identity (`HL_CUDA_NAME` / `HL_CUDA_CC`) is what every guest library reports.
#[test]
fn the_configured_device_identity_is_applied_and_defaults_are_kept() {
    let mut device = CudaDeviceDesc::apple_default(4 << 30);
    device.configure(Some("NVIDIA GeForce RTX 3060"), Some("7.5"));
    assert_eq!(device.name, "NVIDIA GeForce RTX 3060");
    assert_eq!(device.compute_capability, (7, 5));
    assert_eq!(device.compute_capability_str(), "7.5");

    // A bare major, and empty/absent/unparsable input leaving the current value untouched.
    device.configure(None, Some("9"));
    assert_eq!(device.compute_capability, (9, 0));
    device.configure(Some("  "), Some("not-a-capability"));
    assert_eq!(device.name, "NVIDIA GeForce RTX 3060");
    assert_eq!(device.compute_capability, (9, 0));
}

/// The rejections above are narrow: the equivalent kernel that stays inside the modeled subset still runs
/// through the real lowering + executor and produces exact results. `masked_scale` uses `@p bra` (not a
/// predicated store), an integer compare, and `cvt.rn.f32.s32` — every construct the IR truly carries.
const MASKED_SCALE_PTX: &str = r#"
    .visible .entry masked_scale(
        .param .u64 masked_scale_param_0,
        .param .u64 masked_scale_param_1,
        .param .u32 masked_scale_param_2
    )
    {
        .reg .pred %p<2>;
        .reg .f32  %f<4>;
        .reg .b32  %r<6>;
        .reg .b64  %rd<9>;

        ld.param.u64 %rd1, [masked_scale_param_0];
        ld.param.u64 %rd2, [masked_scale_param_1];
        ld.param.u32 %r2,  [masked_scale_param_2];
        mov.u32      %r3, %ntid.x;
        mov.u32      %r4, %ctaid.x;
        mov.u32      %r5, %tid.x;
        mad.lo.s32   %r1, %r4, %r3, %r5;
        setp.ge.s32  %p1, %r1, %r2;
        @%p1 bra     DONE;

        cvta.to.global.u64 %rd3, %rd1;
        cvta.to.global.u64 %rd4, %rd2;
        mul.wide.s32 %rd5, %r1, 4;
        add.s64      %rd6, %rd3, %rd5;
        add.s64      %rd7, %rd4, %rd5;
        ld.global.f32 %f1, [%rd6];
        cvt.rn.f32.s32 %f2, %r1;
        fma.rn.f32   %f3, %f1, %f1, %f2;
        st.global.f32 [%rd7], %f3;

    DONE:
        ret;
    }
"#;

#[test]
fn executes_the_modeled_equivalent_exactly() {
    let input = [-3.0f32, -0.5, 0.0, 2.5, 7.25, -1.75, 4.0, 100.0];
    let n = input.len() as u32;

    let mut exec = CpuExecutor::new();
    exec.set_kernel_compiler(|desc: &KernelDescriptor| {
        ptx::compile(&desc.ptx, &desc.entry, desc.block)
    });
    let mut sink = InProcessCommandSink::new(exec);
    let request = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: Capabilities::command_bits(CUDA_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
        ..FeatureRequest::default()
    };
    sink.negotiate(&request).expect("negotiate");

    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(MASKED_SCALE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "masked_scale").unwrap();

    let bytes = u64::from(n) * 4;
    let src = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let dst = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let host: Vec<u8> = input.iter().flat_map(|x| x.to_le_bytes()).collect();
    transfer::memcpy_htod(&mut ctx, &mut sink, src, &host).unwrap();

    let args = [
        KernelArg::Ptr(src),
        KernelArg::Ptr(dst),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (n, 1, 1), &args).unwrap();

    let out = transfer::read_dtoh(&ctx, &mut sink, dst, bytes as usize).unwrap();
    for (i, chunk) in out.chunks_exact(4).enumerate() {
        let got = f32::from_le_bytes(chunk.try_into().unwrap());
        // `fma.rn.f32(x, x, i)` is a single-rounding fused multiply-add: bit-exact against `f32::mul_add`.
        let want = input[i].mul_add(input[i], i as f32);
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "masked_scale[{i}]: got {got}, want {want}"
        );
    }
}
