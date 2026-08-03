use super::*;

/// Draw the constant-`C` fullscreen triangle into a fresh 2×2 `fmt` target and return its RAW tight readback
/// (`width*height*bytes_per_texel(fmt)` bytes, no row padding).
pub(super) fn draw_const(exec: &mut WgpuExecutor, fmt: TextureFormat) -> Vec<u8> {
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

#[test]
fn every_color_format_roundtrips_exact_stored_bytes() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

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
            TextureFormat::Rgba16Unorm => {
                let got = [
                    le_u16(&t0[0..2]),
                    le_u16(&t0[2..4]),
                    le_u16(&t0[4..6]),
                    le_u16(&t0[6..8]),
                ];
                let expected = C.map(|value| (value * u16::MAX as f32 + 0.5) as u16);
                assert_eq!(got, expected, "Rgba16Unorm stores exact normalized u16 channels");
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
            TextureFormat::Rg32Float => {
                let got = [le_f32_at(&t0[0..4]), le_f32_at(&t0[4..8])];
                assert_eq!(
                    got,
                    C[..2],
                    "Rg32Float: f32 texel must store R and G exactly"
                );
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
