use super::*;

pub(super) fn gen_clear(seed: u64) -> Prog {
    let w = 3 + (seed % 6) as u32; // 3..=8
    let h = 2 + (seed % 5) as u32; // 2..=6
    let c = fcolor_opaque(seed);
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: c,
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "clear",
        ops: vec!["BeginRenderPass", "EndRenderPass"],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h * 4) as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (1) Upload a base plane, then `ClearRect` a sub-rectangle (clamped). Both write the same clamped rect
/// with the same packed colour. EXACT.
pub(super) fn gen_clear_rect(seed: u64) -> Prog {
    let w = 4 + (seed % 5) as u32; // 4..=8
    let h = 4 + (seed % 4) as u32; // 4..=7
    let base: Vec<u8> = (0..w * h).flat_map(|i| texel(seed ^ i as u64)).collect();
    let rx = (seed % w as u64) as u32;
    let ry = (seed % h as u64) as u32;
    let rw = 1 + (seed % 3) as u32;
    let rh = 1 + (seed % 3) as u32;
    let c = fcolor_opaque(seed.wrapping_add(9));
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateBuffer(
            1,
            buf(
                base.len() as u64,
                buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: base,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: w * 4,
                    dst: 1,
                    mip: 0,
                    width: w,
                    height: h,
                },
                Enc::ClearRect {
                    texture: 1,
                    x: rx,
                    y: ry,
                    w: rw,
                    h: rh,
                    color: c,
                    base_array_layer: 0,
                    layer_count: 1,
                    mip_level: 0,
                },
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "clear_rect",
        ops: vec!["CopyBufferToTexture", "ClearRect"],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h * 4) as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (2) `CopyBufferToTexture` of a deterministic plane (varying size), read back tight. EXACT.
pub(super) fn gen_copy_b2t(seed: u64) -> Prog {
    let w = 1 + (seed % 7) as u32; // 1..=7
    let h = 1 + (seed % 5) as u32; // 1..=5
    let data: Vec<u8> = (0..w * h)
        .flat_map(|i| texel(seed.wrapping_add(i as u64 * 3)))
        .collect();
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateBuffer(
            1,
            buf(
                data.len() as u64,
                buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 1,
                src_offset: 0,
                bytes_per_row: w * 4,
                dst: 1,
                mip: 0,
                width: w,
                height: h,
            }],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "copy_b2t",
        ops: vec!["CopyBufferToTexture"],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h * 4) as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (3) Seed a source texture, `CopyTextureToTexture` a sub-region into a fresh dest. EXACT.
pub(super) fn gen_copy_t2t(seed: u64) -> Prog {
    let w = 4u32;
    let h = 4u32;
    let src: Vec<u8> = (0..w * h)
        .flat_map(|i| texel(seed.wrapping_add(i as u64)))
        .collect();
    let ew = 1 + (seed % 3) as u32; // 1..=3
    let eh = 1 + (seed % 3) as u32;
    let sx = seed as u32 % (w - ew + 1);
    let sy = (seed / 2) as u32 % (h - eh + 1);
    let dx = (seed / 3) as u32 % (w - ew + 1);
    let dy = (seed / 5) as u32 % (h - eh + 1);
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateTexture(2, tex(w, h)),
        Cmd::CreateBuffer(
            1,
            buf(
                src.len() as u64,
                buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: w * 4,
                    dst: 1,
                    mip: 0,
                    width: w,
                    height: h,
                },
                Enc::CopyTextureToTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d { x: sx, y: sy, z: 0 },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d { x: dx, y: dy, z: 0 },
                    extent: Extent3d {
                        width: ew,
                        height: eh,
                        depth: 1,
                    },
                },
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "copy_t2t",
        ops: vec!["CopyBufferToTexture", "CopyTextureToTexture"],
        cmds,
        read: Read::Tex {
            id: 2,
            len: (w * h * 4) as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (4) `CopyBufferToBuffer`: write a src pattern, copy a sub-range into a dst, read the dst back. EXACT.
pub(super) fn gen_copy_b2b(seed: u64) -> Prog {
    let n = 16u64;
    let src: Vec<u8> = (0..n).map(|i| chan(seed, i)).collect();
    let size = 4 + (seed % 9); // 4..=12
    let so = seed % (n - size + 1);
    let doo = (seed / 2) % (n - size + 1);
    let cmds = vec![
        Cmd::CreateBuffer(1, buf(n, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::CreateBuffer(2, buf(n, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer {
                src: 1,
                src_offset: so,
                dst: 2,
                dst_offset: doo,
                size,
            }],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "copy_b2b",
        ops: vec!["CopyBufferToBuffer"],
        cmds,
        read: Read::Buf {
            id: 2,
            offset: 0,
            len: n as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (5) `FillBuffer`: write a base pattern, then memset a sub-range with a repeating 4-byte pattern. EXACT.
pub(super) fn gen_fill_buffer(seed: u64) -> Prog {
    let n = 16u64;
    let base: Vec<u8> = (0..n).map(|i| chan(seed.wrapping_add(1), i)).collect();
    let value = (seed.wrapping_mul(2654435761) & 0xFFFF_FFFF) as u32;
    let size = 4 + (seed % 9); // 4..=12 (may be non-multiple of 4 → partial tail pattern, both tile it)
    let off = seed % (n - size + 1);
    let cmds = vec![
        Cmd::CreateBuffer(1, buf(n, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: base,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::FillBuffer {
                buffer: 1,
                offset: off,
                size,
                value,
            }],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "fill_buffer",
        ops: vec!["FillBuffer"],
        cmds,
        read: Read::Buf {
            id: 1,
            offset: 0,
            len: n as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (6) `BlitTexture` NEAREST integer upscale (NxN → kN×kN). Point sampling of an integer upscale is pure
/// texel replication on both backends → EXACT.
pub(super) fn gen_blit_nearest(seed: u64) -> Prog {
    let n = 2 + (seed % 3) as u32; // 2..=4
    let k = 2 + (seed % 3) as u32; // 2..=4
    let dw = n * k;
    let dh = n * k;
    let src: Vec<u8> = (0..n * n)
        .flat_map(|i| texel(seed.wrapping_add(i as u64 * 7)))
        .collect();
    let cmds = blit_cmds(&src, n, n, dw, dh, Filter::Nearest);
    Prog {
        seed,
        category: "blit_nearest",
        ops: vec!["CopyBufferToTexture", "ClearRect", "BlitTexture"],
        cmds,
        read: Read::Tex {
            id: 2,
            len: (dw * dh * 4) as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (7) `BlitTexture` LINEAR upscale. Bilinear filtering agrees to a few unorm steps (pixel-centre +
/// clamp-to-edge, but not bit-identical between `sample_bilinear` and the hardware sampler). ±3.
pub(super) fn gen_blit_linear(seed: u64) -> Prog {
    let n = 2 + (seed % 2) as u32; // 2..=3
    let dw = 3 + (seed % 4) as u32; // 3..=6
    let dh = 3 + (seed % 3) as u32; // 3..=5
    let src: Vec<u8> = (0..n * n)
        .flat_map(|i| texel(seed.wrapping_add(i as u64 * 11)))
        .collect();
    let cmds = blit_cmds(&src, n, n, dw, dh, Filter::Linear);
    Prog {
        seed,
        category: "blit_linear",
        ops: vec!["CopyBufferToTexture", "ClearRect", "BlitTexture"],
        cmds,
        read: Read::Tex {
            id: 2,
            len: (dw * dh * 4) as usize,
        },
        tol: 3,
        kernel: None,
    }
}

/// Shared blit program body: upload `src` (tight) into tex 1, pre-clear tex 2 (dst) opaque black, then blit
/// the full source extent into the full destination extent with `filter`.
fn blit_cmds(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, filter: Filter) -> Vec<Cmd> {
    vec![
        Cmd::CreateTexture(1, tex(sw, sh)),
        Cmd::CreateTexture(2, tex(dw, dh)),
        Cmd::CreateBuffer(
            1,
            buf(
                src.len() as u64,
                buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src.to_vec(),
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: sw * 4,
                    dst: 1,
                    mip: 0,
                    width: sw,
                    height: sh,
                },
                Enc::ClearRect {
                    texture: 2,
                    x: 0,
                    y: 0,
                    w: dw,
                    h: dh,
                    color: [0.0, 0.0, 0.0, 1.0],
                    base_array_layer: 0,
                    layer_count: 1,
                    mip_level: 0,
                },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: sw,
                        height: sh,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: dw,
                        height: dh,
                        depth: 1,
                    },
                    filter,
                },
            ],
            signal: None,
        }),
    ]
}
