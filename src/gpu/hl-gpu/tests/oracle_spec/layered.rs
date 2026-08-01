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
use hl_gpu::protocol::model::descriptor::TextureViewDesc;
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

/// A full-extent base-plane clear sized to whatever shape `desc` is.
fn clear_of(desc: &TextureDesc, color: [f32; 4]) -> Enc {
    Enc::ClearRect {
        texture: 1,
        x: 0,
        y: 0,
        w: desc.width,
        h: desc.height,
        color,
        base_array_layer: 0,
        layer_count: 1,
        mip_level: 0,
    }
}
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
        matches!(err, GpuError::Unsupported(m) if m.contains("render attachment")),
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

/// EVERY dimension is materialized, and each is refused exactly where the executor refuses it.
///
/// This test used to assert the opposite, and its premise was mine. It said 1D, 3D and cube stay refused
/// because the executor had no operation on them for this reference to agree with — true when written,
/// and re-measured on current code it is false. The executor serves `ClearRect`, both buffer↔texture
/// copies and a texture-to-texture copy on all three, and its readback returns their base slice, so the
/// class is comparable. A belief encoded in a test reads as authoritative to whoever audits it next,
/// which is why re-measuring rather than trusting the note was worth one probe.
///
/// The blit is the single operation that does NOT extend to every dimension: it resamples by rendering
/// through a 2D view, so the executor declines 1D and 3D on either side — and a cube face IS a 2D layer
/// there, so cube blits fine and is deliberately absent from that refusal. Getting the exception wrong
/// in either direction is a divergence: refusing cube would decline what the subject performs, and
/// allowing 1D would perform what the subject declines.
#[test]
fn every_dimension_is_materialized_and_refused_where_the_executor_refuses_it() {
    let shape = |dim: TextureDim, w: u32, h: u32, depth: u32| TextureDesc {
        dim,
        width: w,
        height: h,
        depth,
        ..layered(w, h, depth, COPYABLE | texture_usage::RENDER_TARGET)
    };
    let cases = [
        ("1D", shape(TextureDim::D1, 4, 1, 1)),
        ("3D", shape(TextureDim::D3, 2, 2, 3)),
        ("cube", shape(TextureDim::Cube, 2, 2, 6)),
        ("2D array", shape(TextureDim::D2, 2, 2, 3)),
        ("plain 2D", shape(TextureDim::D2, 2, 2, 1)),
    ];

    for (what, desc) in &cases {
        // Created, and its base plane clearable — the operation both backends serve on every shape.
        let (exec, s) = run(&[
            Cmd::CreateTexture(1, desc.clone()),
            Cmd::Submit(CommandBuffer {
                encoder: vec![clear_of(desc, RED)],
                signal: None,
            }),
        ]);
        let plane = (desc.width * desc.height * 4) as usize;
        assert_eq!(
            readback(&exec, &s, 1, plane)[0..4],
            [255, 0, 0, 255],
            "{what}: the base plane must be clearable and readable"
        );
    }

    // A RENDER ATTACHMENT is 2D and single-layer, by dimension and not merely by plane count: a 1D
    // texture has exactly one plane and is still not a colour target on the executor, so a layer-count
    // test alone would let it through.
    for (what, desc) in &cases {
        let attempt = try_run(&[
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
        ]);
        let is_plain_2d = desc.dim == TextureDim::D2 && desc.depth <= 1;
        assert_eq!(
            attempt.is_ok(),
            is_plain_2d,
            "{what}: only a plain 2D texture is a colour attachment, got {attempt:?}"
        );
    }

    // The BLIT declines 1D and serves the rest — the one operation that is not uniform across
    // dimensions.
    //
    // `D3` moved out of this refusal on 2026-08-01 and the change is DELIBERATELY not symmetric with
    // the executor, which still answers "wgpu: 1D/3D blit source". A 3D blit is core Vulkan 1.0 with no
    // format bit to withdraw and no query to decline it through, so it has to be served; the reference
    // has to be able to represent it before the executor can be validated against it, because two sides
    // that both refuse agree by mutual refusal and prove nothing. `oracle_spec/blit3d.rs` holds the
    // per-slice assertions. When the executor learns the same operation, its own dimension test is the
    // one that closes this gap — not this line.
    for (what, desc) in &cases {
        let extent = Extent3d {
            width: desc.width,
            height: desc.height,
            depth: 1,
        };
        let attempt = try_run(&[
            Cmd::CreateTexture(1, desc.clone()),
            Cmd::CreateTexture(2, shape(TextureDim::D2, desc.width, desc.height, 1)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: extent,
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: extent,
                    filter: Filter::Nearest,
                    mirror: Mirror::NONE,
                }],
                signal: None,
            }),
        ]);
        let blittable = !matches!(desc.dim, TextureDim::D1);
        assert_eq!(
            attempt.is_ok(),
            blittable,
            "{what}: 1D has no 2D view to render through and is declined; 3D is served per slice and a \
             cube FACE is an ordinary 2D layer, got {attempt:?}"
        );
    }

    // Shape rules, which are the executor's. A wrong one would not refuse a legal texture — it would
    // allocate a different number of planes from the subject for the same descriptor.
    assert!(
        try_run(&[Cmd::CreateTexture(1, shape(TextureDim::D1, 4, 2, 1))]).is_err(),
        "a 1D texture with height != 1 is refused"
    );
    assert!(
        try_run(&[Cmd::CreateTexture(1, shape(TextureDim::Cube, 2, 3, 6))]).is_err(),
        "a cube with non-square faces is refused"
    );
    assert!(
        try_run(&[Cmd::CreateTexture(1, shape(TextureDim::Cube, 2, 2, 4))]).is_err(),
        "a cube whose face count is not a multiple of six is refused"
    );
}


/// A texture VIEW is refused, because this reference cannot alias and a snapshot is worse than nothing.
///
/// The base view — whole mip, whole layer — used to be accepted by cloning the texture into the view's
/// id. Measured against the executor on one program (clear the texture red, clear THROUGH a base view
/// green, read the texture): the executor reports green because the view names the same image; this
/// reference reported red, because the write landed in a copy. That is a live disagreement in an
/// advertised path, and the differential could not see it because no generator emits a view.
///
/// The non-base refusal is asserted separately, because it is the narrower and older statement and must
/// stay distinguishable — collapsing the two would lose the fact that subresource selection is a
/// different missing thing from aliasing.
///
/// Retirement condition, so this is checkable rather than folklore: a faithful view needs two ids to name
/// one object, and to keep that object alive while either id lives (the executor's view holds a reference
/// to the texture, so destroying the texture does not invalidate it). That contradicts the
/// singular-ownership rule this executor's storage is built on — one id, one object — so it is a change
/// to the resource model, not to the view arm. When the resource table can express shared ownership,
/// this refusal becomes an alias and the test below becomes a round-trip.
#[test]
fn a_texture_view_is_refused_rather_than_modelled_as_a_copy() {
    let base_view = TextureViewDesc {
        texture: 1,
        dim: TextureDim::D2,
        format: RGBA,
        aspect: TextureAspect::All,
        base_mip: 0,
        mip_count: 1,
        base_layer: 0,
        layer_count: 1,
    };
    let attempt = |view: TextureViewDesc| {
        try_run(&[
            Cmd::CreateTexture(1, layered(2, 2, 1, COPYABLE)),
            Cmd::CreateTextureView(2, view),
        ])
    };

    // Positive control: the texture alone is created happily, so the refusals are about the VIEW.
    assert!(try_run(&[Cmd::CreateTexture(1, layered(2, 2, 1, COPYABLE))]).is_ok());

    let err = attempt(base_view.clone()).expect_err("a base view must be refused, not cloned");
    assert!(
        matches!(err, GpuError::Unsupported(m) if m.contains("a view aliases its texture")),
        "the base view must be refused for the reason that makes it wrong, got {err:?}"
    );

    // The narrower, older refusal is still its own answer.
    let err = attempt(TextureViewDesc {
        base_layer: 1,
        ..base_view
    })
    .expect_err("a non-base view must be refused");
    assert!(
        matches!(err, GpuError::Unsupported(m) if m.contains("subresource views")),
        "a non-base view keeps the narrower message, got {err:?}"
    );
}

/// A MULTISAMPLED texture cannot take part in a blit, and both backends say so.
///
/// This reference has refused the pair from the start. The executor did not check at all until a test
/// binding its attachment grant to its guards was written and failed on its first run: a multisampled
/// destination reached `RenderPass::end` as `IncompatibleSampleCount`, naming the blit pipeline rather
/// than the texture the caller passed. The reference was the correct side and nothing recorded the
/// agreement, so this pins it from here.
///
/// Multisampled content reaches a blit only after `ResolveTexture` has made it single-sampled, which is
/// that operation's entire purpose.
#[test]
fn a_multisampled_texture_is_refused_by_a_blit_on_both_sides() {
    // Both copy usages, or the destination case is refused for lacking COPY_DST before the sample count
    // is ever consulted — a refusal that looks identical to the one under test if only `is_err` were
    // asserted. Asserting the REASON is what surfaced it.
    let msaa = TextureDesc {
        sample_count: 4,
        ..layered(2, 2, 1, COPYABLE | texture_usage::RENDER_TARGET)
    };
    let plain = layered(2, 2, 1, COPYABLE | texture_usage::RENDER_TARGET);
    let extent = Extent3d {
        width: 2,
        height: 2,
        depth: 1,
    };
    let blit = |src: u32, dst: u32| Enc::BlitTexture {
        src,
        src_sub: TextureSubresource::base(),
        src_origin: Origin3d::default(),
        src_extent: extent,
        dst,
        dst_sub: TextureSubresource::base(),
        dst_origin: Origin3d::default(),
        dst_extent: extent,
        filter: Filter::Nearest,
        mirror: Mirror::NONE,
    };
    let attempt = |op: Enc| {
        try_run(&[
            Cmd::CreateTexture(1, msaa.clone()),
            Cmd::CreateTexture(2, plain.clone()),
            Cmd::Submit(CommandBuffer {
                encoder: vec![op],
                signal: None,
            }),
        ])
    };

    // Positive control: plain to plain runs, so the refusals are about the sample count.
    assert!(attempt(blit(2, 2)).is_ok(), "a single-sampled blit must run");

    for (op, side) in [(blit(1, 2), "source"), (blit(2, 1), "destination")] {
        let err = attempt(op).expect_err("a multisampled side must be refused");
        assert!(
            matches!(err, GpuError::Unsupported(m) if m.contains("multisample")),
            "a multisampled {side} must be refused by name, got {err:?}"
        );
    }
}
