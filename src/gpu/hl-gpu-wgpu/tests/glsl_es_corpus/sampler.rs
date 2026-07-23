use super::*;

#[test]
fn es_combined_sampler_fragment_samples_exact_texel() {
    let mut guard = match exec() {
        Some(g) => g,
        None => return,
    };
    let exec = &mut *guard;
    // A GLSL-ES `uniform sampler2D` sampled at center -- exercises the combined->separate split END TO END:
    // the split lands the texture at binding 1 and the sampler at binding 2 (glsl_es scheme 1+2k / 2+2k).
    let texel: [u8; 4] = [40, 160, 210, 255];
    let fs = r#"#version 300 es
precision highp float;
uniform sampler2D uTex;
layout(location = 0) out vec4 o;
void main() { o = texture(uTex, vec2(0.5, 0.5)); }
"#;
    let mut s = new_sess(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, rt(W, H)),
            Cmd::CreateTexture(
                2,
                TextureDesc {
                    width: 1,
                    height: 1,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: texel.to_vec(),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", DRAW_VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
            },
            Cmd::CreateSampler(
                1,
                SamplerDesc {
                    min_filter: Filter::Nearest,
                    mag_filter: Filter::Nearest,
                    mip_filter: Filter::Nearest,
                    address_u: AddressMode::ClampToEdge,
                    address_v: AddressMode::ClampToEdge,
                    address_w: AddressMode::ClampToEdge,
                },
            ),
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
                    vertex_buffers: vec![],
                    color_targets: vec![ct()],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            // The ES combined-sampler split: texture -> binding 1, sampler -> binding 2.
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Sampler { id: 1 },
                        },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 2,
                        mip: 0,
                        width: 1,
                        height: 1,
                    },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [1.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
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
    .expect("ES combined-sampler draw must run cleanly");
    let px = exec.read_texture(&s.resources, 1).unwrap();
    for chunk in px.chunks_exact(4) {
        let p = [chunk[0], chunk[1], chunk[2], chunk[3]];
        assert!(
            approx(p, texel, 1),
            "ES combined-sampler fragment must sample {texel:?} end-to-end, got {p:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------------
// mat2-in-std140-UBO: an EXACT-PIXEL geometry proof that the reconstructed matrix multiplies the REAL
// std140 bytes correctly (not just that it compiles). A unit quad synthesized from gl_VertexID is
// transformed by a `mat2` in a std140 uniform block; the covered pixels must land EXACTLY where the mat2
// maps the quad. A wrong std140 column offset (the padding bug the `vec4 col[N]` rewrite exists to avoid) or
// a transposed/collapsed reconstruction would move or reshape the block and fail the exact check.
// ---------------------------------------------------------------------------------------------------

// std140 stores each 2-row matrix column in its own 16-byte (vec4) slot — only `.xy` carries data. This is
// EXACTLY the byte image `split_std140_mat2`'s `vec4 col[N]` rewrite reads back, so the uploaded bytes are
// the app's real UBO contents with no re-pack.
