#[cfg(target_os = "macos")]
mod macos {
    use dd_display::metal::MetalCtx;
    use dd_display::metal_backend::MetalBackend;
    use dd_gpu::ir::{
        buffer_usage, BlendState, BufferDesc, Cmd, ColorAttachment, ColorTargetState,
        CommandBuffer, Enc, LoadOp, RenderPipelineDesc, ShaderRef, TextureFormat, Topology,
        VertexAttr, VertexLayout,
    };

    const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VIn {
    float2 pos [[attribute(0)]];
    float4 color [[attribute(1)]];
};

struct VOut {
    float4 pos [[position]];
    float4 color;
};

vertex VOut vmain(VIn in [[stage_in]]) {
    VOut out;
    out.pos = float4(in.pos, 0.0, 1.0);
    out.color = in.color;
    return out;
}

fragment float4 fmain(VOut in [[stage_in]]) {
    return in.color;
}
"#;

    fn pack_msl(src: &str) -> Vec<u32> {
        let mut words = vec![src.len() as u32];
        for chunk in src.as_bytes().chunks(4) {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(word));
        }
        words
    }

    fn quad_vertices_rgba(color: [f32; 4]) -> Vec<u8> {
        let verts: [[f32; 6]; 6] = [
            [-1.0, -1.0, color[0], color[1], color[2], color[3]],
            [1.0, -1.0, color[0], color[1], color[2], color[3]],
            [-1.0, 1.0, color[0], color[1], color[2], color[3]],
            [1.0, -1.0, color[0], color[1], color[2], color[3]],
            [1.0, 1.0, color[0], color[1], color[2], color[3]],
            [-1.0, 1.0, color[0], color[1], color[2], color[3]],
        ];
        let mut out = Vec::with_capacity(verts.len() * 6 * std::mem::size_of::<f32>());
        for vertex in verts {
            for value in vertex {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        out
    }

    fn render_blended_quad(clear: [f32; 4], src: [f32; 4], write_mask: u32) -> [u8; 4] {
        let ctx = MetalCtx::new().expect("Metal device is required for blend tests");
        let target = ctx.new_bgra_texture(8, 8);
        let vertices = quad_vertices_rgba(src);

        let mut backend = MetalBackend::new(&ctx);
        backend.set_render_target(1, target.clone());

        let cmds = vec![
            Cmd::CreateBuffer(
                10,
                BufferDesc {
                    size: vertices.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: "blend-quad-vbo".into(),
                },
            ),
            Cmd::WriteBuffer {
                id: 10,
                offset: 0,
                data: vertices,
            },
            Cmd::CreateShader {
                id: 20,
                spirv: pack_msl(MSL),
            },
            Cmd::CreateRenderPipeline(
                30,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 20,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 20,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 24,
                        step_mode: 0,
                        attrs: vec![
                            VertexAttr {
                                location: 0,
                                format: 2,
                                offset: 0,
                            },
                            VertexAttr {
                                location: 1,
                                format: 4,
                                offset: 8,
                            },
                        ],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Bgra8Unorm,
                        blend: Some(BlendState {
                            src_color: 1,
                            dst_color: 5,
                            op_color: 0,
                            src_alpha: 1,
                            dst_alpha: 5,
                            op_alpha: 0,
                        }),
                        write_mask,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: "premultiplied-alpha-blend".into(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear,
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 10,
                        offset: 0,
                    },
                    Enc::Draw {
                        vertex_count: 6,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];

        dd_gpu::replay::replay(&mut backend, &cmds).expect("Metal replay failed");

        let bgra = ctx.readback_bgra(&target, 8, 8);
        let i = (4 * 8 + 4) * 4;
        [bgra[i + 2], bgra[i + 1], bgra[i], bgra[i + 3]]
    }

    fn assert_pixel_near(actual: [u8; 4], expected: [u8; 4]) {
        for (channel, (a, e)) in ["r", "g", "b", "a"].iter().zip(actual.iter().zip(expected)) {
            let delta = (*a as i16 - e as i16).abs();
            assert!(
                delta <= 2,
                "channel {channel}: expected {expected:?}, got {actual:?}, delta {delta}"
            );
        }
    }

    #[test]
    fn premultiplied_alpha_blends_color_and_alpha() {
        let pixel = render_blended_quad([0.4, 0.2, 0.8, 0.25], [0.2, 0.1, 0.0, 0.5], 0xf);

        assert_pixel_near(pixel, [102, 51, 102, 159]);
    }

    #[test]
    fn rgb_write_mask_preserves_destination_alpha() {
        let pixel = render_blended_quad([0.4, 0.2, 0.8, 0.25], [0.2, 0.1, 0.0, 0.5], 0x7);

        assert_pixel_near(pixel, [102, 51, 102, 64]);
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn metal_blend_tests_require_macos() {
    eprintln!("Metal blend tests are macOS-only");
}
