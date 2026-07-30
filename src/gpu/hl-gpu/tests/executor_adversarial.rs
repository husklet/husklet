//! Adversarial coverage for the CPU reference executor (the semantic ORACLE) + the runtime validation it
//! sits behind. A malformed-but-decodable batch — an out-of-bounds copy/write/read, a use-after-free, a
//! duplicate id, a draw that overruns its vertex buffer, a dispatch with nothing bound, an over-huge grid —
//! must produce a TYPED error (`OutOfBounds` / `UnknownId` / `DuplicateId` / `Invalid`), never memory
//! corruption, a panic, or a hang. Every rejection is atomic: the command-buffer is fully validated before
//! any mutation, so a bad op late in a submit leaves earlier state untouched.
//!
//! These drive `GpuExecutor::execute` directly over a fresh `SessionResources` (isolating the executor's
//! own validation from the runtime residency layer), plus a couple of runtime-pipeline checks.

use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::protocol::model::kernel::{
    glsl_stage, gty, GlslDescriptor, Inst, KernelProgram, Op, Param, KERNEL_MAGIC,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, CpuExecutor, Enc, GpuError, GpuExecutor, InProcessCommandSink,
    SessionResources, ShaderPayloadKind, TextureId,
};

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        label: String::new(),
    }
}

fn tex(w: u32, h: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}

/// A fresh executor + resources with `setup` already applied (asserted clean).
fn primed(setup: &[Cmd]) -> (CpuExecutor, SessionResources) {
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    exec.execute(&mut res, setup)
        .expect("setup must run cleanly");
    (exec, res)
}

fn submit(ops: Vec<Enc>) -> Cmd {
    Cmd::Submit(CommandBuffer {
        encoder: ops,
        signal: None,
    })
}

// ---------------------------------------------------------------------------------------------------
// resource lifecycle: duplicate create, use-after-free, double-free, empty batch
// ---------------------------------------------------------------------------------------------------

/// A trivial real kernel (block 1x1x1): store the constant `1.0f` into region 0 (binding 1), driven via
/// `define_kernel`. Used to exercise the dispatch grid-block ceiling over a REAL program (a SPIR-V module
/// would short-circuit before the kernel runs).
fn store_one_program() -> KernelProgram {
    KernelProgram {
        entry: "store_one".into(),
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
        reg_count: 3,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::Cvta { d: 1, s: 0 },
            Inst::MovImmF {
                d: 2,
                bits: 0x3F80_0000,
            }, // 1.0f
            Inst::StGlobal {
                addr: 1,
                off: 0,
                src: Op::Reg(2),
                ty: gty::F32,
            },
            Inst::Ret,
        ],
    }
}

/// The dispatch setup commands for [`store_one_program`]: shader + compute pipeline + a param buffer
/// (binding 0) and a 4-byte output buffer (binding 1 -> region 0).
fn store_one_setup() -> Vec<Cmd> {
    vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: vec![KERNEL_MAGIC, 0],
        },
        Cmd::CreateComputePipeline(
            1,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 1,
                    entry: "store_one".into(),
                },
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(1, buf(8, buffer_usage::STORAGE)),
        Cmd::CreateBuffer(2, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 8,
                        },
                    },
                    BindEntry {
                        binding: 1,
                        resource: BindResource::Buffer {
                            id: 2,
                            offset: 0,
                            size: 4,
                        },
                    },
                ],
            },
        ),
    ]
}

#[path = "executor_adversarial/compute.rs"]
mod compute;
#[path = "executor_adversarial/raster.rs"]
mod raster;
#[path = "executor_adversarial/presentation.rs"]
mod presentation;
#[path = "executor_adversarial/resources.rs"]
mod resources;
#[path = "executor_adversarial/shader.rs"]
mod shader;
#[path = "executor_adversarial/synchronization.rs"]
mod synchronization;
#[path = "executor_adversarial/validation.rs"]
mod validation;
