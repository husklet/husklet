//! Fixed-function BLEND correctness for the wgpu executor.
//!
//! One question, answered with exact pixels: does this executor honor the protocol's per-target
//! [`BlendState`] end to end? Before this test the pipeline builder hardcoded `blend: None`, so a
//! translucent draw over an opaque one OVERWROTE instead of compositing — `glBlendFunc`/`GL_BLEND` (GL) and
//! alpha-blend graphics pipelines (Vulkan) were silently dropped.
//!
//! The demo draws an OPAQUE background quad (blend disabled → replace), then a centered 50%-alpha
//! foreground quad over it, and reads back. It runs the SAME IR twice, differing only in the foreground
//! pipeline's protocol blend field:
//!   * blend = src-alpha-over  → the overlap pixel must equal the EXACT straight-alpha composite
//!     `bg*(1-a) + fg*a`, while the border (background only) stays the background color.
//!   * blend = None            → the overlap pixel must equal the foreground color exactly (an opaque
//!     overwrite), proving the field — not some always-on blend — is what changed the result.
//!
//! The target is a LINEAR `Rgba8Unorm` (no sRGB gamma), so the hardware blend is performed directly on the
//! normalized values and the composite is an exact 8-bit integer: with `a = 0.5` each channel is the plain
//! average of foreground and background. If NO adapter is reachable the test skips, like the rest of the
//! suite.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat, Topology};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 32;
const H: u32 = 32;

// Opaque background and translucent foreground. Channels chosen so the 50% average is an exact integer.
const BG: [u8; 3] = [200, 100, 40];
const FG: [u8; 3] = [40, 180, 220];
const FG_ALPHA: f32 = 0.5;

// GL/protocol blend-factor + op wire numbering (`hl_wip-gl` `blend_factor_wire`/`blend_op_wire`):
const SRC_ALPHA: u32 = 4;
const ONE_MINUS_SRC_ALPHA: u32 = 5;
const OP_ADD: u32 = 0;

/// Straight-alpha "source over": `src * srcAlpha + dst * (1 - srcAlpha)` on all four channels — exactly
/// what `glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA)` and a Vulkan alpha-blend pipeline request.
fn src_alpha_over() -> BlendState {
    BlendState {
        src_color: SRC_ALPHA,
        dst_color: ONE_MINUS_SRC_ALPHA,
        op_color: OP_ADD,
        src_alpha: SRC_ALPHA,
        dst_alpha: ONE_MINUS_SRC_ALPHA,
        op_alpha: OP_ADD,
    }
}

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor { stage, entry: entry.to_string(), source: source.to_string() }.to_words()
}

fn le_f32(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn tex(usage: u32) -> TextureDesc {
    TextureDesc {
        width: W,
        height: H,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn new_session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)))
}

// A quad from `gl_VertexIndex` (triangle strip) spanning the NDC rect in the set-0 uniform's `rect`
// (x0,y0,x1,y1); the fragment paints the uniform's straight (non-premultiplied) `color` including alpha.
const VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 rect; vec4 color; } u;
void main() {
    float x = ((gl_VertexIndex & 1) == 1) ? u.rect.z : u.rect.x;
    float y = ((gl_VertexIndex >> 1) == 1) ? u.rect.w : u.rect.y;
    gl_Position = vec4(x, y, 0.0, 1.0);
}
"#;

const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 rect; vec4 color; } u;
layout(location = 0) out vec4 o;
void main() { o = u.color; }
"#;

/// `[x0,y0,x1,y1, r,g,b,a]` std140 uniform payload for one quad draw.
fn quad_uniform(rect: [f32; 4], color: [f32; 4]) -> Vec<u8> {
    le_f32(&[rect[0], rect[1], rect[2], rect[3], color[0], color[1], color[2], color[3]])
}

/// Draw an opaque full-screen background, then a centered foreground quad whose pipeline carries `fg_blend`
/// (Some ⇒ fixed-function blend, None ⇒ opaque overwrite). Returns the tight RGBA8 readback.
fn run(exec: &mut WgpuExecutor, fg_blend: Option<BlendState>) -> Vec<u8> {
    let mut s = new_session(exec);

    let f = |c: [u8; 3], a: f32| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a];
    let opaque_target = ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF };
    let fg_target = ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: fg_blend, write_mask: 0xF };

    let render_pipe = |id: u32, target: ColorTargetState| {
        Cmd::CreateRenderPipeline(
            id,
            RenderPipelineDesc {
                vertex: ShaderRef { module: 1, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                vertex_buffers: vec![],
                color_targets: vec![target],
                depth: None,
                topology: Topology::TriangleStrip,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        )
    };

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, tex(texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
            // set 0 binding 0: background quad (full screen, opaque).
            Cmd::CreateBuffer(1, BufferDesc { size: 32, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: quad_uniform([-1.0, -1.0, 1.0, 1.0], f(BG, 1.0)) },
            // set 0 binding 0: foreground quad (centered, 50% alpha).
            Cmd::CreateBuffer(2, BufferDesc { size: 32, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 2, offset: 0, data: quad_uniform([-0.5, -0.5, 0.5, 0.5], f(FG, FG_ALPHA)) },
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", VS) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS) },
            render_pipe(1, opaque_target), // background pipeline: opaque replace
            render_pipe(2, fg_target),     // foreground pipeline: blend under test
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc { set: 0, entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 32 } }] },
            ),
            Cmd::CreateBindGroup(
                2,
                BindGroupDesc { set: 0, entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 2, offset: 0, size: 32 } }] },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw { vertex_count: 4, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::SetPipeline(2),
                    Enc::SetBindGroup { index: 0, group: 2 },
                    Enc::Draw { vertex_count: 4, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the background+foreground blend draw must run cleanly");

    exec.read_texture(&s.resources, 1).unwrap()
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// `|a-b| <= 1` per channel — absorbs only last-ULP unorm rounding; the composite here is an exact integer.
fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= 1)
}

#[test]
fn src_alpha_over_composites_exactly_and_disabled_overwrites() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return, // no reachable adapter — skip, like the rest of the suite
    };

    // Center pixel is inside the foreground quad (the overlap); a corner is background-only.
    let (cx, cy) = (W / 2, H / 2);
    let (bx, by) = (1, 1);

    // The EXACT straight-alpha composite at the overlap for a=0.5: bg*(1-a) + fg*a = (bg+fg)/2 per channel.
    let avg = |b: u8, g: u8| ((b as u16 + g as u16) / 2) as u8;
    let composite = [
        avg(BG[0], FG[0]),
        avg(BG[1], FG[1]),
        avg(BG[2], FG[2]),
        // alpha: fg.a*fg.a + bg.a*(1-fg.a) = 0.5*0.5 + 1.0*0.5 = 0.75 → round(0.75*255) = 191
        191,
    ];
    let bg_opaque = [BG[0], BG[1], BG[2], 255];
    let fg_opaque = [FG[0], FG[1], FG[2], 128]; // overwrite alpha = round(0.5*255); ±1 tolerance covers 127/128

    // --- blend ENABLED: the overlap composites, the border stays background --------------------------
    let blended = run(&mut exec, Some(src_alpha_over()));
    let overlap = px(&blended, cx, cy);
    let border = px(&blended, bx, by);

    assert!(
        near(overlap, composite),
        "blend enabled: overlap pixel must be the exact alpha composite {composite:?}, got {overlap:?}"
    );
    assert!(
        near(border, bg_opaque),
        "blend enabled: background-only border must stay {bg_opaque:?}, got {border:?}"
    );
    // Prove the blend actually COMPOSITED — the overlap is neither a pure overwrite nor a no-op.
    assert!(
        !near(overlap, [FG[0], FG[1], FG[2], overlap[3]]) && !near(overlap, bg_opaque),
        "blend enabled: overlap {overlap:?} must differ from both pure foreground and pure background"
    );

    // --- blend DISABLED (same IR, blend: None): the foreground OVERWRITES ----------------------------
    let overwritten = run(&mut exec, None);
    let overlap_off = px(&overwritten, cx, cy);
    let border_off = px(&overwritten, bx, by);

    assert!(
        near(overlap_off, fg_opaque),
        "blend disabled: overlap pixel must be the opaque foreground {fg_opaque:?}, got {overlap_off:?}"
    );
    assert!(
        near(border_off, bg_opaque),
        "blend disabled: background-only border must stay {bg_opaque:?}, got {border_off:?}"
    );
    // The two runs differ ONLY in the protocol blend field, yet the overlap pixel differs — end-to-end proof.
    assert!(
        !near(overlap, overlap_off),
        "the blend field must change the overlap pixel: blended {overlap:?} vs overwritten {overlap_off:?}"
    );
}
