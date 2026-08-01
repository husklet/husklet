//! LAYERED textures: the reference materializes one plane per array layer, and serves exactly the
//! operations the executor serves on one.
//!
//! This class was outside the differential entirely. The reference refused to create an array texture
//! ("software: only 2D single-layer textures"), so no program using one could run on both backends and
//! any disagreement inside the class would have gone on reporting clean — the same coverage shape that
//! hid a mirrored blit, where the battery only compares what both sides were already known to handle.
//!
//! What the executor genuinely does with a layered texture was measured before any of this was written,
//! rather than decided from what the contract ought to be. It clears any layer range; it reads a named
//! layer out through a region copy; and it refuses a non-base subresource on texture-to-texture copy, on
//! blit, and on resolve, and refuses a layered render attachment outright. The reference now matches that
//! set on both sides of the line. Matching the refusals matters as much as matching the service: a
//! reference that accepted what the subject refuses is a false divergence in the other direction.

use super::*;
use hl_gpu::protocol::model::descriptor::Mirror;
use hl_gpu::protocol::model::enums::TextureAspect;
use hl_gpu::GpuError;

const RGBA: TextureFormat = TextureFormat::Rgba8Unorm;
const COPYABLE: u32 = texture_usage::COPY_SRC | texture_usage::COPY_DST;

fn layered(w: u32, h: u32, layers: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        depth: layers,
        ..tex(w, h, RGBA, usage)
    }
}

fn clear(texture: u32, color: [f32; 4], base_array_layer: u32, layer_count: u32) -> Enc {
    Enc::ClearRect {
        texture,
        x: 0,
        y: 0,
        w: 2,
        h: 2,
        color,
        base_array_layer,
        layer_count,
        mip_level: 0,
    }
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

/// A clear of a non-base layer must leave the base layer alone.
///
/// This is the whole point of layered storage, and it is observable through the ORDINARY readback
/// channel: both backends read back the base layer only (the executor's `copy_texture_to_buffer` is
/// issued with `depth_or_array_layers: 1`, and the reference matches that on purpose), so "wrote the
/// wrong layer" and "wrote every layer" both show up here as a base layer that changed when it should
/// not have.
///
/// The positive control is the first assertion: a clear that DOES name the base layer must change it, or
/// the second assertion would pass against a reference whose clears do nothing at all.
#[test]
fn a_clear_writes_the_layers_it_names_and_no_others() {
    let program = |base: u32, count: u32| {
        let (exec, s) = run(&[
            Cmd::CreateTexture(1, layered(2, 2, 3, COPYABLE)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![clear(1, RED, 0, 3), clear(1, GREEN, base, count)],
                signal: None,
            }),
        ]);
        readback(&exec, &s, 1, 16)[0..4].to_vec()
    };
    let red = vec![255u8, 0, 0, 255];
    let green = vec![0u8, 255, 0, 255];

    // Positive control: a range covering the base layer changes it.
    assert_eq!(program(0, 1), green, "a clear of layer 0 must write layer 0");
    assert_eq!(program(0, 3), green, "a clear of every layer includes layer 0");

    // The real assertion: a range that excludes the base layer must not touch it.
    assert_eq!(
        program(1, 1),
        red,
        "a clear of layer 1 alone must leave layer 0 holding its earlier value"
    );
    assert_eq!(
        program(1, 2),
        red,
        "a clear of layers 1..3 must leave layer 0 holding its earlier value"
    );
}

/// Each layer is a distinct plane: painting them in sequence must not have them alias.
#[test]
fn layers_do_not_alias_one_another() {
    let (exec, s) = run(&[
        Cmd::CreateTexture(1, layered(2, 2, 4, COPYABLE)),
        Cmd::Submit(CommandBuffer {
            // Paint every layer a different value, base layer LAST but one, so a texture whose layers
            // aliased would end up holding the final write rather than the base layer's own.
            encoder: vec![
                clear(1, GREEN, 1, 1),
                clear(1, RED, 0, 1),
                clear(1, GREEN, 2, 2),
            ],
            signal: None,
        }),
    ]);
    assert_eq!(
        readback(&exec, &s, 1, 16)[0..4],
        [255, 0, 0, 255],
        "layer 0 must hold its own clear, not a later write to layers 2..4"
    );
}

/// A layer range running past the materialized layers is refused, not clamped.
///
/// Writing fewer layers than asked is the silent-partial-work shape: the command would report success
/// having done part of its job, and nothing downstream could tell. The executor's own bounds reject it.
#[test]
fn a_layer_range_past_the_end_is_refused_rather_than_clamped() {
    let attempt = |base: u32, count: u32| {
        try_run(&[
            Cmd::CreateTexture(1, layered(2, 2, 2, COPYABLE)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![clear(1, RED, base, count)],
                signal: None,
            }),
        ])
    };
    // Positive control first: the whole range is legal and must succeed.
    assert!(attempt(0, 2).is_ok(), "the full layer range is legal");
    assert!(attempt(1, 1).is_ok(), "the last layer alone is legal");

    assert!(
        matches!(attempt(0, 3), Err(GpuError::OutOfBounds)),
        "a count running past the last layer is out of bounds"
    );
    assert!(
        matches!(attempt(2, 1), Err(GpuError::OutOfBounds)),
        "a base past the last layer is out of bounds"
    );
    assert!(
        matches!(attempt(0, 0), Err(GpuError::OutOfBounds)),
        "an empty layer range is not a silent no-op"
    );
}

/// The reference refuses a layered texture everywhere the EXECUTOR refuses one, and says so.
///
/// Each of these was measured on the executor directly rather than assumed: a non-base subresource is
/// `Unsupported` on texture-to-texture copy, on blit and on resolve, and a layered colour attachment is
/// refused because every render pass there binds the texture's default view, which for an array is a
/// `D2Array` no colour pass can target.
///
/// The reference could serve all four — its planes are addressable and its rasterizer could write any of
/// them — which is exactly why each refusal is explicit rather than incidental. Accepting what the
/// subject refuses would put a divergence in the differential that belongs to neither backend.
#[test]
fn the_reference_refuses_a_layered_texture_wherever_the_executor_does() {
    let base = TextureSubresource::base();
    let layer1 = TextureSubresource {
        mip: 0,
        layer: 1,
        aspect: TextureAspect::All,
    };
    let extent = Extent3d {
        width: 2,
        height: 2,
        depth: 1,
    };
    let setup = |usage: u32| {
        vec![
            Cmd::CreateTexture(1, layered(2, 2, 2, usage)),
            Cmd::CreateTexture(2, tex(2, 2, RGBA, COPYABLE | texture_usage::RENDER_TARGET)),
        ]
    };
    let attempt = |usage: u32, op: Enc| {
        let mut cmds = setup(usage);
        cmds.push(Cmd::Submit(CommandBuffer {
            encoder: vec![op],
            signal: None,
        }));
        try_run(&cmds)
    };

    // A non-base LAYER on a texture-to-texture copy.
    assert!(
        matches!(
            attempt(
                COPYABLE,
                Enc::CopyTextureToTexture {
                    src: 1,
                    src_sub: layer1,
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: base,
                    dst_origin: Origin3d::default(),
                    extent,
                }
            ),
            Err(GpuError::Unsupported(_))
        ),
        "a non-base layer copy is refused by the executor and must be refused here"
    );

    // A non-base LAYER on a blit.
    assert!(
        matches!(
            attempt(
                COPYABLE,
                Enc::BlitTexture {
                    src: 1,
                    src_sub: layer1,
                    src_origin: Origin3d::default(),
                    src_extent: extent,
                    dst: 2,
                    dst_sub: base,
                    dst_origin: Origin3d::default(),
                    dst_extent: extent,
                    filter: Filter::Nearest,
                    mirror: Mirror::NONE,
                }
            ),
            Err(GpuError::Unsupported(_))
        ),
        "a non-base layer blit is refused by the executor and must be refused here"
    );

    // A LAYERED colour attachment. The reference's rasterizer would happily write layer 0.
    let err = attempt(
        COPYABLE | texture_usage::RENDER_TARGET,
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: 1,
                load: LoadOp::Clear,
                clear: [0.0, 0.0, 0.0, 1.0],
                store: true,
            }],
            depth: None,
        },
    )
    .expect_err("a layered colour attachment must be refused");
    assert!(
        matches!(err, GpuError::Unsupported(m) if m.contains("layered render attachment")),
        "a layered colour attachment must be refused by name, got {err:?}"
    );

    // Control: the same pass against a SINGLE-LAYER texture must still run, or the refusal above would be
    // measuring a render pass that cannot open at all.
    assert!(
        try_run(&[
            Cmd::CreateTexture(2, tex(2, 2, RGBA, COPYABLE | texture_usage::RENDER_TARGET)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 2,
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
        ])
        .is_ok(),
        "a single-layer colour attachment must still open a render pass"
    );
}

/// A multisampled texture cannot also be layered, because the executor cannot create one.
///
/// Measured on the executor: wgpu rejects the creation with "Multisampled texture depth or array layers
/// must be 1". This reference could allocate it perfectly well, which is why the refusal is explicit —
/// a texture only one backend can hold is a program the differential can never run.
#[test]
fn a_multisampled_texture_cannot_be_layered() {
    let msaa = |samples: u32, layers: u32| {
        try_run(&[Cmd::CreateTexture(
            1,
            TextureDesc {
                sample_count: samples,
                ..layered(2, 2, layers, texture_usage::RENDER_TARGET)
            },
        )])
    };
    // Positive controls: each on its own is fine, so the refusal is about the COMBINATION.
    assert!(msaa(4, 1).is_ok(), "a multisampled single-layer texture is fine");
    assert!(msaa(1, 4).is_ok(), "a layered single-sampled texture is fine");

    let err = msaa(4, 2).expect_err("multisampled and layered together is refused");
    assert!(
        matches!(err, GpuError::Unsupported(m) if m.contains("cannot be layered")),
        "the combination must be refused by name, got {err:?}"
    );
}

/// 1D, 3D and cube textures stay refused, and the message says why rather than restating the shape.
///
/// They are not blocked by storage — the plane is layer-major and a depth slice or a cube face would sit
/// in it identically. They are blocked by there being no operation to agree with: the executor refuses a
/// non-base subresource on every copy, blit and resolve, so a 3D or cube texture the reference
/// materialized could not be exercised against it in the first place.
#[test]
fn the_dimensions_with_no_shared_operation_stay_refused() {
    // Each descriptor must be VALID for its dimension, or the refusal under test is not the one that
    // fires: a cube with one face is refused by residency accounting as a malformed cube
    // (`invalid cube texture shape`) long before the executor sees it, which would make this test pass
    // while measuring something else entirely.
    for (dim, height, layers) in [
        (TextureDim::D1, 1, 1),
        (TextureDim::D3, 2, 2),
        (TextureDim::Cube, 2, 6),
    ] {
        let err = try_run(&[Cmd::CreateTexture(
            1,
            TextureDesc {
                dim,
                height,
                ..layered(2, 2, layers, COPYABLE)
            },
        )])
        .expect_err("only 2D is materialized");
        assert!(
            matches!(err, GpuError::Unsupported(m) if m.contains("only 2D textures")),
            "{dim:?} must be refused with the reason, got {err:?}"
        );
    }
    // Control: 2D, layered and not, is accepted — so the refusals above are about the dimension.
    assert!(try_run(&[Cmd::CreateTexture(1, layered(2, 2, 1, COPYABLE))]).is_ok());
    assert!(try_run(&[Cmd::CreateTexture(1, layered(2, 2, 4, COPYABLE))]).is_ok());
}
