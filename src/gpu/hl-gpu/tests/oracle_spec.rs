//! SPEC-CORRECTNESS oracle battery: every value asserted here is HAND-COMPUTED from the WebGPU/Vulkan
//! semantics the CPU reference oracle claims to implement — NOT captured from the executor. A silent
//! oracle no-op or a wrong-but-plausible result (a dropped write-mask, a flipped cull, a blend that
//! doesn't blend, a copy that copies the wrong region) is a bug that would validate wrong behavior across
//! the whole differential fuzzer, so each op is pinned to an independently-derived expected value.
//!
//! Fills the gaps `conformance.rs`/`depth.rs`/`stencil.rs` leave: sRGB clear gamma-encode, gradient
//! (barycentric) draw, premultiplied source-over blend, per-channel write-mask, face cull × front-face,
//! CopyBufferToTexture / CopyTextureToTexture region copies, nearest + linear blit resample, and the
//! multisample-resolve path.

use hl_gpu::protocol::model::descriptor::{
    BlendState, BufferDesc, ColorAttachment, ColorTargetState, Extent3d, Origin3d,
    RenderPipelineDesc, ShaderRef, TextureDesc, TextureSubresource, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{Inst, KernelProgram, KERNEL_MAGIC};
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind, TextureId};

// -------------------------------------------------------------------------------------------------
// harness (mirrors conformance.rs / depth.rs; widens the session caps to whatever the batch needs)
// -------------------------------------------------------------------------------------------------

fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}
fn placeholder_shader() -> KernelProgram {
    KernelProgram {
        entry: "vs".into(),
        block: [1, 1, 1],
        params: vec![],
        param_bytes: 0,
        num_regions: 0,
        shared_bytes: 0,
        reg_count: 1,
        insts: vec![Inst::Ret],
    }
}

fn run(cmds: &[Cmd]) -> (hl_gpu::CpuExecutor, hl_gpu::Session) {
    let mut exec = hl_gpu::CpuExecutor::new();
    exec.define_kernel(1, placeholder_shader());
    let caps = exec.capabilities();
    let mut limits = hl_gpu::Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = hl_gpu::Session::new(
        limits,
        hl_gpu::GlobalLedger::unbounded(),
        Box::new(hl_gpu::FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, cmds).expect("oracle program must run cleanly");
    (exec, s)
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        label: String::new(),
    }
}
fn tex(w: u32, h: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}
fn tex_ms(w: u32, h: u32, samples: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: samples,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}
fn readback(exec: &hl_gpu::CpuExecutor, s: &hl_gpu::Session, id: u32, n: usize) -> Vec<u8> {
    let mut px = vec![0u8; n];
    exec.read_texture(&s.resources, TextureId(id), &mut px)
        .unwrap();
    px
}

/// A stride-24 vertex (2D pos at 0/4, RGBA color at 8..24) — the oracle's `stride >= 24` layout.
fn vtx24(x: f32, y: f32, c: [f32; 4]) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..4].copy_from_slice(&x.to_le_bytes());
    b[4..8].copy_from_slice(&y.to_le_bytes());
    b[8..12].copy_from_slice(&c[0].to_le_bytes());
    b[12..16].copy_from_slice(&c[1].to_le_bytes());
    b[16..20].copy_from_slice(&c[2].to_le_bytes());
    b[20..24].copy_from_slice(&c[3].to_le_bytes());
    b
}
/// A stride-12 vertex (3D pos only) — the oracle defaults its color to opaque white here.
fn vtx12(x: f32, y: f32) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&x.to_le_bytes());
    b[4..8].copy_from_slice(&y.to_le_bytes());
    // z at 8..12 stays 0.
    b
}

// =================================================================================================
// 1. sRGB clear must GAMMA-ENCODE (linear 0.5 -> 188), not naively quantize to 128.
// =================================================================================================

#[test]
fn srgb_clear_gamma_encodes_color_channels_but_not_alpha() {
    // linear 0.5 through the IEC 61966-2-1 OETF: 1.055*0.5^(1/2.4)-0.055 = 0.7353... -> round(0.7353*255)=188.
    // Alpha is a plain unorm quantize: 0.5 -> 128. A naive (wrong) oracle would store 128 for the color too.
    for fmt in [TextureFormat::Rgba8Srgb, TextureFormat::Bgra8Srgb] {
        let (exec, s) = run(&[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    fmt,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.5, 0.5, 0.5, 0.5],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ]);
        // Bgra vs Rgba only permutes byte order; all three color channels are 0.5 so the bytes match either way.
        assert_eq!(
            readback(&exec, &s, 1, 4),
            vec![188, 188, 188, 128],
            "sRGB {fmt:?} must gamma-encode color to 188 and quantize alpha to 128"
        );
    }
    // Contrast: a LINEAR Rgba8Unorm clear of the same 0.5 quantizes every channel to 128 (no gamma).
    let (exec, s) = run(&[
        Cmd::CreateTexture(
            1,
            tex(
                1,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [0.5, 0.5, 0.5, 0.5],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ]);
    assert_eq!(
        readback(&exec, &s, 1, 4),
        vec![128, 128, 128, 128],
        "linear clear must NOT gamma-encode"
    );
}

// =================================================================================================
// 2. gradient (barycentric) draw — the interpolated color is derived by hand from the edge functions.
// =================================================================================================

fn draw_pipeline(
    id: u32,
    blend: Option<BlendState>,
    write_mask: u32,
    cull: u32,
    front_face: u32,
    stride: u32,
    color_off: u32,
) -> Cmd {
    let mut attrs = vec![VertexAttr {
        location: 0,
        format: 0,
        offset: 0,
    }];
    if stride >= 24 {
        attrs.push(VertexAttr {
            location: 1,
            format: 0,
            offset: color_off,
        });
    }
    Cmd::CreateRenderPipeline(
        id,
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: 1,
                entry: "vs".into(),
            },
            fragment: None,
            vertex_buffers: vec![VertexLayout {
                stride,
                step_mode: 0,
                attrs,
            }],
            color_targets: vec![ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend,
                write_mask,
            }],
            depth: None,
            topology: Topology::TriangleList,
            cull,
            front_face,
            sample_count: 1,
            label: String::new(),
        },
    )
}

#[test]
fn gradient_draw_interpolates_vertex_colors_barycentrically() {
    // 4x1 target, one CCW fullscreen triangle with red/green/blue corners. For the pixel-0 centre (0.5,0.5)
    // the edge functions give barycentric weights [0.6875, 0.0625, 0.25] (hand-derived), so the color is
    //   R=0.6875, G=0.0625, B=0.25 -> [175, 16, 64, 255].
    // Pixel-3 centre (3.5,0.5) gives [0.3125, 0.4375, 0.25] -> [80, 112, 64, 255].
    let verts: Vec<u8> = [
        ((-1.0f32, -1.0f32), [1.0, 0.0, 0.0, 1.0]),
        ((3.0, -1.0), [0.0, 1.0, 0.0, 1.0]),
        ((-1.0, 3.0), [0.0, 0.0, 1.0, 1.0]),
    ]
    .iter()
    .flat_map(|((x, y), c)| vtx24(*x, *y, *c))
    .collect();

    let (exec, s) = run(&[
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kernel_words(),
        },
        draw_pipeline(1, None, 0xF, 0, 0, 24, 8),
        Cmd::CreateTexture(
            1,
            tex(
                4,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateBuffer(
            1,
            buf(
                verts.len() as u64,
                buffer_usage::VERTEX | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: verts,
        },
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
                Enc::SetVertexBuffer {
                    slot: 0,
                    buffer: 1,
                    offset: 0,
                },
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
    ]);
    let px = readback(&exec, &s, 1, 16);
    assert_eq!(
        &px[0..4],
        &[175, 16, 64, 255],
        "pixel0 barycentric gradient"
    );
    assert_eq!(
        &px[12..16],
        &[80, 112, 64, 255],
        "pixel3 barycentric gradient"
    );
}

// =================================================================================================
// 3. premultiplied source-over blend in LINEAR light — hand-computed.
// =================================================================================================

#[test]
fn blend_source_over_composites_against_the_destination() {
    // Clear to opaque blue [0,0,255,255], then draw a fullscreen triangle src=[1,0,0,0.5] with blend ENABLED.
    // out = src.rgb*a + dst.rgb*(1-a) = [0.5, 0, 0.5], out.a = a + dst.a*(1-a) = 1.0 -> [128, 0, 128, 255].
    let verts: Vec<u8> = [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)]
        .iter()
        .flat_map(|(x, y)| vtx24(*x, *y, [1.0, 0.0, 0.0, 0.5]))
        .collect();
    let blend = Some(BlendState {
        src_color: 1,
        dst_color: 0,
        op_color: 0,
        src_alpha: 1,
        dst_alpha: 0,
        op_alpha: 0,
    });
    let (exec, s) = run(&[
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kernel_words(),
        },
        draw_pipeline(1, blend, 0xF, 0, 0, 24, 8),
        Cmd::CreateTexture(
            1,
            tex(
                1,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateBuffer(
            1,
            buf(
                verts.len() as u64,
                buffer_usage::VERTEX | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: verts,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [0.0, 0.0, 1.0, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetVertexBuffer {
                    slot: 0,
                    buffer: 1,
                    offset: 0,
                },
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
    ]);
    assert_eq!(
        readback(&exec, &s, 1, 4),
        vec![128, 0, 128, 255],
        "red@0.5 over blue = purple"
    );
}

// =================================================================================================
// 4. write-mask must GATE which channels a draw reaches (a dropped mask is the exact #213 hazard).
// =================================================================================================

#[test]
fn write_mask_restricts_the_draw_to_enabled_channels() {
    // Clear to black [0,0,0,255], draw an opaque-WHITE fullscreen triangle (replace, no blend) with a
    // write_mask of R-only (0x1). Only R may change -> [255, 0, 0, 255]. A dropped mask would give white.
    let verts: Vec<u8> = [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)]
        .iter()
        .flat_map(|(x, y)| vtx24(*x, *y, [1.0, 1.0, 1.0, 1.0]))
        .collect();
    let scene = |mask: u32| {
        vec![
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::PtxKernel,
                spirv: kernel_words(),
            },
            draw_pipeline(1, None, mask, 0, 0, 24, 8),
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateBuffer(
                1,
                buf(
                    verts.len() as u64,
                    buffer_usage::VERTEX | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: verts.clone(),
            },
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
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 1,
                        offset: 0,
                    },
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
        ]
    };
    let (exec, s) = run(&scene(0x1));
    assert_eq!(
        readback(&exec, &s, 1, 4),
        vec![255, 0, 0, 255],
        "R-only mask keeps G/B from the black clear"
    );
    // Control: 0xF writes all channels -> full white.
    let (exec, s) = run(&scene(0xF));
    assert_eq!(
        readback(&exec, &s, 1, 4),
        vec![255, 255, 255, 255],
        "0xF mask writes every channel"
    );
    // G+B mask (0x6): only G,B change -> [0,255,255,255].
    let (exec, s) = run(&scene(0x6));
    assert_eq!(
        readback(&exec, &s, 1, 4),
        vec![0, 255, 255, 255],
        "GB mask keeps R from black + A from clear"
    );
}

// =================================================================================================
// 5. face cull × front-face — the fullscreen triangle is CCW-in-NDC (front under front_face=0/CCW).
// =================================================================================================

fn cull_scene_pixel(front_face: u32, cull: u32) -> [u8; 4] {
    // Fullscreen CCW triangle, opaque white (stride-12 -> default white). Black clear.
    let verts: Vec<u8> = [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)]
        .iter()
        .flat_map(|(x, y)| vtx12(*x, *y))
        .collect();
    let (exec, s) = run(&[
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kernel_words(),
        },
        draw_pipeline(1, None, 0xF, cull, front_face, 12, 0),
        Cmd::CreateTexture(
            1,
            tex(
                1,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateBuffer(
            1,
            buf(
                verts.len() as u64,
                buffer_usage::VERTEX | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: verts,
        },
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
                Enc::SetVertexBuffer {
                    slot: 0,
                    buffer: 1,
                    offset: 0,
                },
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
    ]);
    readback(&exec, &s, 1, 4).try_into().unwrap()
}

#[test]
fn cull_and_front_face_drop_exactly_the_intended_facing() {
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    // The triangle is CCW-in-NDC => FRONT under front_face=0 (CCW), BACK under front_face=1 (CW).
    // front_face=0 (CCW): triangle is FRONT.
    assert_eq!(cull_scene_pixel(0, 0), WHITE, "cull none -> drawn");
    assert_eq!(
        cull_scene_pixel(0, 1),
        BLACK,
        "cull FRONT drops the front triangle"
    );
    assert_eq!(
        cull_scene_pixel(0, 2),
        WHITE,
        "cull BACK keeps the front triangle"
    );
    // front_face=1 (CW): the same winding is now BACK.
    assert_eq!(
        cull_scene_pixel(1, 1),
        WHITE,
        "cull FRONT keeps the (now) back triangle"
    );
    assert_eq!(
        cull_scene_pixel(1, 2),
        BLACK,
        "cull BACK drops the (now) back triangle"
    );
}

// =================================================================================================
// 6. CopyBufferToTexture — the buffer bytes land in the texture's tight-packed plane, honoring the row stride.
// =================================================================================================

#[test]
fn copy_buffer_to_texture_lays_out_rows() {
    // 2x2 rgba8 texture <- 16 tight bytes. bytes_per_row=8 (2 texels * 4). Each texel is distinct so a
    // transposed / mis-strided copy would be caught.
    let src: Vec<u8> = (0..16u8).collect(); // texel(0,0)=0..4, (1,0)=4..8, (0,1)=8..12, (1,1)=12..16
    let (exec, s) = run(&[
        Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src.clone(),
        },
        Cmd::CreateTexture(
            1,
            tex(
                2,
                2,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 1,
                src_offset: 0,
                bytes_per_row: 8,
                dst: 1,
                mip: 0,
                width: 2,
                height: 2,
            }],
            signal: None,
        }),
    ]);
    assert_eq!(
        readback(&exec, &s, 1, 16),
        src,
        "buffer bytes copied verbatim into the tight texture plane"
    );
}

// =================================================================================================
// 7. CopyTextureToTexture — a 1x1 sub-region moves from src origin to dst origin, leaving the rest untouched.
// =================================================================================================

#[test]
fn copy_texture_to_texture_moves_only_the_named_region() {
    // src 2x2 cleared to red; dst 2x2 left zeroed. Copy the 1x1 texel at src(0,0) -> dst(1,1). Only dst
    // texel (1,1) becomes red; the other three stay [0,0,0,0].
    let (exec, s) = run(&[
        Cmd::CreateTexture(
            1,
            tex(
                2,
                2,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                2,
                2,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [1.0, 0.0, 0.0, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
                Enc::CopyTextureToTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d { x: 0, y: 0, z: 0 },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d { x: 1, y: 1, z: 0 },
                    extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                },
            ],
            signal: None,
        }),
    ]);
    let px = readback(&exec, &s, 2, 16);
    assert_eq!(
        &px[0..12],
        &[0u8; 12],
        "the three unaddressed texels stay zeroed"
    );
    assert_eq!(
        &px[12..16],
        &[255, 0, 0, 255],
        "dst texel (1,1) received the copied red texel"
    );
}

// =================================================================================================
// 8. BlitTexture — nearest SELECTS a texel; linear AVERAGES neighbors. A 2x1 -> 1x1 downscale distinguishes.
// =================================================================================================

fn blit_downscale(filter: Filter) -> [u8; 4] {
    // src 2x1: texel(0,0)=red, texel(1,0)=blue (populated via CopyBufferToTexture). Blit 2x1 -> dst 1x1.
    // Nearest: dst centre maps to src x=1 -> BLUE. Linear: samples between the two -> [128,0,128,255].
    let mut src = vec![0u8; 8];
    src[0..4].copy_from_slice(&[255, 0, 0, 255]); // red
    src[4..8].copy_from_slice(&[0, 0, 255, 255]); // blue
    let (exec, s) = run(&[
        Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src,
        },
        Cmd::CreateTexture(
            1,
            tex(
                2,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                1,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 8,
                    dst: 1,
                    mip: 0,
                    width: 2,
                    height: 1,
                },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 2,
                        height: 1,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    filter,
                },
            ],
            signal: None,
        }),
    ]);
    readback(&exec, &s, 2, 4).try_into().unwrap()
}

#[test]
fn blit_nearest_selects_and_linear_averages() {
    assert_eq!(
        blit_downscale(Filter::Nearest),
        [0, 0, 255, 255],
        "nearest picks the src texel the centre lands on (blue)"
    );
    assert_eq!(
        blit_downscale(Filter::Linear),
        [128, 0, 128, 255],
        "linear averages the two src texels (red+blue)"
    );
}

// =================================================================================================
// 9. ResolveTexture path — a multisample src averages its samples into the single-sample dst. No public
//    write path fills a multisample texture in the oracle, so the reachable case is the zero-sample
//    average (documented limitation); this pins that the resolve op DISPATCHES and produces the exact
//    per-sample mean (0) rather than erroring or leaving the dst untouched-but-garbage.
// =================================================================================================

#[test]
fn resolve_multisample_averages_samples_into_single_sample_dst() {
    let (exec, s) = run(&[
        Cmd::CreateTexture(
            1,
            tex_ms(1, 1, 4, TextureFormat::Rgba8Unorm, texture_usage::COPY_SRC),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                1,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::ResolveTexture {
                src: 1,
                src_sub: TextureSubresource::base(),
                src_origin: Origin3d::default(),
                dst: 2,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d::default(),
                extent: Extent3d {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
            }],
            signal: None,
        }),
    ]);
    // mean of four zero samples per channel = 0; the op ran (didn't error) and wrote the resolved plane.
    assert_eq!(
        readback(&exec, &s, 2, 4),
        vec![0, 0, 0, 0],
        "resolve wrote the per-sample mean into the dst"
    );
}
