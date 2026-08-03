//! Cross-format `Enc::CopyTextureToTexture` — the REINTERPRETING-copy proof.
//!
//! This file used to prove the opposite, and the reversal is the point. The executor routed a
//! format-mismatched copy through a converting blit, so one command meant "move the bytes" to the
//! software oracle and "resample and re-encode" here — different pixels for the same IR, measured at 133
//! against 39 in the first channel. Neither reading was wrong; the OPERATION was, and it has been split:
//! conversion is `BlitTexture`, which with equal extents and a nearest filter is exactly a converting
//! copy, and this operation reinterprets on both backends.
//!
//! The converting copy's stated justification did not survive checking. Its header named three GL entry
//! points — `glBlitFramebuffer`, `glCopyTexSubImage2D`, `glCopyImageSubData`. The latter two operate on
//! the GL model's CPU shadow and never emit this IR at all, and the first now chooses between the copy
//! and the blit on FORMAT as well as extent. No surface reaches the conversion, which is what made
//! removing it safe rather than merely coherent.
//!
//! These tests mint the IR directly and run it on the real `WgpuExecutor` (lavapipe on this headless
//! host), then read the destination back and assert EXACT pixels:
//!   * `rgba_to_bgra_copy_reinterprets_bytes_exact` — Rgba8Unorm → Bgra8Unorm moves the bytes unchanged,
//!     so reading the destination back through its own format shows the channels swapped. That is what
//!     `vkCmdCopyImage` requires and what the oracle has always done.
//!   * `cross_format_subregion_copy_reinterprets_and_leaves_rest_untouched` — the origin/extent
//!     sub-region is honoured; texels outside it survive.
//!   * `same_format_copy_is_exact_raw` — Rgba8Unorm → Rgba8Unorm is a byte-exact raw copy (fast path).
//!   * `srgb_variant_copy_stays_on_the_raw_fast_path` — Rgba8Unorm → Rgba8UnormSrgb (copy-compatible via
//!     the sRGB-suffix rule) preserves bytes exactly.
//!   * `a_copy_across_a_texel_size_change_is_refused` — R8Unorm → Rgba8Unorm is not a copy under any
//!     reading, and is the control proving the acceptances above are about SIZE and not about anything
//!     being mismatched.

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, Extent3d, Origin3d, TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, TextureDim, TextureFormat};
use hl_gpu::{Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const USAGE: u32 = texture_usage::SAMPLED
    | texture_usage::RENDER_TARGET
    | texture_usage::COPY_SRC
    | texture_usage::COPY_DST;

fn tex(w: u32, h: u32, format: TextureFormat) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format,
        usage: USAGE,
        label: String::new(),
    }
}

fn sub() -> TextureSubresource {
    TextureSubresource::base()
}

fn px(plane: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let o = ((y * w + x) * 4) as usize;
    [plane[o], plane[o + 1], plane[o + 2], plane[o + 3]]
}

fn new_session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1; // byte-addressable upload/readback
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

/// Full-extent copy of a fresh `src_fmt` source (uploaded `src_px`) into a fresh `dst_fmt` destination that
/// was first cleared to `dst_clear`. Returns the destination's tight readback plane (raw texel bytes in the
/// destination format's native channel order). Drives the whole runtime pipeline exactly as a guest would.
#[allow(clippy::too_many_arguments)]
fn copy_full(
    exec: &mut WgpuExecutor,
    w: u32,
    h: u32,
    src_fmt: TextureFormat,
    src_px: &[u8],
    dst_fmt: TextureFormat,
    dst_clear: [f32; 4],
) -> Vec<u8> {
    let mut s = new_session(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, tex(w, h, src_fmt)),
            Cmd::CreateTexture(2, tex(w, h, dst_fmt)),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: src_px.len() as u64,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src_px.to_vec(),
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
                        texture: 2,
                        x: 0,
                        y: 0,
                        w,
                        h,
                        color: dst_clear.map(f64::from),
                        base_array_layer: 0,
                        layer_count: 1,
                        mip_level: 0,
                    },
                    Enc::CopyTextureToTexture {
                        src: 1,
                        src_sub: sub(),
                        src_origin: Origin3d::default(),
                        dst: 2,
                        dst_sub: sub(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: w,
                            height: h,
                            depth: 1,
                        },
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("cross/same-format CopyTextureToTexture must lower + run cleanly");

    exec.read_texture(&s.resources, 2)
        .expect("read the copy destination")
}

/// Four distinct opaque RGBA texels, row-major: (0,0)=A (1,0)=B (0,1)=C (1,1)=D.
const A: [u8; 4] = [200, 30, 40, 255];
const B: [u8; 4] = [30, 200, 40, 255];
const C: [u8; 4] = [40, 30, 200, 255];
const D: [u8; 4] = [10, 220, 120, 200];

fn exec_or_skip() -> WgpuExecutor {
    WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor")
}

#[test]
fn rgba_to_bgra_copy_reinterprets_bytes_exact() {
    let mut exec = exec_or_skip();

    // Rgba8Unorm source: tight bytes are [r,g,b,a] per texel.
    let src: Vec<u8> = [A, B, C, D].concat();

    // Copy into a Bgra8Unorm destination. A copy REINTERPRETS, so the destination's bytes are the
    // source's bytes unchanged; reading them back through Bgra8Unorm is what makes the channels appear
    // swapped. The bytes are the invariant, not the colour.
    let out = copy_full(
        &mut exec,
        2,
        2,
        TextureFormat::Rgba8Unorm,
        &src,
        TextureFormat::Bgra8Unorm,
        [0.0; 4],
    );

    assert_eq!(
        px(&out, 2, 0, 0),
        A,
        "texel (0,0) keeps its bytes; a copy does not convert"
    );
    assert_eq!(px(&out, 2, 1, 0), B, "texel (1,0) keeps its bytes");
    assert_eq!(px(&out, 2, 0, 1), C, "texel (0,1) keeps its bytes");
    assert_eq!(px(&out, 2, 1, 1), D, "texel (1,1) keeps its bytes");
}

#[test]
fn cross_format_subregion_copy_reinterprets_and_leaves_rest_untouched() {
    let mut exec = exec_or_skip();

    // 2x2 Rgba8Unorm source; copy ONLY its bottom-right 1x1 texel (D) into the destination's top-left, so
    // the origin+extent sub-region path is exercised. The destination is pre-cleared to opaque red so every
    // texel the copy does NOT write is a KNOWN value.
    let src: Vec<u8> = [A, B, C, D].concat();
    let mut s = new_session(&exec);
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, tex(2, 2, TextureFormat::Rgba8Unorm)),
            Cmd::CreateTexture(2, tex(2, 2, TextureFormat::Bgra8Unorm)),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: src.len() as u64,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src.clone(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 1,
                        mip: 0,
                        width: 2,
                        height: 2,
                    },
                    // dst pre-cleared to opaque red → Bgra8 stores [0,0,255,255].
                    Enc::ClearRect {
                        texture: 2,
                        x: 0,
                        y: 0,
                        w: 2,
                        h: 2,
                        color: [1.0, 0.0, 0.0, 1.0],
                        base_array_layer: 0,
                        layer_count: 1,
                        mip_level: 0,
                    },
                    Enc::CopyTextureToTexture {
                        src: 1,
                        src_sub: sub(),
                        src_origin: Origin3d { x: 1, y: 1, z: 0 }, // source texel D
                        dst: 2,
                        dst_sub: sub(),
                        dst_origin: Origin3d { x: 0, y: 0, z: 0 }, // dest top-left
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("sub-region cross-format copy must run cleanly");
    let out = exec.read_texture(&s.resources, 2).expect("read dst");

    let red_bgra = [0u8, 0, 255, 255]; // the pre-clear, unchanged
    assert_eq!(
        px(&out, 2, 0, 0),
        D,
        "the copied 1x1 region is D's bytes, unchanged"
    );
    assert_eq!(
        px(&out, 2, 1, 0),
        red_bgra,
        "texel (1,0) must be untouched (pre-clear)"
    );
    assert_eq!(
        px(&out, 2, 0, 1),
        red_bgra,
        "texel (0,1) must be untouched (pre-clear)"
    );
    assert_eq!(
        px(&out, 2, 1, 1),
        red_bgra,
        "texel (1,1) must be untouched (pre-clear)"
    );
}

#[test]
fn same_format_copy_is_exact_raw() {
    let mut exec = exec_or_skip();

    // Same format → copy-compatible → the fast raw byte copy. The destination must be BYTE-IDENTICAL to the
    // source (no conversion path taken).
    let src: Vec<u8> = [A, B, C, D].concat();
    let out = copy_full(
        &mut exec,
        2,
        2,
        TextureFormat::Rgba8Unorm,
        &src,
        TextureFormat::Rgba8Unorm,
        [0.0; 4],
    );
    assert_eq!(
        out, src,
        "same-format copy must be byte-exact (raw fast path)"
    );
}

#[test]
fn srgb_variant_copy_stays_on_the_raw_fast_path() {
    let mut exec = exec_or_skip();

    // Rgba8Unorm → Rgba8UnormSrgb: same base format ignoring the sRGB suffix, so wgpu treats it as
    // COPY-COMPATIBLE. The executor keeps the raw byte copy (no transfer-function conversion) — the stored
    // bytes are preserved verbatim, exactly like a same-format copy.
    let src: Vec<u8> = [A, B, C, D].concat();
    let out = copy_full(
        &mut exec,
        2,
        2,
        TextureFormat::Rgba8Unorm,
        &src,
        TextureFormat::Rgba8Srgb,
        [0.0; 4],
    );
    assert_eq!(
        out, src,
        "sRGB-variant copy is copy-compatible → raw byte copy, bytes preserved"
    );
}

/// A copy across a TEXEL-SIZE change is refused — the control that keeps the acceptances above about
/// size rather than about mismatch in general.
///
/// `R8Unorm` into `Rgba8Unorm` is not a copy under any reading: there is no byte count that moves. It is
/// a converting operation, and `Enc::BlitTexture` is where it belongs — proven there by
/// `differential`'s `blit_cross_format`, which compares exactly this widening against real hardware.
#[test]
fn a_copy_across_a_texel_size_change_is_refused() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("adapter");
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let refused = hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, tex(2, 2, TextureFormat::R8Unorm)),
            Cmd::CreateTexture(2, tex(2, 2, TextureFormat::Rgba8Unorm)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
    )
    .expect_err("one byte per texel into four is not a copy");
    assert!(
        matches!(refused, hl_gpu::GpuError::Invalid(_)),
        "refused as invalid input, not as an unsupported capability: {refused:?}"
    );
}
