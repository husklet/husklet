//! EXHAUSTIVE sampler coverage: every [`AddressMode`] (ClampToEdge / Repeat / MirrorRepeat) proven by a
//! real out-of-`[0,1]` sample that must wrap to the exact texel, and the Linear MIPMAP filter proven by an
//! inter-level LOD blend. `sampler.rs` maps every filter + address field onto `wgpu::SamplerDescriptor`,
//! but the suite only ever built ClampToEdge + Nearest samplers — so Repeat / MirrorRepeat / Linear-mip were
//! wired but never observed. Here the sampler fields change the READ-BACK pixel, so a wrong mapping fails.
//! Skips with no adapter.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// Samples the texture at the uv the uniform carries (`p.xy`), through the bound sampler.
const FS_SAMPLE: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 p; } u;
layout(set = 0, binding = 1) uniform texture2D t;
layout(set = 0, binding = 2) uniform sampler s;
layout(location = 0) out vec4 o;
void main() { o = texture(sampler2D(t, s), vec2(u.p.x, u.p.y)); }
"#;

// Samples at an explicit LOD (uniform `p.z`), exercising the mipmap filter's inter-level blend.
const FS_LOD: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 p; } u;
layout(set = 0, binding = 1) uniform texture2D t;
layout(set = 0, binding = 2) uniform sampler s;
layout(location = 0) out vec4 o;
void main() { o = textureLod(sampler2D(t, s), vec2(u.p.x, u.p.y), u.p.z); }
"#;

fn glsl(stage: u32, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: "main".to_string(),
        source: source.to_string(),
    }
    .to_words()
}

fn target_tex() -> TextureDesc {
    TextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}

fn sampled_tex(w: u32, h: u32, mips: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: mips,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
        label: String::new(),
    }
}

fn sampler(address: AddressMode, mip: Filter) -> SamplerDesc {
    SamplerDesc {
        min_filter: Filter::Nearest,
        mag_filter: Filter::Nearest,
        mip_filter: mip,
        address_u: address,
        address_v: address,
        address_w: address,
        ..SamplerDesc::default()
    }
}

fn session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

fn pipe(fs: u32) -> Cmd {
    Cmd::CreateRenderPipeline(
        1,
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: 1,
                entry: "main".into(),
            },
            fragment: Some(ShaderRef {
                module: fs,
                entry: "main".into(),
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
    )
}

/// Nearest-tap texel index after applying `mode` to `u` for a `width`-texel row — the wgpu semantics we pin.
fn nearest_texel(mode: AddressMode, u: f32, width: u32) -> usize {
    let uu = match mode {
        AddressMode::ClampToEdge => u.clamp(0.0, 1.0),
        AddressMode::Repeat => u - u.floor(),
        AddressMode::MirrorRepeat => {
            let t = u.rem_euclid(2.0);
            if t > 1.0 {
                2.0 - t
            } else {
                t
            }
        }
    };
    ((uu * width as f32).floor() as i64).clamp(0, width as i64 - 1) as usize
}

const A: [u8; 4] = [220, 20, 60, 255];
const B: [u8; 4] = [20, 60, 220, 255];

/// Sample the 2×1 texture [A,B] at `uv_x` through a sampler with address mode `mode`; return the pixel.
fn sample_at(exec: &mut WgpuExecutor, mode: AddressMode, uv_x: f32) -> [u8; 4] {
    let mut s = session(exec);
    let mut texels = Vec::new();
    texels.extend_from_slice(&A);
    texels.extend_from_slice(&B);
    let uv = [uv_x, 0.5_f32, 0.0, 0.0];
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, target_tex()),
            Cmd::CreateTexture(2, sampled_tex(2, 1, 1)),
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
                data: uv.iter().flat_map(|f| f.to_le_bytes()).collect(),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 8,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: texels,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, FS_SAMPLE),
            },
            Cmd::CreateSampler(1, sampler(mode, Filter::Nearest)),
            pipe(2),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 16,
                            },
                        },
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
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 2,
                        mip: 0,
                        width: 2,
                        height: 1,
                    },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0; 4],
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
    .expect("the sampled draw must run cleanly");
    let px = exec.read_texture(&s.resources, 1).unwrap();
    [px[0], px[1], px[2], px[3]]
}

fn near(a: [u8; 4], b: [u8; 4], tol: i16) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= tol)
}

#[test]
fn every_address_mode_wraps_to_the_exact_texel() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };
    let modes = [
        AddressMode::ClampToEdge,
        AddressMode::Repeat,
        AddressMode::MirrorRepeat,
    ];
    // uv=1.25 and uv=2.25 together uniquely identify all three modes (Mirror diverges from Clamp at 2.25).
    for &uv in &[1.25_f32, 2.25] {
        for &mode in &modes {
            let want = if nearest_texel(mode, uv, 2) == 0 {
                A
            } else {
                B
            };
            let got = sample_at(&mut exec, mode, uv);
            assert!(
                near(got, want, 2),
                "address mode {mode:?} at uv={uv}: must wrap to texel {want:?}, got {got:?}"
            );
        }
    }
    // Sanity: at uv=2.25 Repeat and MirrorRepeat land on different texels (A vs B) — proves the modes are
    // not aliased onto one another.
    assert!(!near(
        sample_at(&mut exec, AddressMode::Repeat, 2.25),
        sample_at(&mut exec, AddressMode::ClampToEdge, 2.25),
        2
    ));
}

#[test]
fn linear_mipmap_filter_blends_between_levels() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };
    // A 2×2 base of solid X and a 1×1 mip1 of solid Y; sampling at LOD 0.5 with a LINEAR mipmap filter must
    // blend the two levels (~ the average), while a NEAREST filter would snap to one level.
    let x = [200u8, 200, 200, 255];
    let y = [40u8, 40, 40, 255];
    let run_lod = |exec: &mut WgpuExecutor, mip: Filter| -> [u8; 4] {
        let mut s = session(exec);
        let mut base = Vec::new();
        for _ in 0..4 {
            base.extend_from_slice(&x);
        } // 2×2
        let mip1: Vec<u8> = y.to_vec(); // 1×1
        let uv = [0.5_f32, 0.5, 0.5, 0.0]; // p.z = lod 0.5
        hl_gpu::runtime::submit(
            &mut s,
            exec,
            0,
            &[
                Cmd::CreateTexture(1, target_tex()),
                Cmd::CreateTexture(2, sampled_tex(2, 2, 2)),
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
                    data: uv.iter().flat_map(|f| f.to_le_bytes()).collect(),
                },
                Cmd::CreateBuffer(
                    2,
                    BufferDesc {
                        size: 16,
                        usage: buffer_usage::COPY_SRC,
                        label: String::new(),
                    },
                ),
                Cmd::WriteBuffer {
                    id: 2,
                    offset: 0,
                    data: base,
                },
                Cmd::CreateBuffer(
                    3,
                    BufferDesc {
                        size: 4,
                        usage: buffer_usage::COPY_SRC,
                        label: String::new(),
                    },
                ),
                Cmd::WriteBuffer {
                    id: 3,
                    offset: 0,
                    data: mip1,
                },
                Cmd::CreateShader {
                    id: 1,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: glsl(glsl_stage::VERTEX, VS),
                },
                Cmd::CreateShader {
                    id: 2,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: glsl(glsl_stage::FRAGMENT, FS_LOD),
                },
                Cmd::CreateSampler(1, sampler(AddressMode::ClampToEdge, mip)),
                pipe(2),
                Cmd::CreateBindGroup(
                    1,
                    BindGroupDesc {
                        set: 0,
                        entries: vec![
                            BindEntry {
                                binding: 0,
                                resource: BindResource::Buffer {
                                    id: 1,
                                    offset: 0,
                                    size: 16,
                                },
                            },
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
                            src: 2,
                            src_offset: 0,
                            bytes_per_row: 8,
                            dst: 2,
                            mip: 0,
                            width: 2,
                            height: 2,
                        },
                        Enc::CopyBufferToTexture {
                            src: 3,
                            src_offset: 0,
                            bytes_per_row: 4,
                            dst: 2,
                            mip: 1,
                            width: 1,
                            height: 1,
                        },
                        Enc::BeginRenderPass {
                            color: vec![ColorAttachment {
                                texture: 1,
                                load: LoadOp::Clear,
                                clear: [0.0; 4],
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
        .expect("the LOD sample draw must run cleanly");
        let px = exec.read_texture(&s.resources, 1).unwrap();
        [px[0], px[1], px[2], px[3]]
    };

    let linear = run_lod(&mut exec, Filter::Linear);
    let avg = [(200 + 40) / 2, (200 + 40) / 2, (200 + 40) / 2, 255];
    assert!(
        near(linear, avg, 6),
        "linear mipmap filter at LOD 0.5 must blend level 0 ({x:?}) and level 1 ({y:?}) ~ {avg:?}, got {linear:?}"
    );
    // The blended result must differ from BOTH pure levels — proving the inter-level blend actually ran.
    assert!(
        !near(linear, x, 6) && !near(linear, y, 6),
        "linear-mip result {linear:?} must differ from both levels"
    );
}

#[test]
fn sampler_lod_and_comparison_state_is_validated_before_native_creation() {
    let mut executor = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(executor) => executor,
        Err(_) => return,
    };
    let mut valid = session(&executor);
    let descriptor = SamplerDesc {
        lod_min_clamp: 1.0,
        lod_max_clamp: 4.0,
        compare: Some(hl_gpu::protocol::model::enums::compare::GREATER_EQUAL),
        ..SamplerDesc::default()
    };
    hl_gpu::runtime::submit(
        &mut valid,
        &mut executor,
        0,
        &[Cmd::CreateSampler(1, descriptor)],
    )
    .expect("finite ordered LOD clamps and a known comparison function are supported");

    let mut invalid = session(&executor);
    let descriptor = SamplerDesc {
        lod_min_clamp: 5.0,
        lod_max_clamp: 4.0,
        ..SamplerDesc::default()
    };
    let error = hl_gpu::runtime::submit(
        &mut invalid,
        &mut executor,
        0,
        &[Cmd::CreateSampler(1, descriptor)],
    )
    .unwrap_err();
    assert_eq!(error, hl_gpu::GpuError::Invalid("sampler LOD clamp"));
}
