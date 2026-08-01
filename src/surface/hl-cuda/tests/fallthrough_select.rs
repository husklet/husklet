//! A value produced on a FALL-THROUGH arm of a select chain must survive to the join point.
//!
//! ## What this reduces
//!
//! `e2e/husklet/apps/cuda-present` runs a Mandelbrot kernel that colours each pixel by selecting one of
//! eight 32-bit constants through a chain of `mov` / `setp.ne` / `@!%p bra STORE`. Seven of the eight
//! arms reach `STORE` by a TAKEN branch. The eighth — the last one — reaches it by falling through, and
//! it is the only one that comes out wrong: every pixel in that band received `0xFF7F7F7F` where the
//! kernel writes the immediate `0xFF7F7FFF`. 108 pixels, and the wrong value was already present in the
//! `cuMemcpyDtoH` readback, so it is upstream of anything graphical.
//!
//! Bands 0..6 use an identical instruction shape with equally large immediates and are byte-exact, so
//! the distinguishing property is the fall-through, not the constant's size and not the select chain in
//! general. This file is that observation reduced to the smallest kernel that can hold it: eight
//! threads, eight constants, one chain, one store.
//!
//! ## What it separates, and what it does not
//!
//! Two lowerings stand between this PTX and a result. The driver's front end
//! ([`hl_cuda::adapter::ptx`]) turns the text into the neutral kernel IR, and the executor turns that IR
//! into something it can run. This file drives the FIRST directly and the second through the reference
//! [`CpuExecutor`] interpreter, which is not the WGSL back end the product ships.
//!
//! So this file answers exactly one question — is the front end dropping or corrupting the value? — and
//! deliberately answers it in isolation:
//!
//!   * `the_frontend_carries_every_immediate` reads the compiled IR with no executor at all, so it
//!     cannot be confused by an execution defect.
//!   * `every_arm_of_the_select_chain_reaches_the_store` runs the same IR on the interpreter.
//!
//! If both pass, the front end and the IR are exonerated and the defect is in the second lowering, which
//! `CpuExecutor` never performs. That case needs a wgpu-executor test beside
//! `hl-gpu-wgpu/tests/signed_atomic_minmax.rs` — a suite that exists precisely because a WGSL lowering
//! once refused an operation the interpreter implemented, which made the defect invisible to every
//! interpreter-based suite. Passing here is therefore NOT a statement that the product is correct; it is
//! a statement about which half to look in.

use hl_cuda::adapter::ptx;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::capability::{shader_payload, Capabilities, COLOR_FORMATS};
use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::kernel::{Inst, KernelDescriptor};
use hl_gpu::{
    BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION,
};

const CUDA_COMMANDS: &[u8] = &[
    etag::BEGIN_COMPUTE_PASS,
    etag::END_COMPUTE_PASS,
    etag::DISPATCH,
    etag::COPY_B2B,
];

/// The eight constants, in the order the chain selects them. These are the `cuda-present` palette; the
/// last one is the fall-through arm and the one observed wrong on the real driver.
const CONSTANTS: [u32; 8] = [
    0xFF00_00FF, 0xFF00_7FFF, 0xFF00_FFFF, 0xFF00_FF00, 0xFFFF_FF00, 0xFFFF_0000, 0xFFFF_00FF,
    0xFF7F_7FFF,
];

/// The poison. No arm of the chain can produce it, so a word still holding it after the launch is a
/// store that never happened — distinct from a store that happened with the wrong value, which is the
/// distinction the original defect turns on.
const POISON: u32 = 0xDEAD_BEEF;

/// `select8(out, n)`: thread `i` selects `CONSTANTS[i]` through a compare chain and stores it.
///
/// The shape is the point. Arms 0..6 each end in `@!%pb bra STORE`, so they reach the join by a TAKEN
/// branch. Arm 7 has no branch after its `mov` and reaches the join by falling through — the only
/// structural difference between it and the seven arms that work on the real driver.
fn select_ptx() -> String {
    let mut body = String::from(
        r#"
    .version 7.5
    .target sm_86
    .address_size 64

    .visible .entry select8(
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %pout, [p_out];
        ld.param.u32 %n, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %cb, %ctaid.x;
        mov.u32 %t, %tid.x;
        mad.lo.s32 %gid, %cb, %nt, %t;
        setp.ge.s32 %poob, %gid, %n;
        @%poob bra DONE;
        mov.u32 %band, %gid;
"#,
    );
    for (index, constant) in CONSTANTS.iter().enumerate() {
        body.push_str(&format!("        mov.u32 %px, {constant};\n"));
        // The last arm gets no branch: it falls through into STORE.
        if index + 1 < CONSTANTS.len() {
            body.push_str(&format!(
                "        setp.ne.s32 %pb, %band, {index};\n        @!%pb bra STORE;\n"
            ));
        }
    }
    body.push_str(
        r#"    STORE:
        cvta.to.global.u64 %gout, %pout;
        mul.wide.s32 %off, %gid, 4;
        add.s64 %ptr, %gout, %off;
        st.global.u32 [%ptr], %px;
    DONE:
        ret;
    }
"#,
    );
    body
}

fn harness() -> InProcessCommandSink<CpuExecutor> {
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
    sink.negotiate(&request).expect("negotiate against CpuExecutor");
    sink
}

fn readback(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &CudaContext,
    pointer: DevicePtr,
    len: usize,
) -> Vec<u32> {
    let (buffer, offset): (BufferId, u64) = ctx.device_location(pointer).unwrap();
    sink.read_buffer(buffer, offset, len)
        .unwrap()
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// The front end alone: does the compiled IR still contain every constant the PTX names?
///
/// No executor is involved, so this cannot be satisfied or defeated by an execution defect. If a
/// constant is missing here, the value never entered the IR and nothing downstream could have written
/// it; if every constant is present, the front end carried them and the question moves to the lowering
/// that consumes the IR.
#[test]
fn the_frontend_carries_every_immediate() {
    let program = ptx::compile(&select_ptx(), "select8", [8, 1, 1]).expect("select8 must compile");
    let immediates: Vec<u64> = program
        .insts
        .iter()
        .filter_map(|inst| match inst {
            Inst::MovImmI { imm, .. } => Some(*imm),
            _ => None,
        })
        .collect();
    for (index, constant) in CONSTANTS.iter().enumerate() {
        assert!(
            immediates.contains(&u64::from(*constant)),
            "the front end dropped or altered arm {index}'s immediate {constant:#010x}; the IR's \
             MovImmI values are {immediates:#x?}. A constant that is not in the IR cannot be stored \
             by any executor, so this is a front-end defect and not a lowering one."
        );
    }
}

/// Every arm, including the one that falls through, must reach the store with its own value.
///
/// The output buffer is poisoned before the launch. Without that, an executor that skipped the store
/// entirely would be graded against whatever the allocation happened to contain, and a zeroed
/// allocation would make a missing store indistinguishable from a store of zero.
#[test]
fn every_arm_of_the_select_chain_reaches_the_store() {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let ptx_text = select_ptx();
    let module = ctx.load_module(ptx_text.as_bytes()).expect("select8 module must load");
    let function =
        load_module::module_get_function(&ctx, module, "select8").expect("select8 must resolve");

    let count = CONSTANTS.len();
    let poison: Vec<u8> = std::iter::repeat(POISON)
        .take(count)
        .flat_map(|w| w.to_le_bytes())
        .collect();
    let out = allocate::mem_alloc(&mut ctx, &mut sink, poison.len() as u64).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, out, &poison).unwrap();

    // The poison is really there before the launch. An "output nobody produced" is invisible unless the
    // buffer is known to have held something else first, and asserting that is cheaper than assuming it.
    assert_eq!(
        readback(&mut sink, &ctx, out, poison.len()),
        vec![POISON; count],
        "the poison did not reach the device, so this test could not tell a missing store from a \
         correct one and its result below would be vacuous"
    );

    let args = vec![
        KernelArg::Ptr(out),
        KernelArg::Scalar((count as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        function,
        (1, 1, 1),
        (count as u32, 1, 1),
        &args,
    )
    .expect("select8 must launch");

    let observed = readback(&mut sink, &ctx, out, poison.len());
    let unwritten: Vec<usize> = (0..count).filter(|&i| observed[i] == POISON).collect();
    assert!(
        unwritten.is_empty(),
        "arms {unwritten:?} never stored anything: their words still hold the poison {POISON:#010x}. \
         That is a missing store, not a wrong value."
    );
    for (index, expected) in CONSTANTS.iter().enumerate() {
        assert_eq!(
            observed[index], *expected,
            "arm {index} stored {:#010x} where the PTX names {expected:#010x}. Arm {} is the only one \
             that reaches the store by FALLING THROUGH rather than by a taken branch, which is the \
             property the real-driver defect tracks.",
            observed[index],
            CONSTANTS.len() - 1
        );
    }
}
