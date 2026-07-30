//! The Zed GPUI instanced-quad unblock: an instanced draw with NO vertex buffer whose per-instance
//! geometry is read from a VERTEX-stage storage buffer.
//!
//! GPUI (Zed's renderer) draws every quad as `Draw(vertex_count=4, instance_count=N)` with no vertex
//! buffer — the unit quad is synthesized from `@builtin(vertex_index)` and each instance's rectangle
//! (origin + size) is read from a `var<storage, read>` buffer bound at SET 1, indexed by
//! `@builtin(instance_index)`. If the per-instance storage data never reaches the VERTEX shader (the
//! storage binding not given VERTEX visibility, the wrong `BindingType`, the bind group not built/bound,
//! or `instance_count` not passed to `pass.draw`), every quad collapses to zero area and the target stays
//! blank — exactly the (0,0,0,0) swapchain Zed produced.
//!
//! This reproduces that draw against the real device: a vertex shader builds a quad from
//! `gl_VertexIndex`, reads a set-0 uniform (a viewport scale, identity here) AND a set-1 read-only storage
//! buffer of per-instance rectangles, and draws N instances at DISTINCT positions. FAIL-before: a
//! collapsed/blank target (no white pixels, zero luminance spread). PASS-after: N white quads at their
//! instance offsets — real geometry the storage-fed vertex shader could only draw if the per-instance data
//! reached it.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, ShaderRef, TextureDesc,
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

const W: u32 = 16;
const H: u32 = 16;

// Vertex: NO vertex buffer. The unit quad is synthesized from gl_VertexIndex (a 4-vertex triangle strip),
// and EACH instance's rectangle (center.xy, half-extent.zw, in NDC) is read from the SET-1 read-only
// storage buffer at gl_InstanceIndex — the exact GPUI instanced-quad shape. The set-0 uniform (an
// identity viewport scale) is read too, so the vertex stage genuinely reads BOTH a uniform (set 0) and a
// storage buffer (set 1), like GPUI's globals-uniform + instance-storage pair.
const VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform Globals { vec2 scale; } g;
layout(std430, set = 1, binding = 0) readonly buffer Quads { vec4 quads[]; };
void main() {
    vec4 q = quads[gl_InstanceIndex];
    vec2 corner = vec2(float(gl_VertexIndex & 1), float((gl_VertexIndex >> 1) & 1)) * 2.0 - 1.0;
    vec2 pos = (q.xy + corner * q.zw) * g.scale;
    gl_Position = vec4(pos, 0.0, 1.0);
}
"#;

// Fragment: constant white — the geometry (not the shading) is what proves the per-instance data landed.
const FS: &str = r#"#version 460
layout(location = 0) out vec4 color;
void main() { color = vec4(1.0, 1.0, 1.0, 1.0); }
"#;

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: entry.to_string(),
        source: source.to_string(),
    }
    .to_words()
}

fn tex(w: u32, h: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn le_f32(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|f| f.to_le_bytes()).collect()
}

#[test]
fn instanced_draw_reads_per_instance_geometry_from_a_vertex_storage_buffer() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    // Two instances at DISTINCT NDC positions: quad 0 in the lower-left, quad 1 in the upper-right, each a
    // half-extent of 0.25 (a ~4px block on the 16px target). If the storage read collapses to zeros, both
    // quads become a zero-area point at the origin and nothing rasterizes.
    let quads: [f32; 8] = [
        -0.5, -0.5, 0.25, 0.25, // instance 0: center (-0.5,-0.5)
        0.5, 0.5, 0.25, 0.25, // instance 1: center (0.5,0.5)
    ];
    let scale: [f32; 2] = [1.0, 1.0]; // identity viewport scale

    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            // set-0 uniform: the identity viewport scale.
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 8,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&scale),
            },
            // set-1 storage: the per-instance rectangles (read-only in the vertex shader).
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 32,
                    usage: buffer_usage::STORAGE,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: le_f32(&quads),
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
                    vertex_buffers: vec![], // NO vertex buffer — the GPUI instanced-quad case
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: None,
                    topology: Topology::TriangleStrip,
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
                            size: 8,
                        },
                    }],
                },
            ),
            Cmd::CreateBindGroup(
                2,
                BindGroupDesc {
                    set: 1,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 2,
                            offset: 0,
                            size: 32,
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
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 }, // set 0 → the uniform
                    Enc::SetBindGroup { index: 1, group: 2 }, // set 1 → the per-instance storage
                    // NO vertex/index buffer: 4 verts (unit quad from vertex_index) × 2 instances.
                    Enc::Draw {
                        vertex_count: 4,
                        instance_count: 2,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the instanced storage-fed draw must run cleanly");

    let px = exec.read_texture(&s.resources, 1).unwrap();

    // Count bright (rasterized) pixels and track luminance spread + the bounding regions the two quads land
    // in. A collapsed draw (storage never reached the vertex shader) leaves every pixel the black clear →
    // zero white pixels, zero spread. Real instanced geometry paints two separated white blocks.
    let mut white = 0usize;
    let mut min_lum = 255i32;
    let mut max_lum = 0i32;
    let mut left_half = false; // a white pixel with x < W/2
    let mut right_half = false; // a white pixel with x >= W/2
    for (i, p) in px.chunks_exact(4).enumerate() {
        let x = (i as u32) % W;
        let lum = p[0] as i32; // white quads → 255, black clear → 0
        min_lum = min_lum.min(lum);
        max_lum = max_lum.max(lum);
        if p[0] > 200 && p[1] > 200 && p[2] > 200 {
            white += 1;
            if x < W / 2 {
                left_half = true;
            } else {
                right_half = true;
            }
        }
    }

    let spread = max_lum - min_lum;
    assert!(
        white > 0,
        "instanced storage-fed draw produced NO rasterized pixels — the per-instance geometry never \
         reached the vertex shader (the quads collapsed to zero area); white={white} spread={spread}"
    );
    assert!(
        spread > 40,
        "luminance spread {spread} too low — the target is effectively blank; the storage-fed instanced \
         draw did not paint distinct geometry"
    );
    assert!(
        left_half && right_half,
        "both instances must paint at their DISTINCT storage-provided offsets (left_half={left_half}, \
         right_half={right_half}) — if only one region is painted the instance_index→storage lookup did \
         not vary per instance"
    );
}
