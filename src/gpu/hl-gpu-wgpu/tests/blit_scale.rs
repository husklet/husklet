//! Exact-pixel scaling-blit demo — the end-to-end proof that `Enc::BlitTexture` with a source extent that
//! DIFFERS from the destination extent (a scaling `glBlitFramebuffer`) is really lowered by the wgpu
//! executor. wgpu has no native image blit, so the executor RESAMPLES the source into the destination rect
//! with a textured-triangle draw (`src/blit.rs`); this test pins that resample to exact pixels.
//!
//! Two upscales, both blitting the FULL source over the FULL destination:
//!
//!   * NEAREST 2x2 → 4x4: each of the four distinct source texels must replicate into an exact 2x2 block
//!     (a 2x upscale with point sampling is pure texel replication — the sharpest possible exactness check).
//!   * LINEAR 2x2 (a horizontal A|B gradient) → 3x3: the ODD destination width places the middle column's
//!     pixel center exactly at texture u = 0.5, the midpoint between the two texel centers, so a correct
//!     linear filter returns EXACTLY the average (A+B)/2 there; the clamp-to-edge left/right columns return
//!     A and B. The asserted row is therefore `[A, (A+B)/2, B]`, exact.
//!
//! Both are the executed analogue of the CPU oracle's `blit_texture` (pixel-center sampling at
//! `src_origin + (d+0.5)*src_extent/dst_extent`). REGRESSION PROOF: this exact op returned
//! `GpuError::Unsupported("wgpu: BlitTexture (scaled/filtered) unimplemented")` before this change — a
//! scaling blit could not lower at all. The test asserts it now runs AND lands the right pixels, and that
//! the destination is not left untouched (the old silent-drop failure mode). Larger 64x64 upscales are
//! written to `/tmp/hl-demo/` as PNGs (nearest = crisp blocks, linear = a smooth gradient) for a visual
//! confrontation of the real resampled pattern.

use std::io::Write;

use hl_gpu::protocol::model::descriptor::{Extent3d, Origin3d, TextureDesc, TextureSubresource};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, Filter, TextureDim, TextureFormat,
};
use hl_gpu::{Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const RT: u32 = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST;

fn tex(w: u32, h: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: RT,
        label: String::new(),
    }
}

fn sub() -> TextureSubresource {
    TextureSubresource::base()
}

/// Upload `src_px` (tight `sw*sh*4` RGBA) into a fresh source texture, blit its FULL extent — scaled — into
/// a fresh `dw*dh` destination with `filter`, and return the destination's tight RGBA plane. Runs the whole
/// runtime pipeline (validate → account → dispatch → execute) exactly as a guest would.
fn blit_scaled(
    exec: &mut WgpuExecutor,
    sw: u32,
    sh: u32,
    src_px: &[u8],
    dw: u32,
    dh: u32,
    filter: Filter,
) -> Vec<u8> {
    assert_eq!(src_px.len() as u32, sw * sh * 4);
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1; // byte-addressable upload (the executor is stride-agnostic)
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, tex(sw, sh)),
            Cmd::CreateTexture(2, tex(dw, dh)),
            // Staging buffer holding the tight source pixels, uploaded into the source texture.
            Cmd::CreateBuffer(
                1,
                hl_gpu::protocol::model::descriptor::BufferDesc {
                    size: src_px.len() as u64,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer { id: 1, offset: 0, data: src_px.to_vec() },
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
                    // Pre-clear the destination to opaque black so any texel the blit does NOT write is a
                    // KNOWN value — makes the "the whole dest rect was written" check unambiguous.
                    Enc::ClearRect { texture: 2, x: 0, y: 0, w: dw, h: dh, color: [0.0, 0.0, 0.0, 1.0] },
                    // The scaling blit under test: FULL source extent → FULL destination extent.
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: sub(),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d { width: sw, height: sh, depth: 1 },
                        dst: 2,
                        dst_sub: sub(),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d { width: dw, height: dh, depth: 1 },
                        filter,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("a scaling BlitTexture must lower + run cleanly (it returned Unsupported before this change)");

    exec.read_texture(&s.resources, 2)
        .expect("read the blit destination")
}

fn px(plane: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let o = ((y * w + x) * 4) as usize;
    [plane[o], plane[o + 1], plane[o + 2], plane[o + 3]]
}

#[test]
fn scaling_blit_upscales_exact_pixels() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        // No adapter (no lavapipe/Vulkan ICD reachable) — skip, mirroring the suite's other gpu tests.
        Err(_) => return,
    };

    // ============================ NEAREST: 2x2 → 4x4, exact 2x2 blocks ============================
    // Four distinct source texels (row-major): (0,0)=A (1,0)=B (0,1)=C (1,1)=D.
    let a = [200u8, 30, 40, 255];
    let b = [30u8, 200, 40, 255];
    let c = [40u8, 30, 200, 255];
    let d = [200u8, 200, 30, 255];
    let src2x2: Vec<u8> = [a, b, c, d].concat();

    let near = blit_scaled(&mut exec, 2, 2, &src2x2, 4, 4, Filter::Nearest);

    // A 2x upscale with point sampling replicates each source texel into an exact 2x2 block: dest texel
    // (x,y) samples source (x/2, y/2). Assert ALL 16 pixels exactly.
    for y in 0..4u32 {
        for x in 0..4u32 {
            let want = match (x / 2, y / 2) {
                (0, 0) => a,
                (1, 0) => b,
                (0, 1) => c,
                (1, 1) => d,
                _ => unreachable!(),
            };
            assert_eq!(
                px(&near, 4, x, y),
                want,
                "NEAREST 2x2->4x4: dest ({x},{y}) must replicate source texel ({},{})",
                x / 2,
                y / 2
            );
        }
    }
    // Regression: the destination is NOT the black pre-clear anywhere — the blit actually wrote every texel
    // (the old code silently dropped / rejected the op, leaving the pre-cleared black, or erroring outright).
    assert!(
        near.chunks_exact(4).all(|p| p != [0, 0, 0, 255]),
        "every dest texel must carry a blitted source texel, not the black pre-clear (proves the scaling \
         blit was really executed, not silently dropped)"
    );

    // ======================= LINEAR: 2x2 horizontal gradient → 3x3, midpoint =======================
    // A pure horizontal gradient (both rows identical): left column = al, right column = bl. Chosen so the
    // per-channel average is an exact integer (all channel sums even) → the midpoint is exactly (al+bl)/2.
    let al = [40u8, 80, 120, 255];
    let bl = [200u8, 160, 120, 255];
    let mid = [120u8, 120, 120, 255]; // (40+200)/2, (80+160)/2, (120+120)/2, (255+255)/2
    let grad2x2: Vec<u8> = [al, bl, al, bl].concat(); // row0: al,bl ; row1: al,bl

    let lin = blit_scaled(&mut exec, 2, 2, &grad2x2, 3, 3, Filter::Linear);

    // dst width 3 (odd) ⇒ column pixel centers land at u = 1/6, 1/2, 5/6. u=1/2 is the exact midpoint
    // between the two texel centers (0.25, 0.75) ⇒ average; u=1/6 and 5/6 fall outside the texel-center span
    // ⇒ clamp-to-edge returns the edge texel. Every row is identical (the source is a pure H gradient).
    for y in 0..3u32 {
        assert_eq!(
            px(&lin, 3, 0, y),
            al,
            "LINEAR: left column (u=1/6, clamp) must be the left texel"
        );
        assert_eq!(
            px(&lin, 3, 1, y),
            mid,
            "LINEAR: middle column sits at exactly u=0.5 ⇒ the interpolated midpoint (al+bl)/2 = {mid:?}"
        );
        assert_eq!(
            px(&lin, 3, 2, y),
            bl,
            "LINEAR: right column (u=5/6, clamp) must be the right texel"
        );
    }

    // ============================ PNGs for visual confrontation ============================
    // Large upscales of the SAME sources so the pattern is legible: nearest = crisp 32x32 blocks, linear =
    // a smooth 64x64 gradient. These are real blit readbacks, written 1:1 (no host re-scaling).
    let near_big = blit_scaled(&mut exec, 2, 2, &src2x2, 64, 64, Filter::Nearest);
    write_png("/tmp/hl-demo/blit_scale.png", 64, 64, &near_big);
    let lin_big = blit_scaled(&mut exec, 2, 2, &grad2x2, 64, 64, Filter::Linear);
    write_png("/tmp/hl-demo/blit_scale_linear.png", 64, 64, &lin_big);

    // The big nearest upscale must still be crisp 32x32 quadrant blocks (no interpolation between texels).
    for y in 0..64u32 {
        for x in 0..64u32 {
            let want = match (x / 32, y / 32) {
                (0, 0) => a,
                (1, 0) => b,
                (0, 1) => c,
                (1, 1) => d,
                _ => unreachable!(),
            };
            assert_eq!(
                px(&near_big, 64, x, y),
                want,
                "NEAREST 2x2->64x64 block mismatch at ({x},{y})"
            );
        }
    }
    // The big linear upscale must be MONOTONIC across each row (a real gradient, not blocks): red rises
    // left→right from ~al[0] toward ~bl[0]. Adjacent samples never decrease.
    for y in 0..64u32 {
        for x in 1..64u32 {
            let l = px(&lin_big, 64, x - 1, y)[0];
            let r = px(&lin_big, 64, x, y)[0];
            assert!(
                r >= l,
                "LINEAR gradient must be non-decreasing L→R at row {y} col {x}: {l} then {r}"
            );
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Minimal dependency-free PNG writer (uncompressed/stored zlib) — copied from the stencil demo. Writes an
// EXACT-size RGBA image (no host re-scaling): the pixels are the real blit readback.
// -------------------------------------------------------------------------------------------------

fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) {
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::File::create(path) {
        Ok(mut f) => {
            let bytes = encode_png(w, h, rgba);
            let _ = f.write_all(&bytes);
            eprintln!("blit demo PNG written: {path} ({w}x{h})");
        }
        Err(e) => eprintln!("could not write {path}: {e}"),
    }
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((h * (1 + w * 4)) as usize);
    for y in 0..h {
        raw.push(0);
        let row = (y * w * 4) as usize;
        raw.extend_from_slice(&rgba[row..row + (w * 4) as usize]);
    }
    let idat = zlib_stored(&raw);

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA, deflate, no filter/interlace
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"IDAT", &idat);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut i = 0;
    while i < data.len() {
        let chunk = (data.len() - i).min(0xFFFF);
        let last = i + chunk >= data.len();
        out.push(if last { 1 } else { 0 });
        out.extend_from_slice(&(chunk as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk as u16)).to_le_bytes());
        out.extend_from_slice(&data[i..i + chunk]);
        i += chunk;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn write_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    png.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
