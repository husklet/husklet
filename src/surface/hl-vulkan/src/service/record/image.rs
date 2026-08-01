use super::*;

/// The IR aspect a region's Vulkan `aspectMask` names on an image of `format`, or a typed refusal.
///
/// The rule that matters is the accepting one. A Vulkan depth copy MUST name `VK_IMAGE_ASPECT_DEPTH_BIT`
/// — there is no "all" to pass — so a mask naming exactly the aspects the format HAS is the ordinary,
/// legal case and maps to `TextureAspect::All`. Refusing every non-colour mask would turn every legal
/// depth copy into an error, which is the shape of mistake that once turned seven hundred and
/// eighty-three honest refusals into failures on this surface.
///
/// A STRICT SUBSET of a combined depth/stencil image is the case that cannot be served: measured on the
/// host, a `DepthOnly` or `StencilOnly` image-to-image copy is refused outright, so the only honest
/// answers are this refusal or the silent both-planes copy that recording `All` produced. The refusal at
/// least reaches the caller.
fn ir_aspect(format: TextureFormat, aspect_mask: u32, what: &'static str) -> Result<TextureAspect> {
    let (depth, stencil) = (
        aspect_mask & SubresourceLayers::ASPECT_DEPTH != 0,
        aspect_mask & SubresourceLayers::ASPECT_STENCIL != 0,
    );
    let has_stencil = matches!(format, TextureFormat::Depth24PlusStencil8);
    let has_depth = has_stencil || matches!(format, TextureFormat::Depth32Float);
    if !has_depth {
        // A colour image: any mask names the whole thing.
        return Ok(TextureAspect::All);
    }
    // A depth image, named in full (depth for a depth-only format, depth+stencil for a combined one).
    if depth && stencil == has_stencil {
        return Ok(TextureAspect::All);
    }
    Err(GpuError::Unsupported(match what {
        "vkCmdCopyImage" => "vkCmdCopyImage: single-aspect copy of a combined depth/stencil image",
        "vkCmdBlitImage" => "vkCmdBlitImage: single-aspect blit of a combined depth/stencil image",
        _ => "vkCmdResolveImage: single-aspect resolve of a combined depth/stencil image",
    }))
}

/// The colour formats whose texels are raw integers.
///
/// Vulkan requires a blit's two formats to share a numeric class — a signed-integer source needs a
/// signed-integer destination, an unsigned one an unsigned destination — so a mixed integer/float pair is
/// the APPLICATION's error. A matched integer pair is legal Vulkan and this driver still cannot serve it,
/// because the host performs a blit by rendering: measured directly, an integer view cannot bind to the
/// blit's filterable float sampler and the blit shader's float output is incompatible with an integer
/// colour target. Those are two different answers and are reported as two different errors.
const INTEGER_FORMATS: &[TextureFormat] = &[
    TextureFormat::R8Uint,
    TextureFormat::R8Sint,
    TextureFormat::Rg8Uint,
    TextureFormat::Rg8Sint,
    TextureFormat::Rgba8Uint,
    TextureFormat::Rgba8Sint,
];

/// The colour formats that support LINEAR filtering. The 32-bit float formats are absent: WebGPU makes
/// them non-filterable without an optional feature, Vulkan requires the format to advertise linear
/// filtering, and the host was measured refusing exactly these two.
const FILTERABLE: &[TextureFormat] = &[
    TextureFormat::Rgba8Unorm,
    TextureFormat::Bgra8Unorm,
    TextureFormat::Rgba8Srgb,
    TextureFormat::Bgra8Srgb,
    TextureFormat::R8Unorm,
    TextureFormat::Rg8Unorm,
    TextureFormat::Rgba16Float,
];

fn is_integer(format: TextureFormat) -> bool {
    INTEGER_FORMATS.contains(&format)
}

/// Refuse a region whose subresource does not exist on `image`. A copy is not a clear: clamping a
/// too-large level or layer run would return the wrong texels rather than fewer of them, so this is a
/// truthful error instead of a silent narrowing.
fn subresource_in_bounds(
    image: &ImageRec,
    sub: SubresourceLayers,
    what: &'static str,
) -> Result<()> {
    if sub.mip_level >= image.mip_levels {
        return Err(GpuError::Invalid(match what {
            "vkCmdCopyImage: source" => "vkCmdCopyImage: source mip level does not exist",
            "vkCmdCopyImage: destination" => "vkCmdCopyImage: destination mip level does not exist",
            "vkCmdBlitImage: source" => "vkCmdBlitImage: source mip level does not exist",
            "vkCmdBlitImage: destination" => "vkCmdBlitImage: destination mip level does not exist",
            "vkCmdResolveImage: source" => "vkCmdResolveImage: source mip level does not exist",
            _ => "vkCmdResolveImage: destination mip level does not exist",
        }));
    }
    if sub.layer_count == 0
        || sub
            .base_array_layer
            .checked_add(sub.layer_count)
            .is_none_or(|end| end > image.layers)
    {
        return Err(GpuError::OutOfBounds);
    }
    Ok(())
}

/// `vkCmdCopyImage` (one region) — record an exact-size `CopyTextureToTexture` per array layer the
/// region names. Formats must match; both usages present; both regions in-bounds AT THE MIP LEVEL THEY
/// NAME; overlapping same-image self-copy rejected.
///
/// The `VkImageSubresourceLayers` of each region used to be discarded and every copy recorded against
/// mip 0 / layer 0, so a copy of level 3 read and wrote level 0's texels. The bounds check compounded it
/// by measuring the region against the base level, where every smaller level trivially fits, so the
/// wrong-level copy passed validation instead of being refused.
#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkImage,
    src_sub: SubresourceLayers,
    dst_sub: SubresourceLayers,
    src_origin: (u32, u32),
    dst_origin: (u32, u32),
    extent: (u32, u32),
) -> Result<()> {
    let (src_ir, src_fmt, src_usage, src_sub, siw, sih) = {
        let i = dev
            .images
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdCopyImage: unknown src VkImage"))?;
        let sub = SubresourceLayers::resolve(i, src_sub);
        subresource_in_bounds(i, sub, "vkCmdCopyImage: source")?;
        let (w, h) = i.extent_at(sub.mip_level);
        (i.ir_id, i.format, i.usage, sub, w, h)
    };
    let (dst_ir, dst_fmt, dst_usage, dst_sub, diw, dih) = {
        let i = dev
            .images
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdCopyImage: unknown dst VkImage"))?;
        let sub = SubresourceLayers::resolve(i, dst_sub);
        subresource_in_bounds(i, sub, "vkCmdCopyImage: destination")?;
        let (w, h) = i.extent_at(sub.mip_level);
        (i.ir_id, i.format, i.usage, sub, w, h)
    };
    if src_sub.layer_count != dst_sub.layer_count {
        return Err(GpuError::Invalid(
            "vkCmdCopyImage: source and destination layer counts differ",
        ));
    }
    // Vulkan asks for SIZE-COMPATIBLE formats, not identical ones, because a copy reinterprets: RGBA8
    // into BGRA8 moves four bytes unchanged and reads back channel-swapped. That is now expressible,
    // because `Enc::CopyTextureToTexture` means exactly one thing.
    //
    // It briefly was not. I relaxed this once on the evidence that the executor ACCEPTED such a copy,
    // which was not evidence — accepted without error is not produced the right bytes — and the two
    // backends turned out to disagree, one reinterpreting and one converting. The operation has since
    // been split: conversion is `BlitTexture`, which every caller wanting it now emits, and this
    // operation reinterprets on both backends under the same equal-bytes-per-texel rule the oracle
    // always applied. Only with that settled is the specification's rule safe to express here.
    if src_fmt.bytes_per_texel() != dst_fmt.bytes_per_texel() {
        return Err(GpuError::Invalid(
            "vkCmdCopyImage: source and destination formats are not size-compatible",
        ));
    }
    if src_usage & texture_usage::COPY_SRC == 0 || dst_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdCopyImage: missing COPY_SRC/COPY_DST usage",
        ));
    }
    let (w, h) = extent;
    if w == 0 || h == 0 {
        return Err(GpuError::OutOfBounds);
    }
    // Checked add: a hostile `origin` near `u32::MAX` must be a truthful OutOfBounds, never an
    // `origin + extent` add-overflow panic.
    let in_bounds = |o: u32, e: u32, dim: u32| o.checked_add(e).is_some_and(|end| end <= dim);
    if !in_bounds(src_origin.0, w, siw)
        || !in_bounds(src_origin.1, h, sih)
        || !in_bounds(dst_origin.0, w, diw)
        || !in_bounds(dst_origin.1, h, dih)
    {
        return Err(GpuError::OutOfBounds);
    }
    // A same-image overlapping self-copy is undefined; reject it (the reference does). Overlap is only
    // possible within ONE subresource: the same rectangle of two different mip levels or array layers is
    // two disjoint regions of memory, and refusing that would reject a legal layer-to-layer copy.
    if src == dst
        && src_sub.mip_level == dst_sub.mip_level
        && src_sub.base_array_layer < dst_sub.base_array_layer + dst_sub.layer_count
        && dst_sub.base_array_layer < src_sub.base_array_layer + src_sub.layer_count
        && src_origin.0 < dst_origin.0 + w
        && dst_origin.0 < src_origin.0 + w
        && src_origin.1 < dst_origin.1 + h
        && dst_origin.1 < src_origin.1 + h
    {
        return Err(GpuError::Invalid("vkCmdCopyImage: overlapping self-copy"));
    }
    // Resolve the region's aspect against BOTH images before recording anything: a subset aspect the
    // host cannot serve must be refused, not silently widened to the whole image.
    let src_aspect = ir_aspect(src_fmt, src_sub.aspect_mask, "vkCmdCopyImage")?;
    let dst_aspect = ir_aspect(dst_fmt, dst_sub.aspect_mask, "vkCmdCopyImage")?;
    let rec = dev.require_recording_outside_pass(
        cb,
        "vkCmdCopyImage: must be recorded outside a render pass",
    )?;
    // The IR subresource addresses ONE layer, so a multi-layer region becomes one op per layer pair.
    for layer in 0..src_sub.layer_count {
        rec.enc.push(Enc::CopyTextureToTexture {
            src: src_ir,
            src_sub: TextureSubresource {
                mip: src_sub.mip_level,
                layer: src_sub.base_array_layer + layer,
                aspect: src_aspect,
            },
            src_origin: Origin3d {
                x: src_origin.0,
                y: src_origin.1,
                z: 0,
            },
            dst: dst_ir,
            dst_sub: TextureSubresource {
                mip: dst_sub.mip_level,
                layer: dst_sub.base_array_layer + layer,
                aspect: dst_aspect,
            },
            dst_origin: Origin3d {
                x: dst_origin.0,
                y: dst_origin.1,
                z: 0,
            },
            extent: Extent3d {
                width: w,
                height: h,
                depth: 1,
            },
        });
    }
    Ok(())
}

/// `vkCmdBlitImage` (one region) — record a scaled/filtered `BlitTexture` per array layer. Distinct
/// images, matching formats, both usages present, positive src/dst extents in-bounds. `linear` selects
/// the resampling filter (`VK_FILTER_LINEAR` → [`Filter::Linear`], else [`Filter::Nearest`]).
///
/// `mirror` is the NET per-axis flip the caller derived from its two offset pairs. Vulkan expresses a
/// mirrored blit by inverting a rect's bounds, and the origin/extent this function takes are already
/// normalized, so the intent has to arrive alongside them or not at all — it used to not at all.
#[allow(clippy::too_many_arguments)]
pub fn cmd_blit_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkImage,
    src_sub: SubresourceLayers,
    dst_sub: SubresourceLayers,
    src_origin: (u32, u32),
    src_extent: (u32, u32),
    dst_origin: (u32, u32),
    dst_extent: (u32, u32),
    linear: bool,
    mirror: Mirror,
) -> Result<()> {
    if src == dst {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: src and dst image must differ",
        ));
    }
    let (src_ir, src_fmt, src_usage, src_sub, siw, sih) = {
        let i = dev
            .images
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdBlitImage: unknown src VkImage"))?;
        let sub = SubresourceLayers::resolve(i, src_sub);
        subresource_in_bounds(i, sub, "vkCmdBlitImage: source")?;
        let (w, h) = i.extent_at(sub.mip_level);
        (i.ir_id, i.format, i.usage, sub, w, h)
    };
    let (dst_ir, dst_fmt, dst_usage, dst_sub, diw, dih) = {
        let i = dev
            .images
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdBlitImage: unknown dst VkImage"))?;
        let sub = SubresourceLayers::resolve(i, dst_sub);
        subresource_in_bounds(i, sub, "vkCmdBlitImage: destination")?;
        let (w, h) = i.extent_at(sub.mip_level);
        (i.ir_id, i.format, i.usage, sub, w, h)
    };
    if src_sub.layer_count != dst_sub.layer_count {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: source and destination layer counts differ",
        ));
    }
    // A BLIT converts; that is what distinguishes it from a copy. The specification lets the two formats
    // differ freely except in numeric class, and the host does exactly that — measured directly, it
    // resamples `Rgba8Unorm` into `Bgra8Unorm`, `Rgba8Srgb`, `R32Float` and `Rgba16Float`, and across a
    // texel-size change in both directions. Demanding identical formats was this surface's own rule, and
    // it refused the canonical case: blitting an image into a differently-formatted swapchain image.
    //
    // The two refusals that remain are different in kind and say so. A mixed integer/float pair violates
    // the specification's numeric-class rule and is the application's error. A MATCHED integer pair is
    // legal Vulkan that this driver cannot serve, because the host blits by rendering and neither binds
    // an integer view to a filterable sampler nor writes a float shader output into an integer target —
    // an honest refusal here, where the caller can see it, rather than a device-validation failure later.
    if is_integer(src_fmt) != is_integer(dst_fmt) {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: source and destination formats are not of the same numeric class",
        ));
    }
    if is_integer(src_fmt) {
        return Err(GpuError::Unsupported(
            "vkCmdBlitImage: blit of an integer format",
        ));
    }
    // VK_FILTER_LINEAR requires the source format to support linear filtering, which the 32-bit float
    // formats do not without an optional device feature — measured on the host, which refuses exactly
    // these. Refused HERE so the caller gets an attributable answer at record time instead of a device
    // validation failure later, which is the same improvement the integer refusal above makes. A NEAREST
    // blit from one of these formats is fine and deliberately still records.
    if linear && !FILTERABLE.contains(&src_fmt) {
        return Err(GpuError::Unsupported(
            "vkCmdBlitImage: VK_FILTER_LINEAR for a source format that cannot be linearly filtered",
        ));
    }
    if src_usage & texture_usage::COPY_SRC == 0 || dst_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: missing COPY_SRC/COPY_DST usage",
        ));
    }
    if src_extent.0 == 0 || src_extent.1 == 0 || dst_extent.0 == 0 || dst_extent.1 == 0 {
        return Err(GpuError::OutOfBounds);
    }
    // Checked add: a hostile `origin` near `u32::MAX` must be a truthful OutOfBounds, never an
    // `origin + extent` add-overflow panic.
    let in_bounds = |o: u32, e: u32, dim: u32| o.checked_add(e).is_some_and(|end| end <= dim);
    if !in_bounds(src_origin.0, src_extent.0, siw)
        || !in_bounds(src_origin.1, src_extent.1, sih)
        || !in_bounds(dst_origin.0, dst_extent.0, diw)
        || !in_bounds(dst_origin.1, dst_extent.1, dih)
    {
        return Err(GpuError::OutOfBounds);
    }
    // Resolve the region's aspect against BOTH images before recording anything: a subset aspect the
    // host cannot serve must be refused, not silently widened to the whole image.
    let src_aspect = ir_aspect(src_fmt, src_sub.aspect_mask, "vkCmdBlitImage")?;
    let dst_aspect = ir_aspect(dst_fmt, dst_sub.aspect_mask, "vkCmdBlitImage")?;
    let rec = dev.require_recording_outside_pass(
        cb,
        "vkCmdBlitImage: must be recorded outside a render pass",
    )?;
    for layer in 0..src_sub.layer_count {
        rec.enc.push(Enc::BlitTexture {
            src: src_ir,
            src_sub: TextureSubresource {
                mip: src_sub.mip_level,
                layer: src_sub.base_array_layer + layer,
                aspect: src_aspect,
            },
            src_origin: Origin3d {
                x: src_origin.0,
                y: src_origin.1,
                z: 0,
            },
            src_extent: Extent3d {
                width: src_extent.0,
                height: src_extent.1,
                depth: 1,
            },
            dst: dst_ir,
            dst_sub: TextureSubresource {
                mip: dst_sub.mip_level,
                layer: dst_sub.base_array_layer + layer,
                aspect: dst_aspect,
            },
            dst_origin: Origin3d {
                x: dst_origin.0,
                y: dst_origin.1,
                z: 0,
            },
            dst_extent: Extent3d {
                width: dst_extent.0,
                height: dst_extent.1,
                depth: 1,
            },
            filter: if linear {
                Filter::Linear
            } else {
                Filter::Nearest
            },
            mirror,
        });
    }
    Ok(())
}

/// `vkCmdResolveImage` (one region) — a multisample-resolve, one op per array layer.
///
/// When the SOURCE `VkImage` is multisampled (`sample_count > 1`, threaded from `VkImageCreateInfo::samples`
/// by [`create::create_image`]), this is a TRUE resolve: it averages the source's samples down into the
/// single-sample destination. It lowers to [`Enc::ResolveTexture`] (the executor's real multisample resolve,
/// #179) — NOT a copy, which would only pick one sample and drop the antialiasing.
///
/// When the source is single-sample (`sample_count == 1`), a same-extent same-format resolve is exactly an
/// image COPY: it MOVES the rendered content into the resolve target. Recording nothing (the former no-op)
/// left the resolve target blank — an app that renders to a color attachment and resolves into its
/// swapchain/present image would present an empty frame. Lower it to the SAME `CopyTextureToTexture` a
/// `vkCmdCopyImage` of the region emits.
///
/// Both paths enforce matching formats, both usages present, and region in-bounds (via [`cmd_copy_image`]'s
/// validation, which the resolve path reuses). Truthful error on a bad handle / mismatch / OOB.
#[allow(clippy::too_many_arguments)]
pub fn cmd_resolve_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkImage,
    src_sub: SubresourceLayers,
    dst_sub: SubresourceLayers,
    src_origin: (u32, u32),
    dst_origin: (u32, u32),
    extent: (u32, u32),
) -> Result<()> {
    let src_samples = dev
        .images
        .get(&src)
        .ok_or(GpuError::Invalid("vkCmdResolveImage: unknown src VkImage"))?
        .sample_count;
    // A RESOLVE requires IDENTICAL formats — not the size-compatibility a copy asks for — and it requires
    // them whatever the sample count. Checked HERE, above the single-sample shortcut, because that
    // shortcut delegates to `cmd_copy_image` and would otherwise inherit the copy's looser rule: relaxing
    // copy to size-compatible silently widened resolve through this one line, which is what enumerating
    // the callers of a widened check is for.
    if let (Some(s), Some(d)) = (dev.images.get(&src), dev.images.get(&dst)) {
        if s.format != d.format {
            return Err(GpuError::Invalid(
                "vkCmdResolveImage: source and destination formats differ",
            ));
        }
    }
    if src_samples <= 1 {
        // Single-sample source: resolve degenerates to a content-moving copy.
        return cmd_copy_image(
            dev, cb, src, dst, src_sub, dst_sub, src_origin, dst_origin, extent,
        );
    }
    // Multisample source: emit the real resolve. Validate identically to a copy (formats/usages/bounds) so a
    // bad resolve is a truthful error, then push ResolveTexture instead of CopyTextureToTexture.
    let (src_ir, src_fmt, src_usage, src_sub, siw, sih) = {
        let i = dev
            .images
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdResolveImage: unknown src VkImage"))?;
        let sub = SubresourceLayers::resolve(i, src_sub);
        subresource_in_bounds(i, sub, "vkCmdResolveImage: source")?;
        let (w, h) = i.extent_at(sub.mip_level);
        (i.ir_id, i.format, i.usage, sub, w, h)
    };
    let (dst_ir, dst_fmt, dst_usage, dst_sub, diw, dih) = {
        let i = dev
            .images
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdResolveImage: unknown dst VkImage"))?;
        let sub = SubresourceLayers::resolve(i, dst_sub);
        subresource_in_bounds(i, sub, "vkCmdResolveImage: destination")?;
        let (w, h) = i.extent_at(sub.mip_level);
        (i.ir_id, i.format, i.usage, sub, w, h)
    };
    if src_sub.layer_count != dst_sub.layer_count {
        return Err(GpuError::Invalid(
            "vkCmdResolveImage: source and destination layer counts differ",
        ));
    }
    if src_fmt != dst_fmt {
        return Err(GpuError::Invalid(
            "vkCmdResolveImage: source and destination formats differ",
        ));
    }
    if src_usage & texture_usage::COPY_SRC == 0 || dst_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdResolveImage: missing COPY_SRC/COPY_DST usage",
        ));
    }
    let (w, h) = extent;
    if w == 0 || h == 0 {
        return Err(GpuError::OutOfBounds);
    }
    let in_bounds = |o: u32, e: u32, dim: u32| o.checked_add(e).is_some_and(|end| end <= dim);
    if !in_bounds(src_origin.0, w, siw)
        || !in_bounds(src_origin.1, h, sih)
        || !in_bounds(dst_origin.0, w, diw)
        || !in_bounds(dst_origin.1, h, dih)
    {
        return Err(GpuError::OutOfBounds);
    }
    // Resolve the region's aspect against BOTH images before recording anything: a subset aspect the
    // host cannot serve must be refused, not silently widened to the whole image.
    let src_aspect = ir_aspect(src_fmt, src_sub.aspect_mask, "vkCmdResolveImage")?;
    let dst_aspect = ir_aspect(dst_fmt, dst_sub.aspect_mask, "vkCmdResolveImage")?;
    let rec = dev.require_recording_outside_pass(
        cb,
        "vkCmdResolveImage: must be recorded outside a render pass",
    )?;
    for layer in 0..src_sub.layer_count {
        rec.enc.push(Enc::ResolveTexture {
            src: src_ir,
            src_sub: TextureSubresource {
                mip: src_sub.mip_level,
                layer: src_sub.base_array_layer + layer,
                aspect: src_aspect,
            },
            src_origin: Origin3d {
                x: src_origin.0,
                y: src_origin.1,
                z: 0,
            },
            dst: dst_ir,
            dst_sub: TextureSubresource {
                mip: dst_sub.mip_level,
                layer: dst_sub.base_array_layer + layer,
                aspect: dst_aspect,
            },
            dst_origin: Origin3d {
                x: dst_origin.0,
                y: dst_origin.1,
                z: 0,
            },
            extent: Extent3d {
                width: w,
                height: h,
                depth: 1,
            },
        });
    }
    Ok(())
}

// ---- clears ------------------------------------------------------------------------------------

/// `vkCmdClearColorImage` — record a full-extent `ClearRect` over every mip level and array layer the
/// `VkImageSubresourceRange`s name. The image must be `COPY_DST` (a transfer-clear target).
///
/// The ranges were previously discarded and one clear was recorded over the base extent, which the
/// executor then wrote to layer 0 of mip 0 whatever the caller asked for. A clear of layers 1..N therefore
/// painted layer 0 — writing texels the caller wanted preserved and preserving the ones it wanted
/// cleared. One op is recorded per level because each level has its own extent; the layer run rides on the
/// op itself, since the executor fills a contiguous run of layers in a single upload.
pub fn cmd_clear_color_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    image: VkImage,
    color: [f32; 4],
    ranges: &[SubresourceRange],
) -> Result<()> {
    let (ir, usage, w, h, resolved) = {
        let i = dev
            .images
            .get(&image)
            .ok_or(GpuError::Invalid("vkCmdClearColorImage: unknown VkImage"))?;
        let resolved: Vec<SubresourceRange> = if ranges.is_empty() {
            vec![SubresourceRange::whole(i)]
        } else {
            ranges
                .iter()
                .map(|range| SubresourceRange::resolve(i, *range))
                .collect()
        };
        (i.ir_id, i.usage, i.width, i.height, resolved)
    };
    if usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdClearColorImage: image missing COPY_DST usage",
        ));
    }
    let rec = dev.require_recording_outside_pass(
        cb,
        "vkCmdClearColorImage: must be recorded outside a render pass",
    )?;
    for range in resolved {
        if range.layer_count == 0 {
            continue;
        }
        for level in range.base_mip_level..range.base_mip_level + range.level_count {
            rec.enc.push(Enc::ClearRect {
                texture: ir,
                x: 0,
                y: 0,
                // Each level clears its OWN extent. Passing the base extent would overhang every smaller
                // level, and the clamp would then silently shrink the clear to whatever fitted.
                w: (w >> level).max(1),
                h: (h >> level).max(1),
                color,
                base_array_layer: range.base_array_layer,
                layer_count: range.layer_count,
                mip_level: level,
            });
        }
    }
    Ok(())
}

/// `vkCmdClearDepthStencilImage` — clear a depth/stencil image OUTSIDE a render pass. hl has no standalone
/// depth-clear IR op, but the executor DOES clear a depth attachment when a render pass's depth `LoadOp` is
/// `Clear` (`Enc::BeginRenderPass`'s [`DepthAttachment`], landed in the depth-attachment work). So this
/// lowers to a zero-draw depth-clear pass: begin a render pass with NO color target and the image as the
/// depth attachment (`LoadOp::Clear`, `clear_depth`/`clear_stencil` = the `VkClearDepthStencilValue` the
/// app passed), then immediately end it — exactly the depth clear a `vkCmdBeginRendering`/`BeginRenderPass`
/// with a CLEAR depth loadOp performs, but standalone (the two-pass "clear then Load-and-test" pattern the
/// executor already supports). Recording nothing (the former no-op) left the depth image untouched, so an
/// app that cleared depth outside a pass then depth-tested against it saw stale/garbage depth.
///
/// `has_stencil` says the caller's aspect + the image format both carry a stencil plane; when false the
/// stencil clear value is forced to `0` (a depth-only `Depth32Float` attachment has no stencil plane, and a
/// depth-only aspect must not fabricate a stencil write). The image must be a depth/stencil format with
/// `COPY_DST` (the `VK_IMAGE_USAGE_TRANSFER_DST_BIT` the spec requires of a clear target). Truthful error on
/// a bad handle / non-depth format / missing usage.
pub fn cmd_clear_depth_stencil_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    image: VkImage,
    clear_depth: f32,
    clear_stencil: u32,
    has_stencil: bool,
) -> Result<()> {
    let (ir, format, usage) = {
        let i = dev.images.get(&image).ok_or(GpuError::Invalid(
            "vkCmdClearDepthStencilImage: unknown VkImage",
        ))?;
        (i.ir_id, i.format, i.usage)
    };
    let format_has_stencil = match format {
        TextureFormat::Depth32Float => false,
        TextureFormat::Depth24PlusStencil8 => true,
        _ => {
            return Err(GpuError::Invalid(
                "vkCmdClearDepthStencilImage: image is not a depth/stencil format",
            ))
        }
    };
    if usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdClearDepthStencilImage: image missing COPY_DST (TRANSFER_DST) usage",
        ));
    }
    let clear_stencil = if has_stencil && format_has_stencil {
        clear_stencil
    } else {
        0
    };
    let rec = dev.require_recording_outside_pass(
        cb,
        "vkCmdClearDepthStencilImage: must be recorded outside a render pass",
    )?;
    rec.enc.push(Enc::BeginRenderPass {
        color: Vec::new(),
        depth: Some(DepthAttachment {
            texture: ir,
            load: LoadOp::Clear,
            clear_depth,
            clear_stencil,
        }),
    });
    rec.enc.push(Enc::EndRenderPass);
    Ok(())
}
