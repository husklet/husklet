//! Depth-attachment execution in the CPU rasterizer oracle. Two overlapping triangles are drawn at
//! different depths through the real runtime pipeline (validate → account → dispatch → execute); the
//! observable color readback proves the per-fragment depth test (`DepthState.compare`) + depth write
//! (`DepthState.depth_write`) actually gate the color. A depth-disabled control renders the same geometry
//! in painter's order for contrast.

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, ColorAttachment, ColorTargetState, DepthAttachment, DepthState, RenderPipelineDesc,
    ShaderRef, TextureDesc, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{Inst, KernelProgram, KERNEL_MAGIC};
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind, TextureId};

/// The oracle is a fixed-function rasterizer: the vertex/fragment stages are ignored, but a render
/// pipeline still needs its vertex shader module to exist. The oracle only accepts KERNEL shader
/// payloads, so we register a trivial no-op kernel as the placeholder module (mirrors the compute
/// conformance tests' `define_kernel` pattern).
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

// -------------------------------------------------------------------------------------------------
// harness (mirrors tests/conformance.rs)
// -------------------------------------------------------------------------------------------------

fn run_batch(exec: &mut hl_gpu::CpuExecutor, cmds: &[Cmd]) -> hl_gpu::Session {
    let caps = exec.capabilities();
    let mut limits = hl_gpu::Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = hl_gpu::Session::new(
        limits,
        hl_gpu::GlobalLedger::unbounded(),
        Box::new(hl_gpu::FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds).expect("depth program must run cleanly");
    s
}

/// One vertex in the 3D-position + color layout (stride 28): `x,y,z` then `r,g,b,a`.
fn vtx(x: f32, y: f32, z: f32, c: [f32; 4]) -> [u8; 28] {
    let mut b = [0u8; 28];
    b[0..4].copy_from_slice(&x.to_le_bytes());
    b[4..8].copy_from_slice(&y.to_le_bytes());
    b[8..12].copy_from_slice(&z.to_le_bytes());
    b[12..16].copy_from_slice(&c[0].to_le_bytes());
    b[16..20].copy_from_slice(&c[1].to_le_bytes());
    b[20..24].copy_from_slice(&c[2].to_le_bytes());
    b[24..28].copy_from_slice(&c[3].to_le_bytes());
    b
}

const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN_PX: [u8; 4] = [0, 255, 0, 255];
const RED_PX: [u8; 4] = [255, 0, 0, 255];

/// Six vertices for a 2×1 target (pixel0 at NDC (-0.5,0), pixel1 at NDC (0.5,0)):
///  - verts 0..3: NEAR green triangle (z=0.3) covering ONLY pixel0.
///  - verts 3..6: FAR  red   triangle (z=0.7) covering BOTH pixels (fullscreen).
fn vertex_bytes() -> Vec<u8> {
    let mut v = Vec::new();
    // near green, z=0.3 — triangle (-2,-2),(-2,2),(0,0): contains pixel0, excludes pixel1.
    for p in [(-2.0, -2.0), (-2.0, 2.0), (0.0, 0.0)] {
        v.extend_from_slice(&vtx(p.0, p.1, 0.3, GREEN));
    }
    // far red, z=0.7 — fullscreen triangle (-1,-1),(3,-1),(-1,3): covers both pixels.
    for p in [(-1.0, -1.0), (3.0, -1.0), (-1.0, 3.0)] {
        v.extend_from_slice(&vtx(p.0, p.1, 0.7, RED));
    }
    v
}

/// Build + run the scene, returning the two 4-byte target pixels. `depth` selects whether the pipeline
/// declares a depth-stencil state and the render pass binds a depth attachment.
fn render(depth: bool) -> [[u8; 4]; 2] {
    let color_fmt = TextureFormat::Rgba8Unorm;
    let color_tex = tex(2, 1, color_fmt, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC);
    let depth_tex_desc = tex(2, 1, TextureFormat::Depth32Float, texture_usage::RENDER_TARGET);

    let pipeline = RenderPipelineDesc {
        vertex: ShaderRef { module: 1, entry: "vs".into() },
        fragment: None,
        vertex_buffers: vec![VertexLayout {
            stride: 28,
            step_mode: 0,
            attrs: vec![
                VertexAttr { location: 0, format: 0, offset: 0 },
                VertexAttr { location: 1, format: 0, offset: 12 },
            ],
        }],
        color_targets: vec![ColorTargetState { format: color_fmt, blend: None, write_mask: 0xF }],
        depth: depth.then(|| DepthState {
            format: TextureFormat::Depth32Float,
            depth_write: true,
            depth_compare: compare::LESS,
        }),
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        label: String::new(),
    };

    let color_att = ColorAttachment {
        texture: 1,
        load: LoadOp::Clear,
        clear: [0.0, 0.0, 0.0, 1.0],
        store: true,
    };
    let depth_att = depth.then_some(DepthAttachment { texture: 2, load: LoadOp::Clear, clear_depth: 1.0 });

    let mut cmds = vec![
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
        Cmd::CreateRenderPipeline(1, pipeline),
        Cmd::CreateTexture(1, color_tex),
    ];
    if depth {
        cmds.push(Cmd::CreateTexture(2, depth_tex_desc));
    }
    cmds.push(Cmd::CreateBuffer(1, buf(vertex_bytes().len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)));
    cmds.push(Cmd::WriteBuffer { id: 1, offset: 0, data: vertex_bytes() });
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass { color: vec![color_att], depth: depth_att },
            Enc::SetPipeline(1),
            Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
            // near green first, then far red — draw order is deliberately the OPPOSITE of depth order so
            // the depth test and painter's order disagree at the overlap (pixel0).
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 3, first_instance: 0 },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));

    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(1, placeholder_shader());
    let s = run_batch(&mut exec, &cmds);
    let mut px = [0u8; 8];
    exec.read_texture(&s.resources, TextureId(1), &mut px).unwrap();
    [px[0..4].try_into().unwrap(), px[4..8].try_into().unwrap()]
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc { size, usage, label: String::new() }
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

// -------------------------------------------------------------------------------------------------
// tests
// -------------------------------------------------------------------------------------------------

#[test]
fn depth_test_less_lets_nearer_triangle_win_the_overlap() {
    let [pixel0, pixel1] = render(true);
    // pixel0 (both triangles cover it): the NEAR green wins even though the far red is drawn LATER — the
    // depth test (0.7 < 0.3 is false) rejects the far fragment. This is the whole point: not painter's.
    assert_eq!(pixel0, GREEN_PX, "overlap: nearer (green) must survive the later far draw");
    // pixel1 (only the FAR red covers it): the far fragment passes the test against the cleared depth
    // (0.7 < 1.0) and is written — the farther triangle wins where only it covers.
    assert_eq!(pixel1, RED_PX, "far-only region: farther (red) must be written");
}

#[test]
fn depth_disabled_control_is_painters_order() {
    let [pixel0, pixel1] = render(false);
    // Same geometry + draw order, but no depth: the later far red overwrites the overlap, so BOTH pixels
    // are red. Contrast with the depth case, where pixel0 is green — the depth state is what flips it.
    assert_eq!(pixel0, RED_PX, "no depth: later draw (red) wins the overlap (painter's)");
    assert_eq!(pixel1, RED_PX, "no depth: red covers pixel1");
}

#[test]
fn depth_buffer_stores_written_depth() {
    // After the pass, the depth plane must hold each fragment's written z (LESS + depth_write): pixel0
    // holds the near 0.3 (green won), pixel1 holds the far 0.7 (only red covered it).
    let color_fmt = TextureFormat::Rgba8Unorm;
    let mut cmds = vec![
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
        Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef { module: 1, entry: "vs".into() },
                fragment: None,
                vertex_buffers: vec![VertexLayout {
                    stride: 28,
                    step_mode: 0,
                    attrs: vec![VertexAttr { location: 0, format: 0, offset: 0 }],
                }],
                color_targets: vec![ColorTargetState { format: color_fmt, blend: None, write_mask: 0xF }],
                depth: Some(DepthState {
                    format: TextureFormat::Depth32Float,
                    depth_write: true,
                    depth_compare: compare::LESS,
                }),
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                label: String::new(),
            },
        ),
        Cmd::CreateTexture(1, tex(2, 1, color_fmt, texture_usage::RENDER_TARGET)),
        Cmd::CreateTexture(2, tex(2, 1, TextureFormat::Depth32Float, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
        Cmd::CreateBuffer(1, buf(vertex_bytes().len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vertex_bytes() },
    ];
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0; 4], store: true }],
                depth: Some(DepthAttachment { texture: 2, load: LoadOp::Clear, clear_depth: 1.0 }),
            },
            Enc::SetPipeline(1),
            Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 3, first_instance: 0 },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));

    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(1, placeholder_shader());
    let s = run_batch(&mut exec, &cmds);
    let mut raw = [0u8; 8]; // 2 texels × f32
    exec.read_texture(&s.resources, TextureId(2), &mut raw).unwrap();
    let z0 = f32::from_le_bytes(raw[0..4].try_into().unwrap());
    let z1 = f32::from_le_bytes(raw[4..8].try_into().unwrap());
    assert!((z0 - 0.3).abs() < 1e-6, "pixel0 depth should be the near 0.3, got {z0}");
    assert!((z1 - 0.7).abs() < 1e-6, "pixel1 depth should be the far 0.7, got {z1}");
}
