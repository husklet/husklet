//! `vkCmdBlitImage` over a DEPTH-SPANNING region — the recorder side of the 352
//! `dEQP-VK.api.copy_and_blit.core` cases that used to latch `Unsupported("vkCmdBlitImage: 3D region")`.
//!
//! That refusal was the driver's only UNANNOUNCEABLE gap. `VkFormatFeatureFlags` is per FORMAT, not per
//! image type, so there was no bit to withdraw and no query through which an application could have
//! discovered it — every other gap on the list could at least be made honest by advertising less. A 3D
//! blit is core Vulkan 1.0 and has to be served.
//!
//! What the recorder now judges, and what each rule is for:
//!
//! * a depth span must lie inside the image's depth at the named mip — the same bound x and y already had
//! * only a `VK_IMAGE_TYPE_3D` image has a depth axis to span; on any other, Vulkan fixes the z offsets
//!   at 0 and 1, so a wider span is the application's error
//! * the source and destination spans must be EQUAL, because the host serves one destination slice per
//!   source slice and has no trilinear filter — refused here, at record time, where the caller can
//!   attribute it, rather than as a host error later
//!
//! # Evidence
//!
//! The signature change that carries depth (`(u32, u32)` pairs to `Origin3d`/`Extent3d`) was landed
//! FIRST and on its own, with every existing hl-vulkan and shim test green, which is the control proving
//! it moved no behaviour. The three refusals below were then watched failing — the recorder accepted all
//! three regions and recorded them — before the rules existed.
//!
//! The two RECORDING assertions were written after the plumbing already carried depth, so they had never
//! been observed failing. Every rule was pinned by reverting the thing it guards. All seven rows below
//! were run and all seven failed:
//!
//! | reverted rule | test that caught it |
//! | --- | --- |
//! | recorder hardcodes `depth: 1` on the recorded extent | `a_3d_blit_records_the_whole_depth_span` |
//! | recorder hardcodes `z: 0` on the recorded origin | `a_3d_blit_records_the_whole_depth_span` |
//! | recorder hardcodes `Mirror::NONE` | `a_depth_flipped_3d_blit_carries_its_flip_to_the_ir` |
//! | the non-3D depth rule is deleted | `a_depth_span_on_a_non_3d_image_is_refused_by_name` |
//! | the equal-span rule is deleted | `a_depth_scaled_blit_is_refused_at_record_time` |
//! | the SOURCE depth bound is deleted | `a_depth_span_past_the_volume_is_out_of_bounds` |
//! | the DESTINATION depth bound is deleted | `a_depth_span_past_the_volume_is_out_of_bounds` |
//!
//! One row is absent on purpose. `Mirror::net` dropping its z axis was tried against
//! `a_depth_flipped_...` and SURVIVED — the recorder takes an already-combined `mirror` and never calls
//! `net`, which is the shim's job. That mutation was left uncovered by every test in the tree until a
//! z case was added to the shim's own `a_mirrored_blit_region_keeps_its_flip_and_an_empty_one_is_skipped`,
//! where it now fails. Worth recording because the wrong guess was plausible: the two live one call
//! apart, and a matrix written from the code's shape rather than from a run would have claimed it.

use super::*;
use hl_gpu::protocol::model::descriptor::Mirror;
use hl_gpu::protocol::model::enums::TextureDim;

const FMT: u32 = vk_format::R8G8B8A8_UNORM;

/// A `VK_IMAGE_TYPE_3D` image, `w x h x d`, one array layer, one mip.
fn volume(d: &mut Device, sink: &mut RecordingSink, w: u32, h: u32, depth: u32, usage: u32) -> u64 {
    create::create_image_geometry(d, sink, w, h, depth, 1, 1, TextureDim::D3, FMT, usage, 1)
        .expect("a 3D image is creatable")
}

/// An ordinary 2D image — the contrast case for the depth-axis rule.
fn flat(d: &mut Device, sink: &mut RecordingSink, w: u32, h: u32, usage: u32) -> u64 {
    create::create_image(d, sink, w, h, FMT, usage, 1).expect("a 2D image is creatable")
}

fn extent(width: u32, height: u32, depth: u32) -> Extent3d {
    Extent3d {
        width,
        height,
        depth,
    }
}

fn origin(x: u32, y: u32, z: u32) -> Origin3d {
    Origin3d { x, y, z }
}

/// A depth-spanning blit reaches the IR as ONE op carrying the whole span, not one op per slice.
///
/// One op per slice would also be correct output and is the wrong shape: `Enc::BlitTexture` has a depth
/// extent precisely so the executor can decide how to walk it, and a recorder that unrolled the axis
/// here would hide a z mirror (which is a property of the span, not of a slice) and would multiply the
/// command stream by the depth for no gain.
///
/// The origin is non-zero on every axis INCLUDING z, so an implementation that dropped the z origin
/// while keeping x and y fails here rather than passing on the two axes that already worked.
#[test]
fn a_3d_blit_records_the_whole_depth_span() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = volume(&mut d, &mut sink, 8, 8, 6, vk_image_usage::TRANSFER_SRC);
    let dst = volume(&mut d, &mut sink, 8, 8, 6, vk_image_usage::TRANSFER_DST);
    let (s, t) = (img_ir(&d, src), img_ir(&d, dst));

    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_blit_image(
            d,
            cb,
            src,
            dst,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            origin(1, 2, 3),
            extent(4, 4, 3),
            origin(0, 0, 1),
            extent(4, 4, 3),
            false,
            Mirror::NONE,
        )
        .expect("an equal-depth 3D blit inside both volumes must record");
    });

    assert_eq!(
        enc,
        vec![Enc::BlitTexture {
            src: s,
            src_sub: TextureSubresource::base(),
            src_origin: origin(1, 2, 3),
            src_extent: extent(4, 4, 3),
            dst: t,
            dst_sub: TextureSubresource::base(),
            dst_origin: origin(0, 0, 1),
            dst_extent: extent(4, 4, 3),
            filter: Filter::Nearest,
            mirror: Mirror::NONE,
        }],
        "the depth origin and extent must survive lowering on both sides"
    );
}

/// A depth flip reaches the IR as `Mirror::z`.
///
/// Vulkan states it by inverting a region's z offsets, and the recorder takes an already-normalized
/// origin and extent, so the flip has to arrive beside them or not at all. The x and y axes are
/// deliberately UNFLIPPED here, so a `Mirror::net` that collapsed the three axes together fails.
#[test]
fn a_depth_flipped_3d_blit_carries_its_flip_to_the_ir() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = volume(&mut d, &mut sink, 4, 4, 3, vk_image_usage::TRANSFER_SRC);
    let dst = volume(&mut d, &mut sink, 4, 4, 3, vk_image_usage::TRANSFER_DST);
    let flip_z = Mirror {
        z: true,
        ..Mirror::NONE
    };

    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_blit_image(
            d,
            cb,
            src,
            dst,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            origin(0, 0, 0),
            extent(4, 4, 3),
            origin(0, 0, 0),
            extent(4, 4, 3),
            false,
            flip_z,
        )
        .expect("a z-mirrored 3D blit is legal Vulkan and must record");
    });

    match enc.as_slice() {
        [Enc::BlitTexture { mirror, .. }] => assert_eq!(
            *mirror, flip_z,
            "the depth flip must reach the IR, and must not be read as an x or y flip"
        ),
        other => panic!("expected one BlitTexture, got {other:?}"),
    }
}

/// A depth span past the end of the volume is `OutOfBounds`, on either side.
///
/// x and y have had this bound since the recorder existed; z did not, because z could not be expressed.
/// Both sides are checked separately, because a bound applied to the source alone would let a
/// destination overhang through to the host.
#[test]
fn a_depth_span_past_the_volume_is_out_of_bounds() {
    for (side, src_origin, dst_origin) in [
        ("source", origin(0, 0, 2), origin(0, 0, 0)),
        ("destination", origin(0, 0, 0), origin(0, 0, 2)),
    ] {
        let mut d = dev();
        let mut sink = RecordingSink::with_full_caps();
        let src = volume(&mut d, &mut sink, 4, 4, 4, vk_image_usage::TRANSFER_SRC);
        let dst = volume(&mut d, &mut sink, 4, 4, 4, vk_image_usage::TRANSFER_DST);
        let cb = recording_cb(&mut d);
        // 2 + 3 = 5 against a 4-slice volume.
        let err = record::cmd_blit_image(
            &mut d,
            cb,
            src,
            dst,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            src_origin,
            extent(4, 4, 3),
            dst_origin,
            extent(4, 4, 3),
            false,
            Mirror::NONE,
        )
        .expect_err("a depth span past the last slice must be refused");
        assert!(
            matches!(err, GpuError::OutOfBounds),
            "{side}: an overhanging depth span is a bounds error, got {err:?}"
        );
    }

    // The POSITIVE control. A refusal proves nothing without a path that otherwise works: with the
    // volume or the usage bits wrong, the in-bounds and out-of-bounds spans would be refused alike.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = volume(&mut d, &mut sink, 4, 4, 4, vk_image_usage::TRANSFER_SRC);
    let dst = volume(&mut d, &mut sink, 4, 4, 4, vk_image_usage::TRANSFER_DST);
    let cb = recording_cb(&mut d);
    record::cmd_blit_image(
        &mut d,
        cb,
        src,
        dst,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        origin(0, 0, 1),
        extent(4, 4, 3),
        origin(0, 0, 1),
        extent(4, 4, 3),
        false,
        Mirror::NONE,
    )
    .expect("the same span one slice earlier fits exactly and must record");
}

/// Only a 3D image has a depth axis to span.
///
/// Vulkan fixes a non-3D region's z offsets at 0 and 1, so a depth greater than one on a 2D image is the
/// application's error and not a capability question. Refusing it by NAME matters: without this rule the
/// span would reach the bounds check and come back `OutOfBounds`, which reads as "your region is too
/// big" for a region that is not too big — it is on an image with no third axis at all.
#[test]
fn a_depth_span_on_a_non_3d_image_is_refused_by_name() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = flat(&mut d, &mut sink, 4, 4, vk_image_usage::TRANSFER_SRC);
    let dst = flat(&mut d, &mut sink, 4, 4, vk_image_usage::TRANSFER_DST);
    let cb = recording_cb(&mut d);
    let err = record::cmd_blit_image(
        &mut d,
        cb,
        src,
        dst,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        origin(0, 0, 0),
        extent(4, 4, 2),
        origin(0, 0, 0),
        extent(4, 4, 2),
        false,
        Mirror::NONE,
    )
    .expect_err("a 2D image has no depth axis to span");
    assert!(
        matches!(err, GpuError::Invalid(m) if m.contains("depth")),
        "the refusal must name the depth axis rather than the region size: {err:?}"
    );

    // The control: the SAME images and the SAME rect, one slice deep, record fine. Without this, the
    // assertion above would also pass against a recorder that refused every 2D blit.
    let cb = recording_cb(&mut d);
    record::cmd_blit_image(
        &mut d,
        cb,
        src,
        dst,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        origin(0, 0, 0),
        extent(4, 4, 1),
        origin(0, 0, 0),
        extent(4, 4, 1),
        false,
        Mirror::NONE,
    )
    .expect("the ordinary one-slice 2D blit must still record");
}

/// A z-SCALED blit is refused at RECORD time, by name.
///
/// The host serves one destination slice per source slice and has no trilinear filter, so an unequal
/// span cannot be served. Refusing here rather than letting it reach the host is the same choice this
/// recorder already makes for integer formats and for `VK_FILTER_LINEAR` on an unfilterable source: the
/// caller gets an answer naming what it passed, instead of a host error naming the host's internals.
///
/// Both directions are checked. A rule written as "destination deeper than source" would let a
/// reduction through, and a reduction is the harder half — it is the one that looks like it could be
/// served by dropping slices.
#[test]
fn a_depth_scaled_blit_is_refused_at_record_time() {
    for (what, src_depth, dst_depth) in [("magnify", 2u32, 4u32), ("reduce", 4, 2)] {
        let mut d = dev();
        let mut sink = RecordingSink::with_full_caps();
        let src = volume(&mut d, &mut sink, 4, 4, 4, vk_image_usage::TRANSFER_SRC);
        let dst = volume(&mut d, &mut sink, 4, 4, 4, vk_image_usage::TRANSFER_DST);
        let cb = recording_cb(&mut d);
        let err = record::cmd_blit_image(
            &mut d,
            cb,
            src,
            dst,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            origin(0, 0, 0),
            extent(4, 4, src_depth),
            origin(0, 0, 0),
            extent(4, 4, dst_depth),
            false,
            Mirror::NONE,
        )
        .expect_err("an unequal depth span must be refused");
        assert!(
            matches!(err, GpuError::Unsupported(m) if m.contains("depth")),
            "{what}: the refusal must name the depth scaling, got {err:?}"
        );
    }
}
