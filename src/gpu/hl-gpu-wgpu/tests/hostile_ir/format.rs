use super::*;

// =================================================================================================
// (4) MISMATCHED formats
// =================================================================================================

#[test]
fn render_pipeline_attachment_format_mismatch_is_invalid() {
    let Some(mut g) = exec() else { return };
    let vertex = "#version 460\nvoid main(){ gl_Position = vec4(0.0,0.0,0.0,1.0); }\n";
    let fragment = "#version 460\nlayout(location=0) out vec4 c; void main(){ c = vec4(1.0); }\n";
    hostile(
        &mut g,
        "render_attachment_format",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", vertex),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", fragment),
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
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Bgra8Unorm,
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
        is_invalid,
    );
}

#[test]
fn copy_texture_to_texture_between_incompatible_formats_converts_not_rejects() {
    let Some(mut g) = exec() else { return };
    // R8 (1 byte/texel) → Rgba8 (4 bytes/texel): DIFFERENT texel layouts. GL permits this as a CONVERTING
    // copy (the red channel expands to (R,0,0,1)); the executor now routes a format mismatch through a
    // converting blit instead of rejecting it (previously `Invalid("… incompatible formats")`). Prove it
    // SUCCEEDS and leaves the executor healthy — the exact-conversion pixel checks live in `t2t_convert.rs`.
    let mut s = session(&g);
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
        hl_gpu::runtime::submit(
            &mut s,
            &mut *g,
            0,
            &[
                Cmd::CreateTexture(1, tex(4, 4, TextureFormat::R8Unorm, RT)),
                Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyTextureToTexture {
                        src: 1,
                        src_sub: sub(),
                        src_origin: Origin3d::default(),
                        dst: 2,
                        dst_sub: sub(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    }],
                    signal: None,
                }),
            ],
        )
    }));
    match r {
        Err(_) => panic!("[c2t2t_convert] converting copy PANICKED"),
        Ok(Err(e)) => panic!("[c2t2t_convert] converting copy must succeed, got {e:?}"),
        Ok(Ok(_)) => {}
    }
    drop(s);
    assert_survives(&mut g, "c2t2t_convert");
}

#[test]
fn resolve_non_multisampled_source_is_invalid() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "resolve_non_msaa",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)), // single-sampled src
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ResolveTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
        is_invalid,
    );
}

#[test]
fn resolve_format_mismatch_is_invalid() {
    let Some(mut g) = exec() else { return };
    // Multisampled src, single-sample dst, but different formats.
    let msaa = TextureDesc {
        sample_count: 4,
        ..tex(
            4,
            4,
            TextureFormat::Rgba8Unorm,
            texture_usage::RENDER_TARGET,
        )
    };
    hostile(
        &mut g,
        "resolve_fmt_mismatch",
        &[
            Cmd::CreateTexture(1, msaa),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Bgra8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ResolveTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
        is_invalid,
    );
}
