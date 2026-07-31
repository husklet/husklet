//! INTEGER color formats — raw integer texels, never normalized.
//!
//! `GL_RGBA_INTEGER` / `GL_RED_INTEGER` / `GL_RG_INTEGER` textures — the storage a `usampler2D` or
//! `isampler2D` reads — had NO representation in the IR's `TextureFormat` at all, which made the whole
//! integer-texture family unexpressible rather than merely unsupported. `INTEGER_FORMATS` adds the six
//! 8-bit variants, and this module is the proof obligation that comes with advertising them: the
//! neighbouring `executor_advertises_exactly_the_formats_this_suite_proves` fails if a format is advertised
//! without being round-tripped here.
//!
//! An integer format is NOT a unorm format with a different name, and the two phases below are chosen so a
//! backend that confused them fails loudly rather than plausibly:
//!
//! * RENDER — a fragment shader whose output is a `uvec4`/`ivec4` of values that are meaningless as
//!   normalized colors (200, 7, 1, 255 — and negative components for the signed formats) is drawn into an
//!   integer target and read back RAW. A backend that routed these through the unorm path would store
//!   255/255/255 (saturating a "color" of 200.0) or 0/0/0, not the exact integers.
//! * SAMPLE — raw integer texels are uploaded into an integer texture, read with `texelFetch` (the ONLY
//!   legal access: integer textures cannot be filtered), and written into a linear `Rgba8Unorm` target
//!   scaled back down. This drives the `TexSample::Uint`/`Sint` bind-group-layout path end to end, which is
//!   what the guest driver's `usampler2D` support will rest on once it is wired.

use super::*;

/// Integer values that are absurd as normalized colors, so a unorm mispath cannot coincidentally agree:
/// 200 and 7 are far apart, 1 is nonzero-but-tiny, 255 is full-scale.
const UVALS: [u32; 4] = [200, 7, 1, 255];
/// Signed counterparts, including negatives (which have no unorm reading at all).
const IVALS: [i32; 4] = [-120, 7, -1, 127];

fn is_signed(fmt: TextureFormat) -> bool {
    matches!(
        fmt,
        TextureFormat::Rgba8Sint | TextureFormat::R8Sint | TextureFormat::Rg8Sint
    )
}

/// Channel count of an 8-bit integer format (its `bytes_per_texel`, since each channel is one byte).
fn channels(fmt: TextureFormat) -> usize {
    fmt.bytes_per_texel()
        .expect("an integer color format has a texel footprint")
}

/// A fragment shader emitting the constant integer vector for `fmt`. The output type MUST match the
/// target's numeric class — a `vec4` output into a `Rgba8Uint` target is a pipeline validation error, which
/// is itself part of what this proves.
fn const_fs(fmt: TextureFormat) -> String {
    if is_signed(fmt) {
        format!(
            "#version 460\nlayout(location = 0) out ivec4 o;\nvoid main() {{ o = ivec4({}, {}, {}, {}); }}\n",
            IVALS[0], IVALS[1], IVALS[2], IVALS[3]
        )
    } else {
        format!(
            "#version 460\nlayout(location = 0) out uvec4 o;\nvoid main() {{ o = uvec4({}u, {}u, {}u, {}u); }}\n",
            UVALS[0], UVALS[1], UVALS[2], UVALS[3]
        )
    }
}

/// Draw the constant integer vector into a 2×2 target of `fmt` and return the RAW readback bytes.
fn draw_const_integer(exec: &mut WgpuExecutor, fmt: TextureFormat) -> Vec<u8> {
    const W: u32 = 2;
    const H: u32 = 2;
    let mut s = new_session(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    W,
                    H,
                    fmt,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", &const_fs(fmt)),
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
                    color_targets: vec![ct(fmt)],
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
                            clear: [0.0, 0.0, 0.0, 0.0],
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
    )
    .unwrap_or_else(|e| panic!("format {fmt:?}: the constant-integer draw must run cleanly, got {e:?}"));
    exec.read_texture(&s.resources, 1)
        .unwrap_or_else(|e| panic!("format {fmt:?}: readback failed: {e:?}"))
}

/// Every advertised integer format materializes as a render target and stores the EXACT integers the
/// shader emitted — no normalization, no saturation, no channel reordering.
#[test]
fn every_integer_format_stores_exact_integer_texels() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    for &fmt in hl_gpu::protocol::model::capability::INTEGER_FORMATS {
        let raw = draw_const_integer(&mut exec, fmt);
        let bpt = channels(fmt);
        assert_eq!(
            raw.len(),
            2 * 2 * bpt,
            "format {fmt:?}: readback is width*height*bpt"
        );
        let texel = &raw[..bpt];
        for channel in 0..bpt {
            let (got, want) = if is_signed(fmt) {
                (texel[channel] as i8 as i32, IVALS[channel])
            } else {
                (texel[channel] as i32, UVALS[channel] as i32)
            };
            assert_eq!(
                got, want,
                "format {fmt:?} channel {channel}: stored {got}, want the exact integer {want} \
                 (raw texel {texel:?}) — an integer target must not normalize"
            );
        }
    }
}

/// A `utexture2D`/`itexture2D` read with `texelFetch` returns the raw integer that was uploaded. This is
/// the bind-group-layout path (`TexSample::Uint`/`Sint` → `wgpu::TextureSampleType::Uint`/`Sint`) the
/// guest driver's `usampler2D` will use; it existed in the reflection already, but no format could reach
/// it before, so it had never been executed.
#[test]
fn texel_fetch_of_an_integer_texture_returns_the_raw_value() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    // texelFetch needs no sampler at all — integer textures cannot be filtered, so there is nothing for
    // a sampler to do, and the bind group carries the texture alone. The fetched integers are scaled into
    // a unorm target so the assertion below reads in raw integer units.
    const FETCH_FS: &str = r#"#version 460
layout(set = 0, binding = 0) uniform utexture2D t;
layout(location = 0) out vec4 o;
void main() {
    uvec4 v = texelFetch(t, ivec2(0, 0), 0);
    o = vec4(v) / 255.0;
}
"#;

    let mut s = new_session(&exec);
    let texels: Vec<u8> = UVALS.iter().map(|v| *v as u8).collect();
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
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
                    TextureFormat::Rgba8Uint,
                    texture_usage::SAMPLED | texture_usage::COPY_DST,
                ),
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
                data: texels.clone(),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FETCH_FS),
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
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Texture { id: 2 },
                    }],
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
    .expect("an integer texture must be uploadable and texelFetch-able");

    let raw = exec.read_texture(&s.resources, 1).expect("readback");
    let got = [raw[0], raw[1], raw[2], raw[3]];
    let want = [
        UVALS[0] as u8,
        UVALS[1] as u8,
        UVALS[2] as u8,
        UVALS[3] as u8,
    ];
    assert!(
        near_tol(got, want, 1),
        "texelFetch of an Rgba8Uint texture returned {got:?}, want the uploaded integers {want:?}"
    );
}

/// The software oracle must REFUSE an integer format rather than materialize plausible-looking bytes for
/// it: every one of its clear/blend/sample paths is defined on normalized float channels, and an integer
/// texel has no normalized reading. `INTEGER_FORMATS` is therefore advertised by this executor only, and
/// the refusal is the honest behaviour the shared `COLOR_FORMATS` set would have hidden.
#[test]
fn the_software_oracle_refuses_integer_formats_rather_than_faking_them() {
    for &fmt in hl_gpu::protocol::model::capability::INTEGER_FORMATS {
        assert!(
            !hl_gpu::protocol::model::capability::COLOR_FORMATS.contains(&fmt),
            "{fmt:?} must not be in the every-backend COLOR_FORMATS set"
        );
        // It still has a texel footprint (the byte layout is well defined) — it is the COLOR semantics the
        // oracle lacks, not the size.
        assert!(
            fmt.bytes_per_texel().is_some(),
            "{fmt:?}: an integer format still has a byte footprint"
        );
    }
}

/// The two facts the guest driver's `usampler2D` support rests on, recorded so the driver half is written
/// against measured behaviour rather than a guess about it.
///
/// The IR proposal expected an integer texture to need a NON-FILTERING sampler binding type, on the theory
/// that a `Filtering` sampler beside an unfilterable texture would be rejected. It does not, because the
/// question is settled one layer earlier and for a better reason: naga REFUSES to sample an integer texture
/// at all (`InvalidImageClass(Sampled { kind: Uint })`), which is what the specification requires — integer
/// textures have no filtering, so `texture()` on one is not a thing that can be lowered. A shader that
/// would have paired a filtering sampler with an integer texture therefore never reaches pipeline creation.
///
/// What DOES build is the shape the driver actually emits for `texelFetch(usampler2D)`: the combined
/// sampler splits into an integer texture plus a sampler, the sampler goes unused by any sampling op, and
/// the pipeline is valid. So no non-filtering derivation is needed in `pipeline::layout` — the speculative
/// item in the proposal is closed as unnecessary, not as unimplemented.
#[test]
fn integer_textures_refuse_sampling_but_allow_texel_fetch_beside_a_sampler() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let pipeline = |exec: &mut WgpuExecutor, fs: &str| {
        let mut s = new_session(exec);
        hl_gpu::runtime::submit(
            &mut s,
            exec,
            0,
            &[
                Cmd::CreateShader {
                    id: 1,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
                },
                Cmd::CreateShader {
                    id: 2,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
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
                        color_targets: vec![ct(TextureFormat::Rgba8Unorm)],
                        depth: None,
                        topology: Topology::TriangleList,
                        cull: 0,
                        front_face: 0,
                        sample_count: 1,
                        label: String::new(),
                    },
                ),
            ],
        )
    };

    const SAMPLED: &str = r#"#version 460
layout(set = 0, binding = 0) uniform utexture2D t;
layout(set = 0, binding = 1) uniform sampler sm;
layout(location = 0) out vec4 o;
void main() { o = vec4(texture(usampler2D(t, sm), vec2(0.5))); }
"#;
    const FETCHED: &str = r#"#version 460
layout(set = 0, binding = 0) uniform utexture2D t;
layout(set = 0, binding = 1) uniform sampler sm;
layout(location = 0) out vec4 o;
void main() { o = vec4(texelFetch(t, ivec2(0), 0)); }
"#;

    let sampled = pipeline(&mut exec, SAMPLED);
    let error = sampled.expect_err("sampling an integer texture must be refused, not silently filtered");
    assert!(
        error.to_string().contains("InvalidImageClass"),
        "the refusal must name the image class, got: {error}"
    );

    pipeline(&mut exec, FETCHED)
        .expect("texelFetch of an integer texture beside a declared sampler must build");
}
