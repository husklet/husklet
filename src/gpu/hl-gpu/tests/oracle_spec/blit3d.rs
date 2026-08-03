//! DEPTH-SPANNING `Enc::BlitTexture` on the CPU oracle — the reference side of the 352 `copy_and_blit`
//! cases the Vulkan surface refuses as `vkCmdBlitImage: 3D region`.
//!
//! Why the oracle first. `VkFormatFeatureFlags` is per FORMAT, not per image type, so there is no bit a
//! driver can withdraw to decline a 3D blit and no query through which an application could discover the
//! refusal. It is the one unannounceable hole in core Vulkan 1.0 here, which is why it is funded ahead of
//! larger-looking gaps — and why the reference has to be able to REPRESENT the case before the executor
//! is built against it. While both sides decline, a differential agrees by mutual refusal and establishes
//! nothing at all.
//!
//! Scope: `src_extent.depth == dst_extent.depth`, so every destination slice reads exactly one source
//! slice and no resampling happens along z. A z-SCALED blit is refused with a typed error rather than
//! served by nearest-slice selection — `VK_FILTER_LINEAR` filters trilinearly, and quietly substituting
//! point selection on the depth axis would read as a filtering difference rather than as a capability
//! this reference does not have.
//!
//! # Evidence
//!
//! Three of these were watched FAILING before the code existed: `an_unscaled_...`, `a_z_mirrored_...`
//! and `a_depth_scaled_...` all reported `Unsupported("software: 3D/depth-slice texture copy")` against
//! the pre-change tree, while `every_slice_...` and `the_mirrored_and_unmirrored_...` passed — which is
//! what makes those two usable as the instrument and the control rather than as further claims.
//!
//! That failure signature was SHARED, though: one refusal stood in front of every case, so watching it
//! says nothing about which rule each test actually guards. Each rule was therefore also reverted on its
//! own and the surviving test named. Every row was run; every row failed.
//!
//! | reverted rule | test that caught it |
//! | --- | --- |
//! | `Mirror::to_u32` drops bit 2 | `wire_adversarial::a_blit_mirror_round_trips_per_axis_...` |
//! | `BlitRect::of` defaults `inverted.z` to `false` (shim) | `a_mirrored_blit_region_keeps_its_flip_...` |
//! | `blit_texture` ignores `mirror.z` | `a_z_mirrored_depth_blit_reverses_the_slice_order_only` |
//! | `blit_texture` drops the destination plane base | `an_unscaled_...` + `a_z_mirrored_...` |
//! | `blit_texture` always samples source plane 0 | `an_unscaled_...` + `a_z_mirrored_...` |
//! | the depth-scale refusal is deleted | `a_depth_scaled_blit_is_refused_with_a_typed_error` |
//! | `check_depth_span_in_texture` is deleted | `an_overhanging_depth_span_is_refused_before_...` |
//!
//! The last row is the one worth reading. It initially SURVIVED, because the plane lookup one layer down
//! reports the same `OutOfBounds` and a test asserting only "the submit failed" passed with the
//! validation deleted. What the bound actually buys is WHEN the refusal happens — before any operation
//! in the command buffer runs — so the test had to assert that an earlier, legal blit in the same buffer
//! left no trace. A guard that is defended twice needs a test aimed at the difference between the two.

use super::*;
use hl_gpu::protocol::model::descriptor::Mirror;
use hl_gpu::BufferId;

/// Six distinct RGBA texels — one per (slice, x) of a 2x1x3 volume. No two agree on any channel, so a
/// slice landing in the wrong place names itself rather than aliasing onto a right answer.
fn texel(i: u8) -> [u8; 4] {
    [10 + i * 30, 200 - i * 25, 5 + i * 20, 255]
}

/// The 24-byte upload for a 2x1x3 `Rgba8Unorm` volume: slice `z` holds `texel(2z)`, `texel(2z + 1)`.
fn volume_bytes() -> Vec<u8> {
    (0..6u8).flat_map(texel).collect()
}

const W: u32 = 2;
const H: u32 = 1;
const D: u32 = 3;
/// Bytes in one 2x1 `Rgba8Unorm` slice.
const SLICE: usize = 8;

fn vol_tex(usage: u32) -> TextureDesc {
    tex3d(W, H, D, TextureFormat::Rgba8Unorm, usage)
}

/// Read slice `z` of texture `id` back through `CopyTextureToBufferRegion`.
///
/// This is the ONLY channel either backend has for observing a non-base plane: `read_texture` returns the
/// base plane alone, by design, because that is the shape the executor's own readback has. A `D3`
/// texture's slices are its planes, so the slice index rides in `sub.layer`.
fn read_slice(cmds: &[Cmd], id: u32, z: u32) -> Vec<u8> {
    let mut program = cmds.to_vec();
    program.push(Cmd::CreateBuffer(
        900,
        buf(
            SLICE as u64,
            buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
        ),
    ));
    program.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToBufferRegion {
            src: id,
            src_sub: TextureSubresource {
                mip: 0,
                layer: z,
                aspect: hl_gpu::protocol::model::enums::TextureAspect::All,
            },
            src_origin: Origin3d::default(),
            extent: Extent3d {
                width: W,
                height: H,
                depth: 1,
            },
            dst: 900,
            dst_offset: 0,
            bytes_per_row: 0,
            rows_per_image: 0,
        }],
        signal: None,
    }));
    let (exec, s) = run(&program);
    let mut out = vec![0u8; SLICE];
    exec.read_buffer(&s.resources, BufferId(900), 0, &mut out)
        .expect("the slice readback buffer is readable");
    out
}

/// Upload `volume_bytes()` into texture 1 (a 2x1x3 volume) and create texture 2 as an empty one.
fn seeded() -> Vec<Cmd> {
    vec![
        Cmd::CreateBuffer(1, buf(24, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: volume_bytes(),
        },
        Cmd::CreateTexture(
            1,
            vol_tex(texture_usage::COPY_DST | texture_usage::COPY_SRC),
        ),
        Cmd::CreateTexture(
            2,
            vol_tex(texture_usage::COPY_DST | texture_usage::COPY_SRC),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 1,
                src_offset: 0,
                bytes_per_row: SLICE as u32,
                dst: 1,
                mip: 0,
                width: W,
                height: H,
            }],
            signal: None,
        }),
    ]
}

fn blit(src_extent: Extent3d, dst_extent: Extent3d, mirror: Mirror) -> Enc {
    Enc::BlitTexture {
        src: 1,
        src_sub: TextureSubresource::base(),
        src_origin: Origin3d::default(),
        src_extent,
        dst: 2,
        dst_sub: TextureSubresource::base(),
        dst_origin: Origin3d::default(),
        dst_extent,
        filter: Filter::Nearest,
        mirror,
    }
}

fn volume(depth: u32) -> Extent3d {
    Extent3d {
        width: W,
        height: H,
        depth,
    }
}

// =================================================================================================
// The INSTRUMENT first. Everything below asserts on per-slice content, and an assertion read through a
// channel that cannot see a non-base slice would be measuring the channel.
// =================================================================================================

/// The upload and readback channels this file depends on address every slice, not just the base one.
///
/// Asserted before the blit tests use them, and asserted with DISTINCT content per slice, because both
/// halves fail the same silent way: an upload that wrote only plane 0 and a readback that always read
/// plane 0 would agree with each other on slice 0 and be blind to the rest. The three expectations
/// differ pairwise, so a channel stuck on any one plane fails two of the three.
#[test]
fn every_slice_of_a_volume_can_be_written_and_read_back() {
    let cmds = seeded();
    for z in 0..D {
        let expected: Vec<u8> = [texel((z * 2) as u8), texel((z * 2 + 1) as u8)].concat();
        assert_eq!(
            read_slice(&cmds, 1, z),
            expected,
            "slice {z} must hold the bytes uploaded for slice {z}"
        );
    }
}

// =================================================================================================
// The capability: a depth-spanning blit at equal depth.
// =================================================================================================

/// An UNSCALED depth-spanning blit copies slice `z` to slice `z`, for every slice.
///
/// The per-slice assertion is the whole point. A reference that accepted the region and wrote only the
/// base plane would pass a "did it error" test, pass a `read_texture` comparison, and be wrong about
/// every slice the operation exists to serve — and a differential built on it would then certify an
/// executor with the same hole.
#[test]
fn an_unscaled_depth_blit_copies_every_slice_to_the_matching_slice() {
    let mut cmds = seeded();
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![blit(volume(D), volume(D), Mirror::NONE)],
        signal: None,
    }));
    for z in 0..D {
        let expected: Vec<u8> = [texel((z * 2) as u8), texel((z * 2 + 1) as u8)].concat();
        assert_eq!(
            read_slice(&cmds, 2, z),
            expected,
            "destination slice {z} must hold source slice {z}"
        );
    }
}

/// `Mirror::z` reverses the slice order, and nothing else.
///
/// Without the bit a depth flip is inexpressible: `vkCmdBlitImage` states it by inverting a region's z
/// offsets, which the min/max normalization into an unsigned origin and extent discards. The x and y
/// texel order must be UNTOUCHED here — a flip implemented by reversing the whole byte range would
/// satisfy "the slices are reversed" and silently reverse the rows too.
#[test]
fn a_z_mirrored_depth_blit_reverses_the_slice_order_only() {
    let mut cmds = seeded();
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![blit(
            volume(D),
            volume(D),
            Mirror {
                z: true,
                ..Mirror::NONE
            },
        )],
        signal: None,
    }));
    for z in 0..D {
        let src = D - 1 - z;
        let expected: Vec<u8> = [texel((src * 2) as u8), texel((src * 2 + 1) as u8)].concat();
        assert_eq!(
            read_slice(&cmds, 2, z),
            expected,
            "with z mirrored, destination slice {z} must hold source slice {src}, in unreversed x order"
        );
    }
}

/// The control for the two tests above: their expectations are pairwise distinct.
///
/// Three slices of a mirrored blit and three of an unmirrored one give six expectations; the mirrored
/// middle slice is the unmirrored middle slice, and that coincidence is real (reversing three slices
/// fixes the centre), so exactly five distinct values are required. An implementation that ignored
/// `Mirror::z` would return the unmirrored plane for both, and the two OUTER slices would each fail.
#[test]
fn the_mirrored_and_unmirrored_slice_expectations_do_not_coincide() {
    let plain: Vec<Vec<u8>> = (0..D)
        .map(|z| [texel((z * 2) as u8), texel((z * 2 + 1) as u8)].concat())
        .collect();
    let flipped: Vec<Vec<u8>> = (0..D).rev().map(|z| plain[z as usize].clone()).collect();
    assert_ne!(
        plain[0], flipped[0],
        "the first slice must move under a z flip"
    );
    assert_ne!(
        plain[2], flipped[2],
        "the last slice must move under a z flip"
    );
    assert_eq!(
        plain[1], flipped[1],
        "the centre of an odd slice count is its own mirror; stated so it is not read as a defect"
    );
}

// =================================================================================================
// What stays refused, and why the refusal is typed rather than approximate.
// =================================================================================================

/// A z-SCALED blit is refused, not approximated.
///
/// Every destination slice would have to be resampled from more than one source slice, and under
/// `VK_FILTER_LINEAR` that resampling is trilinear. Serving it by picking a nearest source slice would
/// produce a plausible image that disagrees with any real driver, and the disagreement would read as a
/// filtering difference rather than as a capability this reference has not implemented — which is the
/// failure mode a reference exists to avoid.
#[test]
fn a_depth_scaled_blit_is_refused_with_a_typed_error() {
    let mut shrink = seeded();
    shrink.push(Cmd::Submit(CommandBuffer {
        encoder: vec![blit(volume(D), volume(1), Mirror::NONE)],
        signal: None,
    }));
    let err = try_run(&shrink).expect_err("a 3:1 depth reduction must be refused");
    assert!(
        matches!(err, hl_gpu::GpuError::Unsupported(m) if m.contains("depth-scaled blit")),
        "the refusal must name the depth scaling, not something incidental: {err:?}"
    );

    // The POSITIVE control. A refusal proves nothing without a path that otherwise works: if the volume
    // setup were broken, the scaled and unscaled cases would be refused identically and the assertion
    // above would be measuring the setup.
    let mut equal = seeded();
    equal.push(Cmd::Submit(CommandBuffer {
        encoder: vec![blit(volume(D), volume(D), Mirror::NONE)],
        signal: None,
    }));
    try_run(&equal).expect("the equal-depth blit these bytes and this volume describe must run");
}

/// A depth span that overhangs the volume is refused BEFORE the command buffer runs, not part-way in.
///
/// This is why the bound is checked in validation and not left to the plane lookup: the lookup would
/// also refuse it, and refusing it later is a different contract. Command-buffer validation is
/// all-or-nothing here, so an operation ordered before the bad blit must not take effect — and a test
/// that only asserted "the submit failed" would pass with the check deleted, because the plane lookup
/// reports the same error one layer down. The EARLIER operation is what separates the two, so it is
/// what this asserts.
#[test]
fn an_overhanging_depth_span_is_refused_before_the_buffer_runs() {
    // Run against a session this test keeps, so the destination can be inspected AFTER the refusal, and
    // seed it in a SEPARATE call so the volumes survive — `submit` is atomic over the whole `Cmd` list,
    // so a single failing call rolls the `CreateTexture`s back too and leaves nothing to inspect.
    // `try_run` discards its session and could only answer whether the submit failed, which is the half
    // that does not distinguish validation from a mid-execution bounds error.
    let mut exec = hl_gpu::CpuExecutor::new();
    let caps = exec.capabilities();
    let mut limits = hl_gpu::Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = hl_gpu::Session::new(
        limits,
        hl_gpu::GlobalLedger::unbounded(),
        Box::new(hl_gpu::FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &seeded()).expect("the volume setup must run");

    let bad = [Cmd::Submit(CommandBuffer {
        encoder: vec![
            // Legal, and would be visible in slice 0 of texture 2 if the buffer ran at all.
            blit(volume(1), volume(1), Mirror::NONE),
            // `origin.z + depth` is 1 + 3 = 4 against a 3-slice volume.
            Enc::BlitTexture {
                src: 1,
                src_sub: TextureSubresource::base(),
                src_origin: Origin3d { x: 0, y: 0, z: 1 },
                src_extent: volume(D),
                dst: 2,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d::default(),
                dst_extent: volume(D),
                filter: Filter::Nearest,
                mirror: Mirror::NONE,
            },
        ],
        signal: None,
    })];
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 0, &bad)
        .expect_err("a depth span past the last slice must be refused");
    assert!(
        matches!(err, hl_gpu::GpuError::OutOfBounds),
        "an overhanging depth span is a bounds error: {err:?}"
    );

    let mut slice0 = vec![0u8; SLICE];
    exec.read_texture(&s.resources, hl_gpu::TextureId(2), &mut slice0)
        .expect("the destination volume's base slice is readable");
    assert_eq!(
        slice0,
        vec![0u8; SLICE],
        "the legal blit ordered BEFORE the refused one must not have taken effect"
    );
}
