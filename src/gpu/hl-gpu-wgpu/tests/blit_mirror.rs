//! MIRRORED `Enc::BlitTexture` on the wgpu executor — exact pixels, per axis.
//!
//! A mirrored blit is legal in both `glBlitFramebuffer` (a rect with `x1 < x0`) and `vkCmdBlitImage`
//! (`offsets[1]` before `offsets[0]`), and both express it by INVERTING a rect's bounds. The IR's origin
//! and extent are unsigned and could not say "flipped" at all, so the two surfaces normalised the rect
//! with a min/max and lost the intent — differently and silently. `Mirror` carries it, and the executor
//! serves it by putting the UV origin at the FAR edge of the source rect and negating the UV scale.
//!
//! The load-bearing claim is that the executor needs no new capability: the blit shader is a plain
//! `uv_off + uv * uv_scale` with nothing constraining the sign, and clamp-to-edge handles the boundary.
//! This test is that claim measured rather than argued.
//!
//! The source is ASYMMETRIC on both axes (a 3x2 plane of six distinct texels), blitted 1:1 with NEAREST so
//! the result is pure texel selection and can be asserted EXACTLY. No two of the four mirror states agree
//! on this source, which is what makes each assertion evidence: an executor that ignored `mirror` would
//! return the unmirrored plane four times and fail three of the four.

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, Extent3d, Mirror, Origin3d, TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, Filter, TextureDim, TextureFormat,
};
use hl_gpu::{Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 3;
const H: u32 = 2;

fn tex(w: u32, h: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST,
        label: String::new(),
    }
}

/// Source texel `i` in row-major order. Every channel of every texel differs from every other, so a wrong
/// reflection cannot alias onto a right one.
fn texel(i: u8) -> [u8; 4] {
    [10 + i * 40, 200 - i * 30, 5 + i * 20, 255]
}

fn source_plane() -> Vec<u8> {
    (0..(W * H) as u8).flat_map(texel).collect()
}

/// Destination texel `(x, y)` must hold source texel `(x', y')` reflected on exactly the mirrored axes.
fn expectation(mirror: Mirror) -> Vec<u8> {
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H as u8 {
        for x in 0..W as u8 {
            let sx = if mirror.x { (W as u8 - 1) - x } else { x };
            let sy = if mirror.y { (H as u8 - 1) - y } else { y };
            out.extend_from_slice(&texel(sy * W as u8 + sx));
        }
    }
    out
}

/// Upload the asymmetric source, pre-clear the destination to opaque black (so an untouched texel is a
/// KNOWN value rather than whatever the allocation held), blit 1:1 with `mirror`, and read the destination.
fn blit_mirrored(exec: &mut WgpuExecutor, mirror: Mirror) -> Vec<u8> {
    let src = source_plane();
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
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
            Cmd::CreateTexture(1, tex(W, H)),
            Cmd::CreateTexture(2, tex(W, H)),
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
                data: src,
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: W * 4,
                        dst: 1,
                        mip: 0,
                        width: W,
                        height: H,
                    },
                    Enc::ClearRect {
                        texture: 2,
                        x: 0,
                        y: 0,
                        w: W,
                        h: H,
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
                            width: W,
                            height: H,
                            depth: 1,
                        },
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: W,
                            height: H,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("a mirrored BlitTexture must lower and run; the Vulkan surface used to refuse it outright");

    let plane = exec
        .read_texture(&s.resources, 2)
        .expect("read the blit destination");
    assert!(
        plane.chunks_exact(4).all(|p| p != [0, 0, 0, 255]),
        "every destination texel must carry a blitted source texel, not the black pre-clear \
         (the pre-clear is what a silently-dropped blit would leave behind)"
    );
    plane
}

#[test]
fn mirrored_blit_reflects_each_axis_exactly() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let states = [
        Mirror::NONE,
        Mirror { x: true, y: false, z: false },
        Mirror { x: false, y: true, z: false },
        Mirror { x: true, y: true, z: false },
    ];

    // The four expectations must be pairwise distinct, or the assertions below would pass for an executor
    // that ignores `mirror`. Checked rather than assumed — an instrument with no power against the defect
    // it addresses is the failure mode this test exists to avoid.
    for i in 0..states.len() {
        for j in i + 1..states.len() {
            assert_ne!(
                expectation(states[i]),
                expectation(states[j]),
                "the {:?} and {:?} expectations must differ on this source",
                states[i],
                states[j]
            );
        }
    }

    for mirror in states {
        assert_eq!(
            blit_mirrored(&mut exec, mirror),
            expectation(mirror),
            "a {mirror:?} blit must reflect the source rect on exactly those axes"
        );
    }
}

#[test]
fn etc2_source_blits_through_native_sampler() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("wgpu adapter");
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
    let compressed = TextureDesc {
        width: 4, height: 4, depth: 1, mip_levels: 1, sample_count: 1,
        dim: TextureDim::D2, format: TextureFormat::Etc2Rgb8Unorm,
        usage: texture_usage::SAMPLED | texture_usage::COPY_DST, label: String::new(),
    };
    hl_gpu::runtime::submit(&mut session, &mut exec, 0, &[
        Cmd::CreateTexture(1, compressed),
        Cmd::CreateTexture(2, tex(4, 4)),
        Cmd::CreateBuffer(1, BufferDesc { size: 8, usage: buffer_usage::COPY_SRC, label: String::new() }),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![0; 8] },
        Cmd::Submit(CommandBuffer { encoder: vec![
            Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 8, dst: 1, mip: 0, width: 4, height: 4 },
            Enc::BlitTexture {
                src: 1, src_sub: TextureSubresource::base(), src_origin: Origin3d::default(),
                src_extent: Extent3d { width: 4, height: 4, depth: 1 },
                dst: 2, dst_sub: TextureSubresource::base(), dst_origin: Origin3d::default(),
                dst_extent: Extent3d { width: 4, height: 4, depth: 1 },
                filter: Filter::Nearest, mirror: Mirror::NONE,
            },
        ], signal: None }),
    ]).expect("ETC2 source blit must execute");
    let pixels = exec.read_texture(&session.resources, 2).expect("read destination");
    assert!(pixels.chunks_exact(4).all(|pixel| pixel == [2, 2, 2, 255]),
        "the all-zero ETC2 block decodes to exact RGB(2), proving sampler decode plus blit write");
}
