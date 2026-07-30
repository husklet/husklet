use super::*;

fn sample_stored(exec: &mut WgpuExecutor, src_fmt: TextureFormat, stored: &[u8]) -> [u8; 4] {
    let mut s = new_session(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateTexture(
                2,
                tex(
                    1,
                    1,
                    src_fmt,
                    texture_usage::SAMPLED | texture_usage::COPY_DST,
                ),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: stored.len() as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: stored.to_vec(),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS_SAMPLE),
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
                    ..SamplerDesc::default()
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
                    color_targets: vec![ct(TextureFormat::Rgba8Unorm)],
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
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: stored.len() as u32,
                        dst: 2,
                        mip: 0,
                        width: 1,
                        height: 1,
                    },
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
    .unwrap_or_else(|e| panic!("sample {src_fmt:?}: draw must run cleanly, got {e:?}"));
    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

// ==========================================================================================================
// COLOR: every advertised color format round-trips to its exact stored bytes.
// ==========================================================================================================

#[test]
fn bgra_swizzles_and_srgb_decodes_on_sample() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // A Bgra8Unorm texel stored as B=64,G=128,R=191,A=255 must SAMPLE as linear R=191,G=128,B=64,A=255.
    let got = sample_stored(&mut exec, TextureFormat::Bgra8Unorm, &[64, 128, 191, 255]);
    assert!(near_tol(got, [191, 128, 64, 255], 2),
        "Bgra8Unorm stored B,G,R,A=[64,128,191,255] must sample to R,G,B,A=[191,128,64,255], got {got:?}");

    // A Bgra8Srgb texel stored 188,188,188 (sRGB of ~0.5) must sample DECODED to ~128 linear per channel,
    // and the swizzle keeps it symmetric. Use an asymmetric stored value to prove BOTH decode AND swizzle:
    // stored B=137(sRGB .25), G=188(sRGB .5), R=225(sRGB .75) → decodes+swizzles to R≈191,G≈128,B≈64.
    let got = sample_stored(&mut exec, TextureFormat::Bgra8Srgb, &[137, 188, 225, 255]);
    assert!(near_tol(got, [191, 128, 64, 255], 3),
        "Bgra8Srgb stored B,G,R=[137,188,225] must DECODE+swizzle to linear R,G,B≈[191,128,64], got {got:?}");

    // Control: the SAME bytes in a plain Bgra8Unorm are swizzled but NOT decoded → stay 225,188,137.
    let raw = sample_stored(&mut exec, TextureFormat::Bgra8Unorm, &[137, 188, 225, 255]);
    assert!(
        near_tol(raw, [225, 188, 137, 255], 2),
        "Bgra8Unorm passthrough (no decode) must sample R,G,B=[225,188,137], got {raw:?}"
    );
    assert!(
        raw[0] as i16 - got[0] as i16 >= 20,
        "the sRGB texel decodes ({}) but the identical Unorm texel does not ({}) — decode is real",
        got[0],
        raw[0]
    );

    eprintln!(
        "sample: Bgra8Unorm swizzles B,G,R→R,G,B; Bgra8Srgb additionally decodes sRGB→linear"
    );
}

#[test]
fn coverage_formats_sample_missing_channels_with_gl_compatible_defaults() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Chrome/Skia glyph atlases commonly use one- or two-channel normalized textures. Their shader sees
    // the stored channels plus WebGPU/OpenGL's defined defaults: absent color channels are zero and absent
    // alpha is one. A wrong texture view, byte pitch, or format conversion makes text disappear while solid
    // geometry continues to render, so assert the actual sampled pixel rather than only resource descriptors.
    let red = sample_stored(&mut exec, TextureFormat::R8Unorm, &[96]);
    assert!(
        near_tol(red, [96, 0, 0, 255], 2),
        "R8 coverage must sample as [R,0,0,1], got {red:?}"
    );

    let red_green = sample_stored(&mut exec, TextureFormat::Rg8Unorm, &[64, 192]);
    assert!(
        near_tol(red_green, [64, 192, 0, 255], 2),
        "RG8 coverage must sample as [R,G,0,1], got {red_green:?}"
    );
}

// ==========================================================================================================
// DEPTH: each advertised depth format backs a real depth attachment; nearest fragment occludes.
// ==========================================================================================================
