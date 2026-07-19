//! DEMO — exhaustive `TextureFormat` coverage on the wgpu executor, proven with EXACT stored bytes.
//!
//! The srgb_target demo proved the two sRGB formats round-trip; the differential fuzzer and the rest of
//! the suite lean almost entirely on `Rgba8Unorm`. This binary closes the gap by sweeping EVERY format the
//! executor advertises in its `Capabilities::texture_formats` bitset and proving each one materializes and
//! reads back the exact bytes its layout demands — channel order (RGBA vs BGRA), gamma encoding (sRGB), and
//! float bit-patterns (f16/f32) all confronted, plus the two depth formats driven as real depth attachments.
//!
//! The formats the protocol advertises (`hl_gpu::protocol::model::capability`):
//!   COLOR_FORMATS: Rgba8Unorm, Bgra8Unorm, Rgba8Srgb, Bgra8Srgb, R8Unorm, Rg8Unorm, Rgba16Float,
//!                  Rgba32Float, R32Float
//!   DEPTH:         Depth32Float (DEPTH_FORMATS) + Depth24PlusStencil8 (this executor lowers stencil, so it
//!                  additionally advertises the combined depth+stencil format — see `capabilities_for`).
//!
//! METHOD (color): a fullscreen triangle whose fragment shader outputs a CONSTANT linear color
//! `C = (0.75, 0.5, 0.25, 1.0)` is drawn into a fresh target of each format, then the target is read back
//! RAW (`copy_texture_to_buffer` copies encoded texels verbatim — no decode) and the tight bytes are
//! asserted against the format's exact layout. This is the same proven gamma-on-write path srgb_target uses;
//! the ONLY variable is the target format, so any mishandling in `convert::texture_format` /
//! `convert::texel_bytes` / `texture::make_texture` / readback surfaces as wrong bytes.
//!
//! sRGB TRANSFER FUNCTION (IEC 61966-2-1 sRGB OETF — the encode wgpu/lavapipe applies on write to an sRGB
//! target; alpha is NEVER encoded):
//!     V = 12.92 · L                       for L ≤ 0.0031308
//!     V = 1.055 · L^(1/2.4) − 0.055       for L  > 0.0031308
//! For the swept color: sRGB8(0.75)=225, sRGB8(0.5)=188, sRGB8(0.25)=137 (each computed live below, ±2 for
//! lavapipe rounding). A naive "sRGB == Unorm" backend would instead store 191/128/64 — the wide gap is the
//! proof the encode actually happened.
//!
//! METHOD (sample/swizzle): a raw texel is uploaded into a 1×1 sampled texture of a BGRA / sRGB format and
//! sampled (nearest) into a LINEAR `Rgba8Unorm` target — proving BGR channels swizzle to RGB and sRGB texels
//! decode to linear on the sample, where a plain Unorm texel passes through untouched.
//!
//! METHOD (depth): each depth format backs a real depth attachment; three fullscreen draws at different
//! depths with a `LESS` test prove the NEAREST fragment occludes the farther ones regardless of draw order
//! (the control re-runs with the test forced `ALWAYS`, where the LAST-drawn — farthest — fragment wins).

mod common;
use common::*;

use hl_gpu::protocol::model::capability::{COLOR_FORMATS, DEPTH_FORMATS};
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    DepthAttachment, DepthState, RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat,
    Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// The constant LINEAR color every color-format draw emits. All three RGB components are exactly
/// representable in f16 AND f32 (0.75 = 3/4, 0.5, 0.25), so the float formats round-trip with zero error;
/// they are distinct so a channel-order bug (RGBA↔BGRA) cannot hide, and none is 0/1 so a dropped-channel
/// or clamp bug shows.
const C: [f32; 4] = [0.75, 0.5, 0.25, 1.0];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS_CONST: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(0.75, 0.5, 0.25, 1.0); }
"#;
// Sample a 1×1 texture at its center into a LINEAR target (used to prove BGR swizzle + sRGB decode).
const FS_SAMPLE: &str = r#"#version 460
layout(set = 0, binding = 0) uniform texture2D t;
layout(set = 0, binding = 1) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() { o = texture(sampler2D(t, s), vec2(0.5, 0.5)); }
"#;

// ---- transfer-function / packing references (independent of the executor under test) -------------------

/// Unorm8 encode with round-half-up — the WebGPU/Vulkan unorm store.
fn unorm8(l: f32) -> u8 {
    (l.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}
/// IEC 61966-2-1 sRGB OETF, then unorm8. The exact linear→sRGB8 encode a `*Srgb` target applies on write.
fn srgb8(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let v = if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0 + 0.5) as u8
}
/// Decode one IEEE-754 binary16 (half) to f32 — to confront the raw bytes an `Rgba16Float` target stores.
fn half_to_f32(h: u16) -> f32 {
    let sign = (h >> 15) & 1;
    let exp = ((h >> 10) & 0x1f) as i32;
    let mant = (h & 0x3ff) as f32;
    let mag = if exp == 0 {
        mant * 2f32.powi(-24) // subnormal
    } else if exp == 0x1f {
        f32::INFINITY
    } else {
        (1.0 + mant / 1024.0) * 2f32.powi(exp - 15)
    };
    if sign == 1 {
        -mag
    } else {
        mag
    }
}
fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le_f32_at(b: &[u8]) -> f32 {
    f32::from_le_bytes([b[0], b[1], b[2], b[3]])
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
fn ct(fmt: TextureFormat) -> ColorTargetState {
    ColorTargetState {
        format: fmt,
        blend: None,
        write_mask: 0xF,
    }
}

/// Draw the constant-`C` fullscreen triangle into a fresh 2×2 `fmt` target and return its RAW tight readback
/// (`width*height*bytes_per_texel(fmt)` bytes, no row padding).
fn draw_const(exec: &mut WgpuExecutor, fmt: TextureFormat) -> Vec<u8> {
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
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS_CONST),
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
    )
    .unwrap_or_else(|e| {
        panic!("format {fmt:?}: the constant-color draw must run cleanly, got {e:?}")
    });
    exec.read_texture(&s.resources, 1)
        .unwrap_or_else(|e| panic!("format {fmt:?}: readback failed: {e:?}"))
}

/// Upload raw `stored` bytes into a 1×1 `src_fmt` sampled texture, sample it (nearest, clamp) into a LINEAR
/// `Rgba8Unorm` target, and return the single readback pixel.
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
fn every_color_format_roundtrips_exact_stored_bytes() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return, // no adapter — skip like the rest of the suite
    };

    // The three exact 8-bit encodings of C, computed live from the transfer functions.
    let (ru, gu, bu, au) = (unorm8(C[0]), unorm8(C[1]), unorm8(C[2]), unorm8(C[3])); // 191,128,64,255
    let (rs, gs, bs) = (srgb8(C[0]), srgb8(C[1]), srgb8(C[2])); // 225,188,137

    for &fmt in COLOR_FORMATS {
        let raw = draw_const(&mut exec, fmt);
        // Two texels of the 2×2 target: byte-per-texel footprint.
        let bpt = fmt
            .bytes_per_texel()
            .expect("color format has a texel footprint");
        assert_eq!(
            raw.len(),
            2 * 2 * bpt,
            "format {fmt:?}: readback is width*height*bpt"
        );
        let t0 = &raw[..bpt]; // first texel — every texel is identical (constant draw)

        match fmt {
            TextureFormat::Rgba8Unorm => {
                assert!(
                    near_tol([t0[0], t0[1], t0[2], t0[3]], [ru, gu, bu, au], 2),
                    "Rgba8Unorm: RGBA order, got {t0:?}, want [{ru},{gu},{bu},{au}]"
                );
            }
            TextureFormat::Bgra8Unorm => {
                // Stored bytes are B,G,R,A — the swizzle vs RGBA is the whole point.
                assert!(
                    near_tol([t0[0], t0[1], t0[2], t0[3]], [bu, gu, ru, au], 2),
                    "Bgra8Unorm: stored bytes must be B,G,R,A = [{bu},{gu},{ru},{au}], got {t0:?}"
                );
            }
            TextureFormat::Rgba8Srgb => {
                // RGB gamma-encoded (225/188/137), alpha linear (255) — NOT the naive 191/128/64.
                assert!(
                    near_tol([t0[0], t0[1], t0[2], t0[3]], [rs, gs, bs, au], 2),
                    "Rgba8Srgb: gamma-encoded R,G,B = [{rs},{gs},{bs}] (linear A {au}), got {t0:?}"
                );
                assert!(t0[0] as i16 - ru as i16 >= 20,
                    "Rgba8Srgb R {} must be far LIGHTER than the naive Unorm {ru} — proves the OETF ran", t0[0]);
            }
            TextureFormat::Bgra8Srgb => {
                // gamma-encoded, B,G,R,A order.
                assert!(near_tol([t0[0], t0[1], t0[2], t0[3]], [bs, gs, rs, au], 2),
                    "Bgra8Srgb: stored bytes must be gamma B,G,R + linear A = [{bs},{gs},{rs},{au}], got {t0:?}");
            }
            TextureFormat::R8Unorm => {
                assert!(
                    (t0[0] as i16 - ru as i16).abs() <= 2,
                    "R8Unorm: single byte = unorm8(R) = {ru}, got {}",
                    t0[0]
                );
            }
            TextureFormat::Rg8Unorm => {
                assert!(
                    (t0[0] as i16 - ru as i16).abs() <= 2 && (t0[1] as i16 - gu as i16).abs() <= 2,
                    "Rg8Unorm: bytes = [unorm8(R),unorm8(G)] = [{ru},{gu}], got {:?}",
                    &t0[..2]
                );
            }
            TextureFormat::Rgba16Float => {
                let got = [
                    half_to_f32(le_u16(&t0[0..2])),
                    half_to_f32(le_u16(&t0[2..4])),
                    half_to_f32(le_u16(&t0[4..6])),
                    half_to_f32(le_u16(&t0[6..8])),
                ];
                for k in 0..4 {
                    assert!((got[k] - C[k]).abs() < 1e-3,
                        "Rgba16Float ch{k}: half-decoded {} must equal C {} (exactly f16-representable)", got[k], C[k]);
                }
            }
            TextureFormat::Rgba32Float => {
                let got = [
                    le_f32_at(&t0[0..4]),
                    le_f32_at(&t0[4..8]),
                    le_f32_at(&t0[8..12]),
                    le_f32_at(&t0[12..16]),
                ];
                assert_eq!(
                    got, C,
                    "Rgba32Float: f32 texels must be C exactly, got {got:?}"
                );
            }
            TextureFormat::R32Float => {
                let got = le_f32_at(&t0[0..4]);
                assert_eq!(got, C[0], "R32Float: single f32 = R = {}, got {got}", C[0]);
            }
            other => panic!("unhandled color format in sweep: {other:?} — add an assertion for it"),
        }

        // Human confrontation: dump an RGBA8 preview for the 8-bit formats (the float/single-channel ones
        // have no direct RGBA8 rendering, so only the 4-byte color formats get a PNG).
        if matches!(
            fmt,
            TextureFormat::Rgba8Unorm
                | TextureFormat::Bgra8Unorm
                | TextureFormat::Rgba8Srgb
                | TextureFormat::Bgra8Srgb
        ) {
            let name = format!("format_{fmt:?}");
            // Present bytes as-stored (BGRA previews will look channel-swapped — that is the proof).
            write_png(&name, 2, 2, &raw);
        }
        eprintln!(
            "format {fmt:?}: {bpt} B/texel, stored texel {:?} — exact round-trip OK",
            t0
        );
    }
}

// ==========================================================================================================
// SAMPLE: BGRA swizzles to RGBA, and sRGB decodes to linear, on the sample.
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

// ==========================================================================================================
// DEPTH: each advertised depth format backs a real depth attachment; nearest fragment occludes.
// ==========================================================================================================
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// VS emitting a fullscreen triangle at a baked constant clip-space `z` (w=1, so `gl_Position.z` is the
/// per-draw depth). FS emits a baked constant color. Distinct `z`/color per draw lets one pass prove occlusion.
fn depth_vs(z: f32) -> Vec<u32> {
    let src = format!(
        "#version 460\nvoid main() {{\n  vec2 p[3] = vec2[3](vec2(-1.0,-1.0), vec2(3.0,-1.0), vec2(-1.0,3.0));\n  gl_Position = vec4(p[gl_VertexIndex], {z:?}, 1.0);\n}}\n"
    );
    glsl(glsl_stage::VERTEX, "vmain", &src)
}
fn depth_fs(c: [u8; 4]) -> Vec<u32> {
    let src = format!(
        "#version 460\nlayout(location=0) out vec4 o;\nvoid main() {{ o = vec4({:?}, {:?}, {:?}, 1.0); }}\n",
        c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0
    );
    glsl(glsl_stage::FRAGMENT, "fmain", &src)
}

/// Run three fullscreen draws (green z=0.5, blue z=0.2, red z=0.8, in that order) through `ds_fmt` as the
/// depth attachment with the given depth `cmp`, and return the single readback pixel of the 1×1 color target.
fn depth_run(exec: &mut WgpuExecutor, ds_fmt: TextureFormat, cmp: u32) -> [u8; 4] {
    let mut s = new_session(exec);
    let ds = DepthState::depth_only(ds_fmt, /*depth_write*/ true, cmp);
    let pipe = |module_vs: u32, module_fs: u32| RenderPipelineDesc {
        vertex: ShaderRef {
            module: module_vs,
            entry: "vmain".into(),
        },
        fragment: Some(ShaderRef {
            module: module_fs,
            entry: "fmain".into(),
        }),
        vertex_buffers: vec![],
        color_targets: vec![ct(TextureFormat::Rgba8Unorm)],
        depth: Some(ds.clone()),
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count: 1,
        label: String::new(),
    };
    let draw = |pipe: u32| {
        vec![
            Enc::SetPipeline(pipe),
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
        ]
    };
    let mut enc = vec![Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: 1,
            load: LoadOp::Clear,
            clear: [0.0, 0.0, 0.0, 1.0],
            store: true,
        }],
        depth: Some(DepthAttachment {
            texture: 2,
            load: LoadOp::Clear,
            clear_depth: 1.0,
            clear_stencil: 0,
        }),
    }];
    enc.extend(draw(1)); // green z=0.5
    enc.extend(draw(2)); // blue  z=0.2 (nearest)
    enc.extend(draw(3)); // red   z=0.8 (farthest, drawn last)
    enc.push(Enc::EndRenderPass);

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
            Cmd::CreateTexture(2, tex(1, 1, ds_fmt, texture_usage::RENDER_TARGET)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_vs(0.5),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_fs(GREEN),
            },
            Cmd::CreateShader {
                id: 3,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_vs(0.2),
            },
            Cmd::CreateShader {
                id: 4,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_fs(BLUE),
            },
            Cmd::CreateShader {
                id: 5,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_vs(0.8),
            },
            Cmd::CreateShader {
                id: 6,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_fs(RED),
            },
            Cmd::CreateRenderPipeline(1, pipe(1, 2)),
            Cmd::CreateRenderPipeline(2, pipe(3, 4)),
            Cmd::CreateRenderPipeline(3, pipe(5, 6)),
            Cmd::Submit(CommandBuffer {
                encoder: enc,
                signal: None,
            }),
        ],
    )
    .unwrap_or_else(|e| panic!("depth {ds_fmt:?} cmp={cmp}: submit must run cleanly, got {e:?}"));
    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

#[test]
fn depth_formats_nearest_occludes_regardless_of_draw_order() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    for &ds_fmt in &[
        TextureFormat::Depth32Float,
        TextureFormat::Depth24PlusStencil8,
    ] {
        // LESS test: nearest (blue, z=0.2) survives; red (z=0.8, drawn LAST) is rejected as farther.
        let with_test = depth_run(&mut exec, ds_fmt, compare::LESS);
        assert!(near_tol(with_test, BLUE, 2),
            "{ds_fmt:?} as depth attachment (LESS): nearest fragment (blue z=0.2) must occlude the farther \
             ones regardless of draw order, got {with_test:?}");

        // Control: force the test ALWAYS → the LAST-drawn fragment (red, the FARTHEST) wins, proving the
        // depth test — not draw order — produced the blue result above.
        let no_test = depth_run(&mut exec, ds_fmt, compare::ALWAYS);
        assert!(near_tol(no_test, RED, 2),
            "{ds_fmt:?} (ALWAYS): with the depth test disabled the last-drawn fragment (red) must win, got {no_test:?}");

        assert_ne!(with_test, no_test,
            "{ds_fmt:?}: the LESS result must differ from the ALWAYS result — proof the format's depth test gated the draw");
        eprintln!("depth {ds_fmt:?}: LESS→nearest(blue) occludes; ALWAYS→last(red) — depth attachment works");
    }
}

// ==========================================================================================================
// The executor must advertise EXACTLY the formats these tests prove it handles.
// ==========================================================================================================
#[test]
fn executor_advertises_exactly_the_formats_this_suite_proves() {
    let exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };
    let advertised = exec.capabilities().texture_formats;

    // The set this file round-trips: all COLOR_FORMATS + Depth32Float (DEPTH_FORMATS) + Depth24PlusStencil8
    // (the combined depth+stencil format this stencil-lowering executor additionally advertises).
    let proven = TextureFormat::bits(COLOR_FORMATS)
        | TextureFormat::bits(DEPTH_FORMATS)
        | TextureFormat::bits(&[TextureFormat::Depth24PlusStencil8]);

    assert_eq!(advertised, proven,
        "the executor's advertised texture_formats bitset ({advertised:#b}) must equal EXACTLY the set this \
         suite round-trips ({proven:#b}) — any advertised-but-unproven (or proven-but-unadvertised) format is a bug");

    // And every advertised color-format bit maps to a real wgpu format + a texel footprint (no silent alias).
    for &fmt in COLOR_FORMATS {
        assert!(
            fmt.bytes_per_texel().is_some(),
            "{fmt:?}: advertised color format must have a texel footprint"
        );
    }
}
