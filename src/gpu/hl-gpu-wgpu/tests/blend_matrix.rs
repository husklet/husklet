//! EXHAUSTIVE fixed-function blend coverage: every protocol blend FACTOR (0..=10) and every blend OP
//! (0..=4) run on the real `WgpuExecutor` with a hand-computed expected composite.
//!
//! `tests/blend.rs` proves ONE canonical blend (src-alpha-over) end to end; this file closes the gap the
//! coverage audit found — the `pipeline::blend_factor` / `pipeline::blend_operation` maps have an arm per
//! wire code, but only SRC_ALPHA / ONE_MINUS_SRC_ALPHA / ADD were ever exercised. Here each factor and each
//! op is driven through a real draw and the read-back pixel is checked against the exact wgpu blend
//! arithmetic (`src*srcFac  OP  dst*dstFac`, MIN/MAX ignoring the factors), so a wrong or dropped mapping
//! shows up as a wrong pixel. The CPU oracle models blend as a hardcoded source-over (it only checks
//! `blend.is_some()`), so this coverage CANNOT come from the differential harness — it must assert directly.
//!
//! Method: clear the target to a known `dst` (the LoadOp::Clear sets all four channels exactly), then draw
//! one full-screen triangle whose fragment emits a known `src`, with the blend state under test. The target
//! is linear `Rgba8Unorm`, so the hardware blend runs on the normalized values with no gamma and the
//! composite is an exact unorm8 (±2 absorbs last-ULP rounding).

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment,
    ColorTargetState, RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 4;
const H: u32 = 4;

// Blend-factor wire codes (the neutral GL-driver `blend_factor_wire` numbering the executor decodes).
const ZERO: u32 = 0;
const ONE: u32 = 1;
const SRC_COLOR: u32 = 2;
const ONE_MINUS_SRC_COLOR: u32 = 3;
const SRC_ALPHA: u32 = 4;
const ONE_MINUS_SRC_ALPHA: u32 = 5;
const DST_COLOR: u32 = 6;
const ONE_MINUS_DST_COLOR: u32 = 7;
const DST_ALPHA: u32 = 8;
const ONE_MINUS_DST_ALPHA: u32 = 9;
const SRC_ALPHA_SATURATE: u32 = 10;
const CONSTANT: u32 = 11;
const ONE_MINUS_CONSTANT: u32 = 12;
const BLEND_CONSTANT: [f32; 4] = [0.25, 0.4, 0.65, 0.75];

// Blend-op wire codes (`blend_op_wire`).
const OP_ADD: u32 = 0;
const OP_SUBTRACT: u32 = 1;
const OP_REVERSE_SUBTRACT: u32 = 2;
const OP_MIN: u32 = 3;
const OP_MAX: u32 = 4;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 color; } u;
layout(location = 0) out vec4 o;
void main() { o = u.color; }
"#;

fn glsl(stage: u32, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: "main".to_string(),
        source: source.to_string(),
    }
    .to_words()
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

fn session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

/// Draw `src` (fragment output) over a cleared `dst` with `blend`, return the center pixel.
fn run(exec: &mut WgpuExecutor, src: [f32; 4], dst: [f32; 4], blend: BlendState) -> [u8; 4] {
    let mut s = session(exec);
    let target = ColorTargetState {
        format: TextureFormat::Rgba8Unorm,
        blend: Some(blend),
        write_mask: 0xF,
    };
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src.iter().flat_map(|f| f.to_le_bytes()).collect(),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, FS),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "main".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![target],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 16,
                        },
                    }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: dst.map(f64::from),
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBlendConstant {
                        color: BLEND_CONSTANT,
                    },
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the blend draw must run cleanly");
    let px = exec.read_texture(&s.resources, 1).unwrap();
    let c = ((H / 2 * W + W / 2) * 4) as usize;
    [px[c], px[c + 1], px[c + 2], px[c + 3]]
}

/// The blend factor value for wire `code` at channel `ch` (0=r,1=g,2=b,3=a) — the exact wgpu semantics.
fn factor(code: u32, ch: usize, src: [f32; 4], dst: [f32; 4]) -> f32 {
    match code {
        ZERO => 0.0,
        ONE => 1.0,
        SRC_COLOR => src[ch],
        ONE_MINUS_SRC_COLOR => 1.0 - src[ch],
        SRC_ALPHA => src[3],
        ONE_MINUS_SRC_ALPHA => 1.0 - src[3],
        DST_COLOR => dst[ch],
        ONE_MINUS_DST_COLOR => 1.0 - dst[ch],
        DST_ALPHA => dst[3],
        ONE_MINUS_DST_ALPHA => 1.0 - dst[3],
        SRC_ALPHA_SATURATE => {
            if ch == 3 {
                1.0
            } else {
                src[3].min(1.0 - dst[3])
            }
        }
        CONSTANT => BLEND_CONSTANT[ch],
        ONE_MINUS_CONSTANT => 1.0 - BLEND_CONSTANT[ch],
        _ => 1.0,
    }
}

fn apply_op(op: u32, a: f32, b: f32, raw_src: f32, raw_dst: f32) -> f32 {
    let v = match op {
        OP_ADD => a + b,
        OP_SUBTRACT => a - b,
        OP_REVERSE_SUBTRACT => b - a,
        OP_MIN => raw_src.min(raw_dst), // MIN/MAX ignore the factors (Vulkan/WebGPU semantics)
        OP_MAX => raw_src.max(raw_dst),
        _ => a + b,
    };
    v.clamp(0.0, 1.0)
}

fn u8c(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// The exact expected composite for `blend`, replicating wgpu's per-component blend.
fn expected(src: [f32; 4], dst: [f32; 4], b: &BlendState) -> [u8; 4] {
    let mut out = [0u8; 4];
    for ch in 0..3 {
        let sf = factor(b.src_color, ch, src, dst);
        let df = factor(b.dst_color, ch, src, dst);
        out[ch] = u8c(apply_op(
            b.op_color,
            src[ch] * sf,
            dst[ch] * df,
            src[ch],
            dst[ch],
        ));
    }
    let sf = factor(b.src_alpha, 3, src, dst);
    let df = factor(b.dst_alpha, 3, src, dst);
    out[3] = u8c(apply_op(
        b.op_alpha,
        src[3] * sf,
        dst[3] * df,
        src[3],
        dst[3],
    ));
    out
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= 2)
}

#[test]
fn every_blend_factor_and_op_composites_exactly() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    // Values chosen so the factors spread the result across the byte range (no two factors collapse to the
    // same pixel for these inputs), making a wrong mapping observable.
    let src = [0.8_f32, 0.6, 0.4, 0.5];
    let dst = [0.2_f32, 0.3, 0.7, 0.8];

    // --- every blend FACTOR, exercised once as the SRC color factor and once as the DST color factor
    // (op = ADD, alpha kept as src.a via ONE/ZERO so it can't mask a color error) ---------------------
    let factors = [
        ZERO,
        ONE,
        SRC_COLOR,
        ONE_MINUS_SRC_COLOR,
        SRC_ALPHA,
        ONE_MINUS_SRC_ALPHA,
        DST_COLOR,
        ONE_MINUS_DST_COLOR,
        DST_ALPHA,
        ONE_MINUS_DST_ALPHA,
        SRC_ALPHA_SATURATE,
        CONSTANT,
        ONE_MINUS_CONSTANT,
    ];
    for &f in &factors {
        for as_src in [true, false] {
            let (sc, dc) = if as_src { (f, ONE) } else { (ONE, f) };
            let b = BlendState {
                src_color: sc,
                dst_color: dc,
                op_color: OP_ADD,
                src_alpha: ONE,
                dst_alpha: ZERO,
                op_alpha: OP_ADD,
            };
            let got = run(&mut exec, src, dst, b.clone());
            let want = expected(src, dst, &b);
            assert!(
                near(got, want),
                "blend factor code {f} as {} factor: expected {want:?}, got {got:?}",
                if as_src { "src" } else { "dst" }
            );
        }
    }

    // --- every blend OP, exercised on the color component (factors fixed to ONE/ONE so ADD/SUB/REVSUB
    // differ, and MIN/MAX ignore them) --------------------------------------------------------------
    for &op in &[OP_ADD, OP_SUBTRACT, OP_REVERSE_SUBTRACT, OP_MIN, OP_MAX] {
        let b = BlendState {
            src_color: ONE,
            dst_color: ONE,
            op_color: op,
            src_alpha: ONE,
            dst_alpha: ONE,
            op_alpha: op,
        };
        let got = run(&mut exec, src, dst, b.clone());
        let want = expected(src, dst, &b);
        assert!(
            near(got, want),
            "blend op code {op}: expected {want:?}, got {got:?}"
        );
    }

    // A cross-check that the harness DISTINGUISHES ops: ADD and MIN of these inputs must differ, so a
    // stub that ignored `op_color` could not pass both above.
    let add = run(
        &mut exec,
        src,
        dst,
        BlendState {
            src_color: ONE,
            dst_color: ONE,
            op_color: OP_ADD,
            src_alpha: ONE,
            dst_alpha: ZERO,
            op_alpha: OP_ADD,
        },
    );
    let min = run(
        &mut exec,
        src,
        dst,
        BlendState {
            src_color: ONE,
            dst_color: ONE,
            op_color: OP_MIN,
            src_alpha: ONE,
            dst_alpha: ZERO,
            op_alpha: OP_ADD,
        },
    );
    assert!(
        !near(add, min),
        "ADD {add:?} and MIN {min:?} must differ — proves op_color is honored"
    );
}
