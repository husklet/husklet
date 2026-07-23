use super::*;

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
