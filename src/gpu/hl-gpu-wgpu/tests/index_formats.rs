//! DEMO 6 — index formats: the SAME indexed geometry drawn with a u16 AND a u32 index buffer must produce
//! byte-for-byte identical exact pixels.
//!
//! A centered quad is drawn as two triangles from a 4-vertex position buffer indexed by `[0,1,2, 2,1,3]`.
//! The run is repeated with the index buffer typed `IndexFormat::U16` and `IndexFormat::U32` (the index
//! DATA widened accordingly). Both readbacks must (a) equal each other exactly and (b) cover the exact
//! framebuffer rectangle the NDC quad maps to — a backend that mis-set the index element size would fetch
//! garbage indices and rasterize a wrong/empty shape.

mod common;
use common::*;

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, ColorAttachment, RenderPipelineDesc, ShaderRef, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, IndexFormat, LoadOp, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 32;
const H: u32 = 32;
const FILL: [u8; 4] = [40, 200, 90, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 255];
const VFMT_F32X2: u32 = 2;

// The centered quad [-0.5, 0.5]² → framebuffer x,y ∈ [8, 24). Covered pixel centers land in (8, 24).
const QUAD: [f32; 8] = [-0.5, -0.5, 0.5, -0.5, -0.5, 0.5, 0.5, 0.5];
const INDICES: [u16; 6] = [0, 1, 2, 2, 1, 3];

const VS: &str = r#"#version 460
layout(location = 0) in vec2 pos;
void main() { gl_Position = vec4(pos, 0.0, 1.0); }
"#;
const FS: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(40.0/255.0, 200.0/255.0, 90.0/255.0, 1.0); }
"#;

/// Draw the indexed quad once with index buffer bytes `idx` typed `fmt`; return the RGBA8 readback.
fn run(exec: &mut WgpuExecutor, fmt: IndexFormat, idx: Vec<u8>) -> Vec<u8> {
    let mut s = new_session(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 32,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&QUAD),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: idx.len() as u64,
                    usage: buffer_usage::INDEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: idx,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 8,
                        step_mode: 0,
                        attrs: vec![VertexAttr {
                            location: 0,
                            format: VFMT_F32X2,
                            offset: 0,
                        }],
                    }],
                    color_targets: vec![color_target()],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
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
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 1,
                        offset: 0,
                    },
                    Enc::SetIndexBuffer {
                        buffer: 2,
                        offset: 0,
                        format: fmt,
                    },
                    Enc::DrawIndexed {
                        index_count: 6,
                        instance_count: 1,
                        first_index: 0,
                        base_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the indexed quad draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

#[test]
fn u16_and_u32_indices_produce_identical_pixels() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    let u16_bytes: Vec<u8> = INDICES.iter().flat_map(|v| v.to_le_bytes()).collect();
    let u32_bytes: Vec<u8> = INDICES
        .iter()
        .flat_map(|v| (*v as u32).to_le_bytes())
        .collect();

    let a = run(&mut exec, IndexFormat::U16, u16_bytes);
    let b = run(&mut exec, IndexFormat::U32, u32_bytes);
    write_png("index_u16", W, H, &a);
    write_png("index_u32", W, H, &b);

    // (a) The two index widths must render the SAME image, byte for byte.
    assert_eq!(
        a, b,
        "u16 and u32 index buffers must produce identical pixels"
    );

    // (b) The image must be the exact centered-quad coverage: interior FILL, exterior CLEAR, right count.
    let mut filled = 0usize;
    for py in 0..H {
        for pxi in 0..W {
            let (cx, cy) = (pxi as f32 + 0.5, py as f32 + 0.5);
            let inside = cx > 8.0 && cx < 24.0 && cy > 8.0 && cy < 24.0;
            let got = px(&a, W, pxi, py);
            let want = if inside { FILL } else { CLEAR };
            assert!(
                near(got, want),
                "px ({pxi},{py}): {got:?} != {want:?} (inside={inside})"
            );
            if inside {
                filled += 1;
            }
        }
    }
    assert_eq!(
        filled,
        16 * 16,
        "the centered quad must cover exactly a 16×16 block"
    );
    eprintln!("demo `index_formats`: u16==u32, exact 16×16 quad — PNGs at {OUT_DIR}/index_u16.png, index_u32.png");
}
