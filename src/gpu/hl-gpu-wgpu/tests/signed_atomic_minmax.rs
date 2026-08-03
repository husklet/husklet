//! Signed atomic minimum and maximum lower to something the shipped executor can actually run.
//!
//! The neutral kernel representation is lowered TWICE: once by the guest driver's front end into this IR,
//! and once again here into WGSL for the host device. The second lowering is the one the reference
//! interpreter never performs, so a lowering that refused an operation the interpreter implements was
//! invisible to every unit suite that used the interpreter — including a reduction-pattern suite that
//! exercised `red.global.max.s32` and was green throughout.
//!
//! `atomicMin`/`atomicMax` on a `u32` atomic compare UNSIGNED, so they cannot serve a signed operation the
//! moment a sign bit is set. The lowering therefore used to refuse, and the refusal reached the caller as
//! a kernel that reported success and wrote nothing — the worst shape available for a compute primitive,
//! because every value downstream of it is then quietly wrong and nothing says so.
//!
//! A compare-and-exchange loop expresses it exactly, so the honest outcome is the working one rather than
//! a refusal. This drives the real executor's compute-pipeline path, which performs that second lowering
//! and hands the result to the device — so a WGSL construct the device rejects fails here.

mod gpu_harness;
use gpu_harness::new_session;

use hl_gpu::protocol::model::descriptor::{ComputePipelineDesc, ShaderRef};
use hl_gpu::protocol::model::kernel::{Inst, KernelProgram, Op, Param, ATOM_MAX, ATOM_MIN};
use hl_gpu::{Cmd, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// A kernel whose only work is one signed atomic against region 0.
fn atomic_program(op: u8, unsigned: bool) -> KernelProgram {
    KernelProgram {
        entry: "atom".into(),
        block: [1, 1, 1],
        params: vec![Param {
            width: 8,
            offset: 0,
            is_ptr: true,
            region: 0,
        }],
        param_bytes: 8,
        num_regions: 1,
        shared_bytes: 0,
        reg_count: 4,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::AtomGlobal {
                d: Some(1),
                addr: 0,
                off: 0,
                op,
                cmp: Op::ImmI(0),
                val: Op::ImmI(-7),
                unsigned,
            },
        ],
    }
}

/// Lower a kernel through the executor's compute-pipeline path — the second lowering, into WGSL, that the
/// interpreter never performs — and hand it to the device.
fn lower(exec: &mut WgpuExecutor, program: KernelProgram) -> hl_gpu::Result<()> {
    let mut session = new_session(exec);
    exec.define_kernel(1, program);
    hl_gpu::runtime::submit(
        &mut session,
        exec,
        0,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::PtxKernel,
                spirv: vec![0], // non-empty placeholder; the program comes from define_kernel
            },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "atom".into(),
                    },
                    label: String::new(),
                },
            ),
        ],
    )
    .map(|_| ())
}

/// Both signed forms lower and reach a real compute pipeline. Before, both were refused outright.
#[test]
fn signed_atomic_min_and_max_lower_for_the_device() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    for (name, op) in [("min", ATOM_MIN), ("max", ATOM_MAX)] {
        lower(&mut exec, atomic_program(op, false)).unwrap_or_else(|e| {
            panic!(
                "signed atomic {name} must lower to something the device accepts — refusing it reached a \
                 guest as a kernel that reported success and wrote nothing: {e}"
            )
        });
    }
}

/// THE POSITIVE CONTROL. The unsigned forms use the native WGSL builtins and were never broken; if the
/// change had damaged them, the test above could still pass while the common path regressed.
#[test]
fn unsigned_atomic_min_and_max_still_lower() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    for (name, op) in [("min", ATOM_MIN), ("max", ATOM_MAX)] {
        lower(&mut exec, atomic_program(op, true))
            .unwrap_or_else(|e| panic!("unsigned atomic {name} must still lower: {e}"));
    }
}
