//! SPEC-CORRECTNESS oracle battery: every value asserted here is HAND-COMPUTED from the WebGPU/Vulkan
//! semantics the CPU reference oracle claims to implement — NOT captured from the executor. A silent
//! oracle no-op or a wrong-but-plausible result (a dropped write-mask, a flipped cull, a blend that
//! doesn't blend, a copy that copies the wrong region) is a bug that would validate wrong behavior across
//! the whole differential fuzzer, so each op is pinned to an independently-derived expected value.
//!
//! Fills the gaps `conformance.rs`/`depth.rs`/`stencil.rs` leave: sRGB clear gamma-encode, gradient
//! (barycentric) draw, premultiplied source-over blend, per-channel write-mask, face cull × front-face,
//! CopyBufferToTexture / CopyTextureToTexture region copies, nearest + linear blit resample, and the
//! multisample-resolve path.

use hl_gpu::protocol::model::descriptor::{
    BlendState, BufferDesc, ColorAttachment, ColorTargetState, Extent3d, Origin3d,
    RenderPipelineDesc, ShaderRef, TextureDesc, TextureSubresource, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{Inst, KernelProgram, KERNEL_MAGIC};
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind, TextureId};

// -------------------------------------------------------------------------------------------------
// harness (mirrors conformance.rs / depth.rs; widens the session caps to whatever the batch needs)
// -------------------------------------------------------------------------------------------------

fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}
fn placeholder_shader() -> KernelProgram {
    KernelProgram {
        entry: "vs".into(),
        block: [1, 1, 1],
        params: vec![],
        param_bytes: 0,
        num_regions: 0,
        shared_bytes: 0,
        reg_count: 1,
        insts: vec![Inst::Ret],
    }
}

fn run(cmds: &[Cmd]) -> (hl_gpu::CpuExecutor, hl_gpu::Session) {
    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(1, placeholder_shader());
    let caps = exec.capabilities();
    let mut limits = hl_gpu::Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = hl_gpu::Session::new(
        limits,
        hl_gpu::GlobalLedger::unbounded(),
        Box::new(hl_gpu::FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, cmds).expect("oracle program must run cleanly");
    (exec, s)
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
fn tex_ms(w: u32, h: u32, samples: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: samples,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}
fn readback(exec: &hl_gpu::CpuExecutor, s: &hl_gpu::Session, id: u32, n: usize) -> Vec<u8> {
    let mut px = vec![0u8; n];
    exec.read_texture(&s.resources, TextureId(id), &mut px)
        .unwrap();
    px
}

/// A stride-24 vertex (2D pos at 0/4, RGBA color at 8..24) — the oracle's `stride >= 24` layout.
fn vtx24(x: f32, y: f32, c: [f32; 4]) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..4].copy_from_slice(&x.to_le_bytes());
    b[4..8].copy_from_slice(&y.to_le_bytes());
    b[8..12].copy_from_slice(&c[0].to_le_bytes());
    b[12..16].copy_from_slice(&c[1].to_le_bytes());
    b[16..20].copy_from_slice(&c[2].to_le_bytes());
    b[20..24].copy_from_slice(&c[3].to_le_bytes());
    b
}
/// A stride-12 vertex (3D pos only) — the oracle defaults its color to opaque white here.
fn vtx12(x: f32, y: f32) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&x.to_le_bytes());
    b[4..8].copy_from_slice(&y.to_le_bytes());
    // z at 8..12 stays 0.
    b
}

// =================================================================================================
// 1. sRGB clear must GAMMA-ENCODE (linear 0.5 -> 188), not naively quantize to 128.
// =================================================================================================

fn draw_pipeline(
    id: u32,
    blend: Option<BlendState>,
    write_mask: u32,
    cull: u32,
    front_face: u32,
    stride: u32,
    color_off: u32,
) -> Cmd {
    let mut attrs = vec![VertexAttr {
        location: 0,
        format: 0,
        offset: 0,
    }];
    if stride >= 24 {
        attrs.push(VertexAttr {
            location: 1,
            format: 0,
            offset: color_off,
        });
    }
    Cmd::CreateRenderPipeline(
        id,
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: 1,
                entry: "vs".into(),
            },
            fragment: None,
            vertex_buffers: vec![VertexLayout {
                stride,
                step_mode: 0,
                attrs,
            }],
            color_targets: vec![ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend,
                write_mask,
            }],
            depth: None,
            topology: Topology::TriangleList,
            cull,
            front_face,
            sample_count: 1,
            label: String::new(),
        },
    )
}

#[path = "oracle_spec/blit.rs"]
mod blit;
#[path = "oracle_spec/color.rs"]
mod color;
#[path = "oracle_spec/copy.rs"]
mod copy;
#[path = "oracle_spec/draw.rs"]
mod draw;
#[path = "oracle_spec/rect.rs"]
mod rect;
