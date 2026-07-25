use super::*;

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
