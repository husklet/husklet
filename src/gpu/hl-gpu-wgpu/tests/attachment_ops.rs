//! Render-pass ATTACHMENT load/store coverage: `ColorAttachment.store` (honored, not silently forced to
//! Store), and every `LoadOp` on a color attachment (Clear / Load / DontCare).
//!
//! The coverage audit found `store` was hardcoded to `wgpu::StoreOp::Store` in `submit.rs` — the wire field
//! was dropped, the same class of bug the #209 audit found for write_mask/cull/front_face. It is now honored
//! (`true` → Store, `false` → Discard). These tests pin the observable contract:
//!   * store=true keeps the drawn pixels for a later readback (the regression guard);
//!   * LoadOp::Load preserves a prior pass's content in the un-drawn region;
//!   * LoadOp::DontCare is mapped conservatively to Load (the executor's documented choice), so it likewise
//!     preserves rather than corrupts.
//!
//! Skips with no adapter.

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, ColorTargetState, RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, TextureDim, TextureFormat, Topology};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 4;
const H: u32 = 4;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

fn fs_const(rgba: [f32; 4]) -> String {
    format!(
        "#version 460\nlayout(location=0) out vec4 o;\nvoid main() {{ o = vec4({:?},{:?},{:?},{:?}); }}\n",
        rgba[0], rgba[1], rgba[2], rgba[3]
    )
}

fn glsl(stage: u32, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: "main".to_string(),
        source: source.to_string(),
    }
    .to_words()
}

fn tex() -> TextureDesc {
    TextureDesc {
        width: W,
        height: H,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}

fn pipe(id: u32, fs: u32) -> Cmd {
    Cmd::CreateRenderPipeline(
        id,
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: 1,
                entry: "main".into(),
            },
            fragment: Some(ShaderRef {
                module: fs,
                entry: "main".into(),
            }),
            vertex_buffers: vec![],
            color_targets: vec![ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: 0xF,
            }],
            depth: None,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            sample_count: 1,
            label: String::new(),
        },
    )
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

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= 2)
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

/// Common shader/pipeline preamble (VS + a red FS + a green FS + two pipelines).
fn preamble() -> Vec<Cmd> {
    vec![
        Cmd::CreateTexture(1, tex()),
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::VERTEX, VS),
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, &fs_const([1.0, 0.0, 0.0, 1.0])),
        },
        Cmd::CreateShader {
            id: 3,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, &fs_const([0.0, 1.0, 0.0, 1.0])),
        },
        pipe(1, 2), // red
        pipe(2, 3), // green
    ]
}

#[test]
fn store_true_keeps_drawn_pixels_and_store_false_does_not_panic() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // store = true: the drawn red survives to the readback (Store honored as Store).
    let mut s = session(&exec);
    let mut cmds = preamble();
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(1),
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &cmds).expect("store=true pass must run");
    let out = exec.read_texture(&s.resources, 1).unwrap();
    assert!(
        near(px(&out, W / 2, H / 2), RED),
        "store=true must keep the drawn red pixel, got {:?}",
        px(&out, W / 2, H / 2)
    );

    // store = false (StoreOp::Discard): the guest declared it won't read the target, so the CONTENTS are
    // undefined after the pass (not asserted). The contract we CAN assert is that the field flows to wgpu
    // and the pass still completes without a device error / panic.
    let mut s2 = session(&exec);
    let mut cmds2 = preamble();
    cmds2.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: false,
                }],
                depth: None,
            },
            Enc::SetPipeline(1),
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hl_gpu::runtime::submit(&mut s2, &mut exec, 0, &cmds2)
        .expect("store=false (Discard) must still run to completion without a device error");
}

#[test]
fn load_op_load_and_dontcare_preserve_prior_content() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // For both Load and DontCare (which the executor maps to Load): pass 1 clears the whole target RED and
    // stores; pass 2 loads and draws GREEN clipped by scissor to the LEFT half. The right half must retain
    // the loaded red — proving the second pass did NOT clear.
    for second_load in [LoadOp::Load, LoadOp::DontCare] {
        let mut s = session(&exec);
        let mut cmds = preamble();
        cmds.push(Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [1.0, 0.0, 0.0, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass, // clear-only pass — the whole target is red
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: second_load,
                        clear: [0.0; 4],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::SetScissor {
                    x: 0,
                    y: 0,
                    w: W / 2,
                    h: H,
                }, // restrict the green draw to the left half
                Enc::SetPipeline(2),
                Enc::Draw {
                    vertex_count: 3,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }));
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &cmds).expect("two-pass load must run");
        let out = exec.read_texture(&s.resources, 1).unwrap();
        assert!(
            near(px(&out, 0, H / 2), GREEN),
            "{second_load:?}: left half must be the freshly drawn green, got {:?}",
            px(&out, 0, H / 2)
        );
        assert!(
            near(px(&out, W - 1, H / 2), RED),
            "{second_load:?}: right (un-drawn) half must retain the loaded red — the load op must not clear, got {:?}",
            px(&out, W - 1, H / 2)
        );
    }
}
