//! No texture shape may reach DEVICE VALIDATION as a colour attachment or a blit destination.
//!
//! `texture.rs` decides which shapes are granted `RENDER_ATTACHMENT`, and `submit/render.rs` and
//! `blit.rs` refuse an attachment that was not granted it by consulting a flag recorded FROM that grant.
//! Recording the flag from the grant is what stops the guard drifting from the rule — but it also couples
//! them in the other direction, and that direction is the dangerous one: **widening the grant silently
//! reopens the path the guard closes.** A guard keyed to the thing being widened dissolves exactly when
//! it is needed.
//!
//! That coupling was documented and unchecked, which is the state three float-filter constants were in
//! when they turned out to be one line from breaking. This binds it instead.
//!
//! The invariant is not "the guard exists". It is the observable one: for every texture shape this
//! executor can create, naming it as a colour attachment must produce either a clean run or the
//! executor's OWN typed refusal — never a device-validation error out of the pass. Measured, the failure
//! that appears the moment the grant is widened to 2D-array without teaching the attachment path to
//! check the bound view is `MissingFeatures(MULTIVIEW)` at `RenderPass::end`: a feature nobody asked for,
//! naming a capability rather than the mistake. The test needs no access to the private flag to catch
//! that, which is deliberate — it asserts what a caller can see.
//!
//! If you widen the grant, this fails, and the fix is not to relax it: the attachment path must refuse a
//! bound view that is not a single-layer 2D view, because that is what a colour pass can actually target.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, Extent3d, Mirror, Origin3d, TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{texture_usage, Filter, LoadOp, TextureDim, TextureFormat};
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuError};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const USAGE: u32 = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST;

/// One texture shape and whether the grant is expected to admit it as a render target.
struct Shape {
    what: &'static str,
    desc: TextureDesc,
    granted: bool,
}

fn shape(what: &'static str, granted: bool, desc: TextureDesc) -> Shape {
    Shape {
        what,
        desc,
        granted,
    }
}

fn base(format: TextureFormat) -> TextureDesc {
    TextureDesc {
        width: 4,
        height: 4,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format,
        usage: USAGE,
        label: String::new(),
    }
}

fn shapes() -> Vec<Shape> {
    vec![
        shape("plain 2D", true, base(TextureFormat::Rgba8Unorm)),
        shape(
            "multisampled 2D",
            true,
            TextureDesc {
                sample_count: 4,
                ..base(TextureFormat::Rgba8Unorm)
            },
        ),
        shape(
            "2D array",
            false,
            TextureDesc {
                depth: 3,
                ..base(TextureFormat::Rgba8Unorm)
            },
        ),
        shape(
            "cube",
            false,
            TextureDesc {
                dim: TextureDim::Cube,
                depth: 6,
                ..base(TextureFormat::Rgba8Unorm)
            },
        ),
        shape(
            "3D",
            false,
            TextureDesc {
                dim: TextureDim::D3,
                depth: 2,
                ..base(TextureFormat::Rgba8Unorm)
            },
        ),
        shape(
            "1D",
            false,
            TextureDesc {
                dim: TextureDim::D1,
                height: 1,
                ..base(TextureFormat::Rgba8Unorm)
            },
        ),
        shape(
            "block-compressed 2D",
            false,
            base(TextureFormat::Bc1RgbaUnorm),
        ),
    ]
}

/// Did the executor answer for itself, or did wgpu answer for it?
///
/// The distinction is the whole subject: a typed refusal names what the caller did, a device-validation
/// error names the shape of the API call that failed and reaches the caller as an opaque string.
fn classify(result: hl_gpu::Result<()>, what: &str, path: &str) -> bool {
    match result {
        Ok(()) => true,
        Err(GpuError::Invalid(m)) if m.contains("not created as a render target") => false,
        // A shape refused for a DIFFERENT honest reason of the executor's own is also fine — it is still
        // the executor answering. Only device validation is the failure this test is about.
        Err(GpuError::Unsupported(_)) | Err(GpuError::Invalid(_)) | Err(GpuError::OutOfBounds) => {
            false
        }
        Err(other) => panic!(
            "{what} as a {path} reached DEVICE VALIDATION instead of an answer from the executor: \
             {other:?}\n\n\
             This is the grant/guard coupling breaking. `submit::render` and `blit` refuse an attachment \
             by consulting a flag recorded from the RENDER_ATTACHMENT grant in `texture.rs`, so widening \
             that grant re-opens the path the guard closed — the guard is keyed to the thing being \
             widened. If you widened it, the attachment path must ALSO refuse a bound view that is not a \
             single-layer 2D view, which is what a colour pass can actually target; relaxing this \
             assertion instead restores an unattributable error (measured: MissingFeatures(MULTIVIEW) at \
             RenderPass::end, naming a feature nobody asked for)."
        ),
    }
}

#[test]
fn no_texture_shape_reaches_device_validation_as_an_attachment() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("adapter");
    let mut exercised = 0usize;
    let mut accepted_somewhere = false;

    for Shape {
        what,
        desc,
        granted,
    } in shapes()
    {
        // A shape this executor cannot CREATE is not evidence either way, and must not be readable as a
        // pass. It is counted separately and reported, never silently skipped into the green.
        let mut s = new_session(&exec);
        if hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[Cmd::CreateTexture(1, desc.clone())])
            .is_err()
        {
            eprintln!("NOT MEASURED: {what} cannot be created by this executor");
            continue;
        }
        exercised += 1;

        // As a COLOUR ATTACHMENT.
        let mut s = new_session(&exec);
        let pass = hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[
                Cmd::CreateTexture(1, desc.clone()),
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
                        Enc::EndRenderPass,
                    ],
                    signal: None,
                }),
            ],
        )
        .map(|_| ());
        let ran = classify(pass, what, "colour attachment");
        accepted_somewhere |= ran;
        assert_eq!(
            ran,
            granted,
            "{what}: expected the colour pass to {} — if the grant in texture.rs changed, this \
             expectation must change WITH the attachment path, not instead of it",
            if granted { "run" } else { "be refused" }
        );

        // As a BLIT DESTINATION, which renders into the texture and needs the same usage. A shape that
        // is refused as an attachment must be refused here too, or the two consumers of one grant have
        // drifted apart from each other rather than from the grant.
        let mut s = new_session(&exec);
        let extent = Extent3d {
            width: 4,
            height: 4,
            depth: 1,
        };
        let blit = hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[
                Cmd::CreateTexture(1, desc.clone()),
                Cmd::CreateTexture(2, base(TextureFormat::Rgba8Unorm)),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::BlitTexture {
                        src: 2,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        src_extent: extent,
                        dst: 1,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        dst_extent: extent,
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    }],
                    signal: None,
                }),
            ],
        )
        .map(|_| ());
        // Only the refusal direction is asserted: a granted shape may still be declined by the blit for
        // its own reasons (a multisampled destination cannot be rendered into by the blit path), and that
        // is the executor answering, which `classify` has already established.
        let blit_ran = classify(blit, what, "blit destination");
        if !granted {
            assert!(
                !blit_ran,
                "{what} is refused as a colour attachment but accepted as a blit destination — the two \
                 consumers of one grant have drifted apart"
            );
        }
    }

    // What would this test print if it were measuring nothing at all? Without these, a run in which every
    // shape failed to create — or in which the executor refused everything — would be indistinguishable
    // from a clean pass.
    assert!(
        exercised >= 4,
        "only {exercised} shapes could be created; this test has no power over the coupling it exists to \
         check and must not be read as green"
    );
    assert!(
        accepted_somewhere,
        "no shape was accepted as a colour attachment at all, so the refusals below prove nothing — the \
         render pass path itself is broken"
    );
}
