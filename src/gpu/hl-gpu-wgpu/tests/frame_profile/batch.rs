//! The glmark2-shaped frame batches `frame_profile` measures: the shared setup, the two resource
//! [`Shape`]s, and the exact pixels each shape must produce.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    ShaderRef, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, LoadOp, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};

use crate::gpu_harness::{color_target, glsl, le_f32, tex2d};

pub(crate) const DRAWS: usize = 600;
pub(crate) const GRID: u32 = 25; // 25 × 24 = 600 cells
pub(crate) const CELL: u32 = 4;
pub(crate) const W: u32 = GRID * CELL; // 100
pub(crate) const H: u32 = (DRAWS as u32 / GRID) * CELL; // 96

const VFMT_F32X2: u32 = 2;

/// Quad vertices come from one shared VBO; the per-draw color arrives through the bind group's UBO, so the
/// bind group is genuinely load-bearing (a shape that skipped it would not measure bind-group cost).
const VS: &str = r#"#version 460
layout(location = 0) in vec2 pos;
layout(std140, binding = 0) uniform U { vec4 tint; };
layout(location = 0) flat out vec4 vcol;
void main() { gl_Position = vec4(pos, 0.0, 1.0); vcol = tint; }
"#;
const FS: &str = r#"#version 460
layout(location = 0) flat in vec4 vcol;
layout(location = 0) out vec4 o;
void main() { o = vcol; }
"#;

/// Deterministic per-draw tint bytes, well separated so a mis-placed draw is visible in readback.
fn tint(i: usize) -> [u8; 4] {
    [
        (30 + (i % 20) * 11) as u8,
        (30 + ((i / 20) % 10) * 20) as u8,
        (40 + (i % 7) * 30) as u8,
        255,
    ]
}

/// The shared vertex buffer: one screen-aligned quad per draw, tiling the target exactly.
fn quad_bytes() -> Vec<u8> {
    let mut verts: Vec<f32> = Vec::with_capacity(DRAWS * 4 * 2);
    for i in 0..DRAWS {
        let gx = (i as u32) % GRID;
        let gy = (i as u32) / GRID;
        let ndc_x = |p: u32| p as f32 / W as f32 * 2.0 - 1.0;
        let ndc_y = |p: u32| 1.0 - p as f32 / H as f32 * 2.0;
        let (x0, x1) = (ndc_x(gx * CELL), ndc_x((gx + 1) * CELL));
        let (yt, yb) = (ndc_y(gy * CELL), ndc_y((gy + 1) * CELL));
        for &(x, y) in &[(x0, yt), (x1, yt), (x0, yb), (x1, yb)] {
            verts.extend_from_slice(&[x, y]);
        }
    }
    verts.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// std140 bytes for draw `i`'s `vec4 tint`.
fn tint_bytes(i: usize) -> Vec<u8> {
    let c = tint(i);
    le_f32(&[
        f32::from(c[0]) / 255.0,
        f32::from(c[1]) / 255.0,
        f32::from(c[2]) / 255.0,
        1.0,
    ])
}

/// Resource ids used by the shared setup batch.
pub(crate) const TARGET: u32 = 1;
const VBO: u32 = 1;
const PIPELINE: u32 = 1;
/// First id handed to per-frame uniform buffers / bind groups (above every setup id).
const FRAME_ID_BASE: u32 = 16;

pub(crate) fn setup_batch() -> Vec<Cmd> {
    let vbytes = quad_bytes();
    vec![
        Cmd::CreateTexture(
            TARGET,
            tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
        ),
        Cmd::CreateBuffer(
            VBO,
            BufferDesc {
                size: vbytes.len() as u64,
                usage: buffer_usage::VERTEX,
                label: String::new(),
            },
        ),
        Cmd::WriteBuffer {
            id: VBO,
            offset: 0,
            data: vbytes,
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
            PIPELINE,
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
                topology: Topology::TriangleStrip,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        ),
    ]
}

/// Which resource shape a frame batch uses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// A fresh uniform buffer + bind group per draw — what `hl-gl` emits today.
    PerDraw,
    /// One uniform buffer + one bind group for the whole frame.
    Shared,
}

/// One frame's commands: the per-draw resources this shape needs, one Submit with the render pass, and the
/// destroys that return the frame's resources (so residency is steady across frames, as a real driver's is).
pub(crate) fn frame_batch(shape: Shape) -> Vec<Cmd> {
    // Per draw: PerDraw needs CreateBuffer + WriteBuffer + CreateBindGroup + 2 destroys; Shared needs 3
    // commands total. Reserve the exact bound.
    let per_draw = usize::from(shape == Shape::PerDraw);
    let mut cmds = Vec::with_capacity(1 + DRAWS * 5 * per_draw + 5);
    let mut ops: Vec<Enc> = Vec::with_capacity(2 + DRAWS * 3);
    ops.push(Enc::SetPipeline(PIPELINE));
    ops.push(Enc::SetVertexBuffer {
        slot: 0,
        buffer: VBO,
        offset: 0,
    });
    match shape {
        Shape::PerDraw => {
            for i in 0..DRAWS {
                let id = FRAME_ID_BASE + i as u32;
                cmds.push(Cmd::CreateBuffer(
                    id,
                    BufferDesc {
                        size: 16,
                        usage: buffer_usage::UNIFORM,
                        label: String::new(),
                    },
                ));
                cmds.push(Cmd::WriteBuffer {
                    id,
                    offset: 0,
                    data: tint_bytes(i),
                });
                cmds.push(Cmd::CreateBindGroup(
                    id,
                    BindGroupDesc {
                        set: 0,
                        entries: vec![BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id,
                                offset: 0,
                                size: 16,
                            },
                        }],
                    },
                ));
                ops.push(Enc::SetBindGroup {
                    index: 0,
                    group: id,
                });
                push_draw(&mut ops, i);
            }
        }
        Shape::Shared => {
            // A single 16-byte UBO cannot carry 600 distinct tints, so the shared shape writes the tint
            // ONCE and every draw reads it. Correctness is still checked — against this shape's own
            // expected pixels (see `expected`), so the two shapes are each verified, not cross-verified.
            let id = FRAME_ID_BASE;
            cmds.push(Cmd::CreateBuffer(
                id,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ));
            cmds.push(Cmd::WriteBuffer {
                id,
                offset: 0,
                data: tint_bytes(0),
            });
            cmds.push(Cmd::CreateBindGroup(
                id,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id,
                            offset: 0,
                            size: 16,
                        },
                    }],
                },
            ));
            ops.push(Enc::SetBindGroup {
                index: 0,
                group: id,
            });
            for i in 0..DRAWS {
                push_draw(&mut ops, i);
            }
        }
    }
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: TARGET,
                load: LoadOp::Clear,
                clear: [0.0, 0.0, 0.0, 1.0],
                store: true,
            }],
            depth: None,
        }]
        .into_iter()
        .chain(ops)
        .chain(std::iter::once(Enc::EndRenderPass))
        .collect(),
        signal: None,
    }));
    let live = if shape == Shape::PerDraw { DRAWS } else { 1 };
    for i in 0..live {
        let id = FRAME_ID_BASE + i as u32;
        cmds.push(Cmd::DestroyBindGroup(id));
        cmds.push(Cmd::DestroyBuffer(id));
    }
    cmds
}

fn push_draw(ops: &mut Vec<Enc>, i: usize) {
    ops.push(Enc::Draw {
        vertex_count: 4,
        instance_count: 1,
        first_vertex: (i * 4) as u32,
        first_instance: 0,
    });
}

/// The tint every pixel of cell `i` must carry for `shape`.
pub(crate) fn expected(shape: Shape, i: usize) -> [u8; 4] {
    match shape {
        Shape::PerDraw => tint(i),
        Shape::Shared => tint(0),
    }
}
