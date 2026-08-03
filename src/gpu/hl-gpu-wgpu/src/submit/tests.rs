mod native_texture_copy {
    use hl_gpu::protocol::model::descriptor::{BufferDesc, TextureDesc};
    use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, TextureDim, TextureFormat};
    use hl_gpu::{Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session};

    use crate::{DeviceConfig, WgpuExecutor};

    fn executor() -> (WgpuExecutor, Session) {
        let exec = WgpuExecutor::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove native texture copies");
        let mut limits = Limits::from_capabilities(exec.capabilities());
        limits.copy_alignment = 1;
        let session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        (exec, session)
    }

    fn texture(format: TextureFormat, width: u32, height: u32, mip_levels: u32) -> TextureDesc {
        TextureDesc {
            width,
            height,
            depth: 1,
            mip_levels,
            sample_count: 1,
            dim: TextureDim::D2,
            format,
            usage: texture_usage::COPY_SRC | texture_usage::COPY_DST,
            label: String::new(),
        }
    }

    fn buffer(size: u64) -> BufferDesc {
        BufferDesc {
            size,
            usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            label: String::new(),
        }
    }

    fn seed(
        exec: &mut WgpuExecutor,
        session: &mut Session,
        format: TextureFormat,
        base: (u32, u32),
        mip: u32,
        extent: (u32, u32),
        pixels: Vec<u8>,
    ) {
        hl_gpu::runtime::submit(
            session,
            exec,
            0,
            &[
                Cmd::CreateTexture(1, texture(format, base.0, base.1, mip + 1)),
                Cmd::CreateBuffer(1, buffer(pixels.len() as u64)),
                Cmd::WriteBuffer {
                    id: 1,
                    offset: 0,
                    data: pixels,
                },
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: extent.0 * format.bytes_per_texel().unwrap() as u32,
                        dst: 1,
                        mip,
                        width: extent.0,
                        height: extent.1,
                    }],
                    signal: None,
                }),
            ],
        )
        .map(|_| ())
        .expect("seed texture");
    }

    struct CopySpec {
        mip: u32,
        width: u32,
        height: u32,
        dst_offset: u64,
        bytes_per_row: u32,
        dst_size: u64,
    }

    fn copy(exec: &mut WgpuExecutor, session: &mut Session, spec: CopySpec) {
        hl_gpu::runtime::submit(
            session,
            exec,
            0,
            &[
                Cmd::CreateBuffer(2, buffer(spec.dst_size)),
                Cmd::WriteBuffer {
                    id: 2,
                    offset: 0,
                    data: vec![0xA5; spec.dst_size as usize],
                },
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyTextureToBuffer {
                        src: 1,
                        mip: spec.mip,
                        width: spec.width,
                        height: spec.height,
                        dst: 2,
                        dst_offset: spec.dst_offset,
                        bytes_per_row: spec.bytes_per_row,
                    }],
                    signal: None,
                }),
            ],
        )
        .expect("copy texture to buffer");
    }

    fn destination(exec: &mut WgpuExecutor, session: &mut Session, size: u64, fill: u8) {
        hl_gpu::runtime::submit(
            session,
            exec,
            0,
            &[
                Cmd::CreateBuffer(2, buffer(size)),
                Cmd::WriteBuffer {
                    id: 2,
                    offset: 0,
                    data: vec![fill; size as usize],
                },
            ],
        )
        .expect("create destination");
    }

    fn submit_copy(
        exec: &mut WgpuExecutor,
        session: &mut Session,
        spec: &CopySpec,
    ) -> hl_gpu::Result<()> {
        hl_gpu::runtime::submit(
            session,
            exec,
            0,
            &[Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToBuffer {
                    src: 1,
                    mip: spec.mip,
                    width: spec.width,
                    height: spec.height,
                    dst: 2,
                    dst_offset: spec.dst_offset,
                    bytes_per_row: spec.bytes_per_row,
                }],
                signal: None,
            })],
        )
        .map(|_| ())
    }

    #[test]
    fn rgba_odd_width_copy_is_one_submit_and_waits_only_when_read() {
        let (mut exec, mut session) = executor();
        let (width, height) = (65, 3);
        let pixels: Vec<u8> = (0u32..width * height * 4)
            .map(|byte| byte.wrapping_mul(17) as u8)
            .collect();
        seed(
            &mut exec,
            &mut session,
            TextureFormat::Rgba8Unorm,
            (width, height),
            0,
            (width, height),
            pixels.clone(),
        );
        let waits = exec.completion_wait_count();
        let submits = exec.native_copy_submission_count();
        let stride = width * 4 + 4;
        let offset = 12;
        let size = offset + u64::from((height - 1) * stride + width * 4) + 8;
        copy(
            &mut exec,
            &mut session,
            CopySpec {
                mip: 0,
                width,
                height,
                dst_offset: offset,
                bytes_per_row: stride,
                dst_size: size,
            },
        );

        assert_eq!(exec.completion_wait_count(), waits);
        assert_eq!(exec.native_copy_submission_count(), submits + 1);
        let output = exec
            .read_bytes(&session.resources, 2, 0, size as usize)
            .unwrap();
        assert_eq!(exec.completion_wait_count(), waits + 1);
        for row in 0..height as usize {
            let start = offset as usize + row * stride as usize;
            assert_eq!(
                &output[start..start + (width * 4) as usize],
                &pixels[row * (width * 4) as usize..(row + 1) * (width * 4) as usize]
            );
            if row + 1 < height as usize {
                assert_eq!(
                    &output[start + (width * 4) as usize..start + stride as usize],
                    &[0xA5; 4]
                );
            }
        }
    }

    #[test]
    fn bgra_mip_copy_preserves_bytes_and_submission_count_is_height_independent() {
        let (mut exec, mut session) = executor();
        let mip_extent = (5, 17);
        let pixels: Vec<u8> = (0..mip_extent.0 * mip_extent.1 * 4)
            .map(|byte| (byte ^ 0x5A) as u8)
            .collect();
        seed(
            &mut exec,
            &mut session,
            TextureFormat::Bgra8Unorm,
            (10, 34),
            1,
            mip_extent,
            pixels.clone(),
        );
        let submits = exec.native_copy_submission_count();
        copy(
            &mut exec,
            &mut session,
            CopySpec {
                mip: 1,
                width: mip_extent.0,
                height: mip_extent.1,
                dst_offset: 0,
                bytes_per_row: 0,
                dst_size: pixels.len() as u64,
            },
        );
        assert_eq!(exec.native_copy_submission_count(), submits + 1);
        assert_eq!(
            exec.read_bytes(&session.resources, 2, 0, pixels.len())
                .unwrap(),
            pixels
        );

        hl_gpu::runtime::submit(
            &mut session,
            &mut exec,
            0,
            &[Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToBuffer {
                    src: 1,
                    mip: 1,
                    width: mip_extent.0,
                    height: 1,
                    dst: 2,
                    dst_offset: 0,
                    bytes_per_row: 0,
                }],
                signal: None,
            })],
        )
        .unwrap();
        assert_eq!(
            exec.native_copy_submission_count(),
            submits + 2,
            "one-row and seventeen-row copies each enqueue exactly one command buffer"
        );
        assert_eq!(
            exec.direct_copy_submission_count(),
            1,
            "a one-row copy is encoded directly into the destination"
        );
    }

    #[test]
    fn unaligned_r8_layout_keeps_exact_fallback() {
        let (mut exec, mut session) = executor();
        let pixels = vec![1, 2, 3, 4, 5, 6];
        seed(
            &mut exec,
            &mut session,
            TextureFormat::R8Unorm,
            (3, 2),
            0,
            (3, 2),
            pixels.clone(),
        );
        let submits = exec.native_copy_submission_count();
        copy(
            &mut exec,
            &mut session,
            CopySpec {
                mip: 0,
                width: 3,
                height: 2,
                dst_offset: 1,
                bytes_per_row: 5,
                dst_size: 10,
            },
        );
        assert_eq!(exec.native_copy_submission_count(), submits);
        let output = exec.read_bytes(&session.resources, 2, 0, 10).unwrap();
        assert_eq!(&output[1..4], &pixels[..3]);
        assert_eq!(&output[6..9], &pixels[3..]);
        assert_eq!(output[4], 0xA5);
        assert_eq!(output[5], 0xA5);
    }

    #[test]
    fn aligned_rows_copy_directly_and_partial_mip_uses_the_requested_extent() {
        let (mut exec, mut session) = executor();
        let mip_width = 8;
        let mip_height = 6;
        let pixels: Vec<u8> = (0u32..mip_width * mip_height * 4)
            .map(|byte| byte.wrapping_mul(29) as u8)
            .collect();
        seed(
            &mut exec,
            &mut session,
            TextureFormat::Rgba8Unorm,
            (16, 12),
            1,
            (mip_width, mip_height),
            pixels.clone(),
        );
        let direct = exec.direct_copy_submission_count();
        let width = 3;
        let height = 4;
        let stride = 256;
        let size = u64::from((height - 1) * stride + width * 4);
        copy(
            &mut exec,
            &mut session,
            CopySpec {
                mip: 1,
                width,
                height,
                dst_offset: 0,
                bytes_per_row: stride,
                dst_size: size,
            },
        );
        assert_eq!(exec.direct_copy_submission_count(), direct + 1);

        let output = exec
            .read_bytes(&session.resources, 2, 0, size as usize)
            .unwrap();
        for row in 0..height as usize {
            let src = row * mip_width as usize * 4;
            let dst = row * stride as usize;
            assert_eq!(
                &output[dst..dst + width as usize * 4],
                &pixels[src..src + width as usize * 4]
            );
        }
    }

    #[test]
    fn rejected_layouts_and_empty_copies_do_not_submit_or_mutate() {
        let (mut exec, mut session) = executor();
        let pixels = vec![0x31; 4 * 4 * 4];
        seed(
            &mut exec,
            &mut session,
            TextureFormat::Rgba8Unorm,
            (4, 4),
            0,
            (4, 4),
            pixels,
        );
        destination(&mut exec, &mut session, 64, 0xA5);
        let baseline = exec
            .read_bytes(&session.resources, 2, 0, 64)
            .expect("baseline");
        let submits = exec.native_copy_submission_count();

        let rejected = [
            CopySpec {
                mip: 0,
                width: 4,
                height: 2,
                dst_offset: 60,
                bytes_per_row: 16,
                dst_size: 64,
            },
            CopySpec {
                mip: 0,
                width: 4,
                height: 2,
                dst_offset: 0,
                bytes_per_row: 12,
                dst_size: 64,
            },
            CopySpec {
                mip: 1,
                width: 1,
                height: 1,
                dst_offset: 0,
                bytes_per_row: 0,
                dst_size: 64,
            },
            CopySpec {
                mip: 0,
                width: 5,
                height: 1,
                dst_offset: 0,
                bytes_per_row: 0,
                dst_size: 64,
            },
        ];
        for spec in &rejected {
            assert!(submit_copy(&mut exec, &mut session, spec).is_err());
            assert_eq!(exec.native_copy_submission_count(), submits);
            assert_eq!(
                exec.read_bytes(&session.resources, 2, 0, 64).unwrap(),
                baseline
            );
        }

        for (width, height) in [(0, 4), (4, 0)] {
            submit_copy(
                &mut exec,
                &mut session,
                &CopySpec {
                    mip: 0,
                    width,
                    height,
                    dst_offset: 0,
                    bytes_per_row: 0,
                    dst_size: 64,
                },
            )
            .expect("empty copy");
            assert_eq!(exec.native_copy_submission_count(), submits);
            assert_eq!(
                exec.read_bytes(&session.resources, 2, 0, 64).unwrap(),
                baseline
            );
        }
    }

    #[test]
    fn multisampled_source_is_rejected_before_submission_or_destination_mutation() {
        let (mut exec, mut session) = executor();
        let mut desc = texture(TextureFormat::Rgba8Unorm, 4, 4, 1);
        desc.sample_count = 4;
        hl_gpu::runtime::submit(&mut session, &mut exec, 0, &[Cmd::CreateTexture(1, desc)])
            .expect("create multisampled texture");
        destination(&mut exec, &mut session, 64, 0xA5);
        let baseline = exec
            .read_bytes(&session.resources, 2, 0, 64)
            .expect("baseline");
        let submits = exec.native_copy_submission_count();

        let err = submit_copy(
            &mut exec,
            &mut session,
            &CopySpec {
                mip: 0,
                width: 4,
                height: 4,
                dst_offset: 0,
                bytes_per_row: 16,
                dst_size: 64,
            },
        )
        .expect_err("multisampled texture must require resolve");
        assert!(matches!(err, hl_gpu::GpuError::Unsupported(_)));
        assert_eq!(exec.native_copy_submission_count(), submits);
        assert_eq!(
            exec.read_bytes(&session.resources, 2, 0, 64).unwrap(),
            baseline
        );
    }
}

#[cfg(test)]
mod multi_group_render_proof {
    //! The Zed multi-bind-group unblock, FAIL-before / PASS-after in one test.
    //!
    //! Zed's GPUI/wgpu renderer draws with a render pipeline whose VERTEX reads a uniform in group 0 and
    //! whose FRAGMENT samples a texture+sampler in group **1** (two distinct bind-group SET INDICES). It
    //! binds a set-0 bind group (the uniform) AND a set-1 bind group (the texture+sampler), then draws.
    //!
    //! The OLD `run_render_pass` was single-group: it tracked only the LAST `SetBindGroup`, built every bind
    //! group against `pipeline.get_bind_group_layout(0)`, and bound it at slot 0 — so the set-1 bind group
    //! (2 entries: texture+sampler) was validated against GROUP 0's layout (1 uniform buffer). wgpu rejects
    //! that with "Number of bindings (…) does not match the bind group layout (…)"; the uncaptured device
    //! error marked Zed's device lost. `run_render_pass` now tracks a pending bind group PER set index and
    //! builds each against THAT set's own layout (`get_bind_group_layout(index)`), binding it at its index —
    //! mirroring `run_compute_pass` — so the set-1 group validates against group 1's layout and the draw
    //! samples the group-1 texture. This test asserts both halves against the SAME built pipeline: the
    //! group-0-layout binding of the set-1 descriptor errors (the old bug), and the full two-group draw reads
    //! back the sampled texel (the fix).

    use hl_gpu::protocol::model::descriptor::{
        BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
        RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
    };
    use hl_gpu::protocol::model::enums::{
        buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat,
        Topology,
    };
    use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
    use hl_gpu::{
        Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
        ShaderPayloadKind,
    };

    use crate::pipeline::PipelineNative;
    use crate::{DeviceConfig, WgpuExecutor};

    // Vertex reads the group-0 uniform (its `.w` scales the clip position → 1.0 = identity), emits a
    // fullscreen triangle with a constant uv.
    const VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U0 { vec4 scale; } u0;
layout(location = 0) out vec2 uv;
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    uv = vec2(0.5, 0.5);
    gl_Position = vec4(p[gl_VertexIndex], 0.0, u0.scale.w);
}
"#;

    // Fragment samples the group-1 texture through the group-1 sampler and multiplies by the group-0
    // uniform (so the pipeline genuinely reads BOTH sets: group 0 in vertex+fragment, group 1 in fragment).
    const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U0 { vec4 scale; } u0;
layout(set = 1, binding = 0) uniform texture2D t0_tex;
layout(set = 1, binding = 1) uniform sampler   t0_smp;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;
void main() {
    color = texture(sampler2D(t0_tex, t0_smp), uv) * u0.scale;
}
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

    fn nearest() -> SamplerDesc {
        SamplerDesc {
            min_filter: Filter::Nearest,
            mag_filter: Filter::Nearest,
            mip_filter: Filter::Nearest,
            address_u: AddressMode::ClampToEdge,
            address_v: AddressMode::ClampToEdge,
            address_w: AddressMode::ClampToEdge,
            ..SamplerDesc::default()
        }
    }

    #[test]
    fn set_index_one_binds_against_its_own_group_layout() {
        let mut exec = WgpuExecutor::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");

        let texel: [u8; 4] = [30, 150, 220, 255]; // the group-1 texture's single texel
        let scale: [f32; 4] = [1.0, 1.0, 1.0, 1.0]; // group-0 uniform: identity (passthrough of the texel)

        let caps = exec.capabilities();
        let mut limits = Limits::from_capabilities(caps);
        limits.copy_alignment = 1;
        let mut s = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );

        // Create resources, the two-group pipeline, and both bind groups (set 0 = the uniform; set 1 = the
        // texture+sampler). No draw yet — this populates `s.resources` so the FAIL-before check below can
        // reach the built pipeline's per-group layouts.
        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[
                Cmd::CreateTexture(
                    1,
                    tex(4, 4, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
                ),
                Cmd::CreateTexture(
                    2,
                    tex(1, 1, texture_usage::SAMPLED | texture_usage::COPY_DST),
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
                    data: scale.iter().flat_map(|f| f.to_le_bytes()).collect(),
                },
                Cmd::CreateBuffer(
                    2,
                    BufferDesc {
                        size: 4,
                        usage: buffer_usage::COPY_SRC,
                        label: String::new(),
                    },
                ),
                Cmd::WriteBuffer {
                    id: 2,
                    offset: 0,
                    data: texel.to_vec(),
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
                Cmd::CreateSampler(1, nearest()),
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
                ),
                // Bind group 1 = SET 0: the uniform buffer (1 entry).
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
                // Bind group 2 = SET 1: the texture + sampler (2 entries).
                Cmd::CreateBindGroup(
                    2,
                    BindGroupDesc {
                        set: 1,
                        entries: vec![
                            BindEntry {
                                binding: 0,
                                resource: BindResource::Texture { id: 2 },
                            },
                            BindEntry {
                                binding: 1,
                                resource: BindResource::Sampler { id: 1 },
                            },
                        ],
                    },
                ),
            ],
        )
        .expect("the two-group pipeline + both bind groups must create cleanly");

        // FAIL-BEFORE: the OLD single-group path built EVERY bind group against group 0's layout and bound it
        // at slot 0. Reproduce that for the SET-1 bind group (id 2): built against `get_bind_group_layout(0)`
        // its 2 texture/sampler entries do NOT match group 0's single uniform-buffer binding — the exact
        // wgpu validation error that marked Zed's device lost. (The new path builds it against group 1's
        // layout instead, which the passing draw below proves.)
        let (layout0, filter) = match PipelineNative::get(&s.resources, 1).unwrap() {
            PipelineNative::Render {
                pipeline,
                used_bindings,
                ..
            } => (pipeline.get_bind_group_layout(0), used_bindings.clone()),
            PipelineNative::Compute { .. } => unreachable!("pipeline 1 is a render pipeline"),
        };
        exec.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let _bad = exec
            .build_bind_group(
                &s.resources,
                &layout0,
                exec.bind_group(&s.resources, 2).unwrap(),
                Some(&filter),
                true,
                None,
                None,
                None,
            )
            .expect("building the descriptor does not itself return Err (the error surfaces via the scope)");
        let err = pollster::block_on(exec.gpu.device.pop_error_scope()).expect(
            "validating the SET-1 bind group against GROUP 0's layout MUST error — if it did not, this test \
             no longer reproduces the single-group bug that lost Zed its device",
        );
        assert!(
            err.to_string().to_lowercase().contains("bind"),
            "the old-path failure must be a bind-group/layout mismatch, got: {err}"
        );

        // Seed the sampled texture before measuring the render submit. This transfer is CPU-mediated by
        // the byte-addressable protocol path and therefore legitimately performs a readback wait.
        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 2,
                    src_offset: 0,
                    bytes_per_row: 4,
                    dst: 2,
                    mip: 0,
                    width: 1,
                    height: 1,
                }],
                signal: None,
            })],
        )
        .expect("seed sampled texture");

        // PASS-AFTER: the full two-group draw. `run_render_pass` binds set 0 against group 0's layout and set
        // 1 against group 1's layout, at their declared indices, and samples the group-1 texture.
        let waits_before_draw = exec.completion_wait_count();
        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 }, // set 0 → the uniform
                    Enc::SetBindGroup { index: 1, group: 2 }, // set 1 → the texture + sampler
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            })],
        )
        .expect(
            "the two-group draw must run: set 1 validated against GROUP 1's layout (not group 0), the exact \
             multi-bind-group draw the old single-group path could not honor",
        );
        assert_eq!(
            exec.completion_wait_count(),
            waits_before_draw,
            "an ordinary render submission must remain queue-ordered instead of blocking the host"
        );

        let px = exec.read_texture(&s.resources, 1).unwrap();
        assert_eq!(
            exec.completion_wait_count(),
            waits_before_draw + 1,
            "CPU-visible texture readback must wait for the queued draw"
        );

        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[Cmd::CreateFence(9)])
            .expect("create completion fence");
        let waits_before_signal = exec.completion_wait_count();
        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[Cmd::Submit(CommandBuffer {
                encoder: Vec::new(),
                signal: Some((9, 1)),
            })],
        )
        .expect("signal completion fence");
        assert_eq!(
            exec.completion_wait_count(),
            waits_before_signal,
            "enqueuing an emulated completion fence must not drain queued GPU work"
        );
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[Cmd::WaitFence { id: 9, value: 1 }])
            .expect("completed fence value must be observable");

        for (i, out) in px.chunks_exact(4).enumerate() {
            assert_eq!(
                out, texel,
                "pixel {i}: must be the group-1 texture's texel {texel:?} (group-0 scale is identity), \
                 proving the set-1 bind group matched GROUP 1's layout and the draw sampled it"
            );
        }
    }
}

#[cfg(test)]
mod clamp_viewport_tests {
    //! Unit coverage for the GL→wgpu viewport intersection (no GPU needed).
    use super::super::Viewport;

    #[test]
    fn fully_in_bounds_is_unchanged() {
        // A viewport already inside the target passes through verbatim — the common (non-scrolled) path
        // stays pixel-exact.
        assert_eq!(
            Viewport::new(4.0, 4.0, 24.0, 16.0, 32, 32),
            Some(Viewport {
                clipped: (4.0, 4.0, 24.0, 16.0),
                correction: [1.0, 1.0, 0.0, 0.0],
            })
        );
        assert_eq!(
            Viewport::new(0.0, 0.0, 32.0, 32.0, 32, 32),
            Some(Viewport {
                clipped: (0.0, 0.0, 32.0, 32.0),
                correction: [1.0, 1.0, 0.0, 0.0],
            })
        );
    }

    #[test]
    fn negative_origin_and_oversize_clamp_to_visible_subrect() {
        // Chrome's scrolled-layer shape: negative Y + a height taller than the target. Intersecting a rect
        // `x=-8,y=-16,w=48,h=40` with a 32×32 target yields `[0,32)×[0,24)`.
        assert_eq!(
            Viewport::new(-8.0, -16.0, 48.0, 40.0, 32, 32)
                .expect("viewport intersects")
                .clipped,
            (0.0, 0.0, 32.0, 24.0)
        );
        // The precise Chrome frame from the bug report: y=-386, h=642 into a 256-tall target → rows [0,256).
        assert_eq!(
            Viewport::new(0.0, -386.0, 832.0, 642.0, 832, 256)
                .expect("viewport intersects")
                .clipped,
            (0.0, 0.0, 832.0, 256.0)
        );
    }

    #[test]
    fn wholly_out_of_bounds_is_empty() {
        // Entirely past the right/bottom edge, entirely above/left, or a zero/negative size → None (the
        // caller drops the draw; GL would rasterize nothing through such a viewport).
        assert_eq!(Viewport::new(100.0, 100.0, 32.0, 32.0, 32, 32), None);
        assert_eq!(Viewport::new(-64.0, -64.0, 32.0, 32.0, 32, 32), None);
        assert_eq!(Viewport::new(10.0, 10.0, 0.0, 10.0, 32, 32), None);
        assert_eq!(Viewport::new(10.0, 10.0, 10.0, -5.0, 32, 32), None);
    }
}
