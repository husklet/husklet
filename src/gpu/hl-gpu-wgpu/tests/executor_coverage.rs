//! Adversarial + coverage suite for the wgpu executor, complementing the frozen `conformance.rs` mirror.
//!
//! Everything here drives the SAME runtime pipeline (validate → account → dispatch → execute) against a
//! `WgpuExecutor` on the headless software Vulkan device (lavapipe / `llvmpipe`), but pushes far past the
//! frozen suite: odd-width / multi-format texture readback repack, sub-region + tight (`bytes_per_row==0`)
//! copies, multi-workgroup + atomic compute, real vertex-buffer draws (multiple attributes, indexed,
//! per-instance step mode), viewport/scissor, a depth-tested draw, GLSL execution, and a wall of error /
//! capability-honesty paths that must return a clean `Err` (never panic) or match the advertisement.

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::capability::{shader_payload, PresentKind};
use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    ComputePipelineDesc, DepthAttachment, DepthState, Extent3d, Origin3d, RenderPipelineDesc,
    ShaderRef, TextureDesc, TextureSubresource, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{
    glsl_stage, gty, GlslDescriptor, Inst, KernelProgram, Op, Param, ATOM_ADD, CMP_GE,
    KERNEL_MAGIC, SPIRV_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// -------------------------------------------------------------------------------------------------
// shared device + runtime-pipeline harness (own EXEC — a test binary is its own process)
// -------------------------------------------------------------------------------------------------

static EXEC: OnceLock<Mutex<WgpuExecutor>> = OnceLock::new();

fn exec() -> MutexGuard<'static, WgpuExecutor> {
    EXEC.get_or_init(|| {
        Mutex::new(
            WgpuExecutor::new(DeviceConfig::default())
                .expect("acquire a wgpu adapter (is a Vulkan ICD / lavapipe reachable?)"),
        )
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

/// Run `cmds` through the whole runtime pipeline, returning the `Result` (so error paths can assert a
/// clean `Err` with no panic). Byte-addressable copy alignment (1) matches the oracle harness.
fn try_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> hl_gpu::Result<Session> {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds)?;
    Ok(s)
}

fn run_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> Session {
    try_batch(exec, cmds).expect("batch must run cleanly")
}

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

const RT: u32 = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST;

/// Pack a vertex-attribute format the way the GL driver's `vertex_format_wire` does:
/// `comps | (kind<<8) | (normalized<<16)`. kinds: 0=f32 1=u8 2=i8 3=u16 4=i16 5=u32 6=i32 7=f16.
fn vfmt(comps: u32, kind: u32, normalized: bool) -> u32 {
    comps | (kind << 8) | ((normalized as u32) << 16)
}

/// Mint real SPIR-V (all entry points) from a WGSL seed via naga — the guest SPIR-V ABI round trip.
fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed wgsl validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spir-v")
}

fn glsl_words(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: entry.into(),
        source: source.into(),
    }
    .to_words()
}

fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}

// =================================================================================================

// (4) GRAPHICS — real vertex buffers: multiple attributes, indexed, per-instance step mode
// =================================================================================================

const SEED_POS2_COLOR: &str = r#"
    struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
    @vertex fn vs_main(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VOut {
        return VOut(vec4<f32>(p, 0.0, 1.0), c);
    }
    @fragment fn fs_main(v: VOut) -> @location(0) vec4<f32> { return v.color; }
"#;

const SEED_POS2_GREEN: &str = r#"
    @vertex fn vs_main(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
        return vec4<f32>(p, 0.0, 1.0);
    }
    @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
"#;

const SEED_VINDEX_GREEN: &str = r#"
    @vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
        var p = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
        return vec4<f32>(p[vi], 0.0, 1.0);
    }
    @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
"#;

const SEED_POS3_COLOR: &str = r#"
    struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
    @vertex fn vs_main(@location(0) p: vec3<f32>, @location(1) c: vec4<f32>) -> VOut {
        return VOut(vec4<f32>(p, 1.0), c);
    }
    @fragment fn fs_main(v: VOut) -> @location(0) vec4<f32> { return v.color; }
"#;

fn all_texels_eq(px: &[u8], expect: [u8; 4]) {
    for (i, t) in px.chunks_exact(4).enumerate() {
        assert_eq!(t, expect, "pixel {i} mismatch");
    }
}

#[path = "executor_coverage/capability.rs"]
mod capability;
#[path = "executor_coverage/compute.rs"]
mod compute;
#[path = "executor_coverage/depth.rs"]
mod depth;
#[path = "executor_coverage/glsl.rs"]
mod glsl;
#[path = "executor_coverage/raster.rs"]
mod raster;
#[path = "executor_coverage/readback.rs"]
mod readback;
#[path = "executor_coverage/transfer.rs"]
mod transfer;
#[path = "executor_coverage/validation.rs"]
mod validation;
#[path = "executor_coverage/vertex.rs"]
mod vertex;
