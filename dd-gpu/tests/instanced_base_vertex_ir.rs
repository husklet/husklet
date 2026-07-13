//! Instanced + base-vertex draws are a single-IR contract both the guest shim (dd-shim-gl) and the
//! host executors must agree on. These tests pin (1) that `Enc::Draw`/`Enc::DrawIndexed` survive a wire
//! encode/decode round-trip with non-default `instance_count` / `first_instance` / `base_vertex`, and
//! (2) that the software oracle actually consumes `base_vertex` when fetching indexed vertices — the
//! two properties that were silently collapsing instanced GLES draws to a single instance / offset 0.

use dd_gpu::backend::GpuBackend;
use dd_gpu::id::*;
use dd_gpu::ir::*;
use dd_gpu::software::SoftwareBackend;

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc { size, usage, label: String::new() }
}

/// The instance/base-vertex fields round-trip byte-exactly through `encode_stream`/`decode_stream`.
/// A regression in the wire layout (without a WIRE_VERSION bump) would surface here as a mismatch.
#[test]
fn draw_instance_and_base_vertex_fields_survive_wire_roundtrip() {
    let draws = vec![
        Enc::Draw { vertex_count: 6, instance_count: 4, first_vertex: 2, first_instance: 1 },
        Enc::DrawIndexed { index_count: 9, instance_count: 3, first_index: 3, base_vertex: -5, first_instance: 7 },
    ];
    let cmds = vec![Cmd::Submit(CommandBuffer { encoder: draws.clone(), signal: None })];
    let bytes = encode_stream(&cmds);
    let back = decode_stream(&bytes).expect("decode");
    let Cmd::Submit(cb) = &back[0] else { panic!("expected Submit") };
    assert_eq!(cb.encoder, draws, "instance_count/first_instance/base_vertex must survive the wire");
}

/// Drive an indexed draw through the public backend API twice — base_vertex 0 vs 3 — over a vertex
/// buffer whose two vertex triples carry different colors. base_vertex must select the second triple.
#[test]
fn software_base_vertex_selects_the_offset_vertices() {
    // 6 vertices ([x,y, r,g,b,a] f32, stride 24): triple 0..3 = color A, triple 3..6 = color B, each a
    // full-screen NDC triangle covering the whole 1×1 target.
    let tri = |c: [f32; 4]| -> [[f32; 6]; 3] {
        [
            [-1.0, -1.0, c[0], c[1], c[2], c[3]],
            [3.0, -1.0, c[0], c[1], c[2], c[3]],
            [-1.0, 3.0, c[0], c[1], c[2], c[3]],
        ]
    };
    let a = [51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0, 1.0];
    let b = [204.0 / 255.0, 153.0 / 255.0, 102.0 / 255.0, 1.0];
    let mut vbytes = Vec::new();
    for v in tri(a).iter().chain(tri(b).iter()) {
        for f in v {
            vbytes.extend_from_slice(&f.to_le_bytes());
        }
    }

    let mut be = SoftwareBackend::new();
    be.create_shader(ShaderId(1), ShaderPayloadKind::DemoBuiltin, &[1, 2, 3]).unwrap();
    be.create_render_pipeline(PipelineId(1), &RenderPipelineDesc {
        vertex: ShaderRef { module: 1, entry: "vs".into() },
        fragment: Some(ShaderRef { module: 1, entry: "fs".into() }),
        vertex_buffers: vec![VertexLayout {
            stride: 24,
            step_mode: 0,
            attrs: vec![
                VertexAttr { location: 0, format: 0, offset: 0 },
                VertexAttr { location: 1, format: 0, offset: 8 },
            ],
        }],
        // No blend → opaque replace, so the fetched vertex color is written straight through.
        color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Srgb, blend: None, write_mask: 0xF }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        label: String::new(),
    }).unwrap();
    be.create_texture(TextureId(1), &TextureDesc {
        width: 1, height: 1, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2,
        format: TextureFormat::Rgba8Srgb, usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC, label: String::new(),
    }).unwrap();
    be.create_buffer(BufferId(1), &buf(vbytes.len() as u64, buffer_usage::VERTEX | buffer_usage::COPY_DST)).unwrap();
    be.write_buffer(BufferId(1), 0, &vbytes).unwrap();
    let ibytes: Vec<u8> = [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
    be.create_buffer(BufferId(2), &buf(ibytes.len() as u64, buffer_usage::INDEX | buffer_usage::COPY_DST)).unwrap();
    be.write_buffer(BufferId(2), 0, &ibytes).unwrap();

    let mut draw = |base_vertex: i32| -> [u8; 4] {
        be.submit(&CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                Enc::SetIndexBuffer { buffer: 2, offset: 0, format: IndexFormat::U16 },
                Enc::DrawIndexed { index_count: 3, instance_count: 1, first_index: 0, base_vertex, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }).unwrap();
        let mut out = [0u8; 4];
        be.read_texture(TextureId(1), &mut out).unwrap();
        out
    };

    assert_eq!(draw(0), [51, 102, 153, 255], "base_vertex 0 → color A (verts 0..3)");
    assert_eq!(draw(3), [204, 153, 102, 255], "base_vertex 3 → color B (verts 3..6)");
}
