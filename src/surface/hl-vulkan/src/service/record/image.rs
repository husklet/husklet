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

/// The colour formats that support LINEAR filtering. The 32-bit float formats are absent: WebGPU makes
/// them non-filterable without an optional feature, Vulkan requires the format to advertise linear
/// filtering, and the host was measured refusing exactly these three.
///
/// This list is only ever consulted for a format that HAS a packed colour texel, because compressed and
/// depth/stencil formats are refused before it on grounds that apply to every filter. That ordering is
/// what lets this list mean "cannot be linearly filtered" rather than "is not on a list" — it is the
/// difference between a membership rule and an obligation the entry must meet. Every member is a format
/// the host was measured filtering; the three omissions are formats it was measured refusing.
///
/// This is one of THREE independent layers that decline a linear filter on
/// `R32Float`/`Rg32Float`/`Rgba32Float` —
/// the others are the executor's blit (`hl-gpu-wgpu`'s `blit.rs::filterable`) and the software
/// reference (`hl-gpu`'s `cpu/format.rs::FILTERABLE_REFUSED`). They agreed by coincidence rather than by
/// construction until `hl-gpu-wgpu/tests/float_filter_agreement.rs` bound them; that test fails if any
/// one of the three moves alone, and its header carries the measurement and the condition for enabling
/// the feature. Add a format here only together with the other two.
const FILTERABLE: &[TextureFormat] = &[
    TextureFormat::Rgba8Unorm,
    TextureFormat::Bgra8Unorm,
    TextureFormat::Rgba8Srgb,
    TextureFormat::Bgra8Srgb,
    TextureFormat::R8Unorm,
    TextureFormat::R8Snorm,
    TextureFormat::Rg8Unorm,
    TextureFormat::Rgba8Snorm,
    TextureFormat::Rg16Float,
    TextureFormat::Rgba16Float,
    TextureFormat::Rgba16Unorm,
    TextureFormat::Rg16Unorm,
    TextureFormat::R16Unorm,
    TextureFormat::R16Snorm,
    TextureFormat::Rg16Snorm,
    TextureFormat::Rgba16Snorm,
    TextureFormat::Rgb10a2Unorm,
    TextureFormat::B4g4r4a4Unorm,
    TextureFormat::Rgb9e5Ufloat,
    TextureFormat::Rg11b10Ufloat,
];

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
/// images or provably disjoint subresources of one image, matching numeric format classes, both usages
/// present, positive src/dst extents in-bounds. `linear` selects
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
    src_origin: Origin3d,
    src_extent: Extent3d,
    dst_origin: Origin3d,
    dst_extent: Extent3d,
    linear: bool,
    mirror: Mirror,
) -> Result<()> {
    let (src_ir, src_fmt, src_usage, src_sub, siw, sih, sid, src_dim) = {
        let i = dev
            .images
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdBlitImage: unknown src VkImage"))?;
        let sub = SubresourceLayers::resolve(i, src_sub);
        subresource_in_bounds(i, sub, "vkCmdBlitImage: source")?;
        let (w, h) = i.extent_at(sub.mip_level);
        (
            i.ir_id,
            i.format,
            i.usage,
            sub,
            w,
            h,
            i.depth_at(sub.mip_level),
            i.dim,
        )
    };
    let (dst_ir, dst_fmt, dst_usage, dst_sub, diw, dih, did, dst_dim) = {
        let i = dev
            .images
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdBlitImage: unknown dst VkImage"))?;
        let sub = SubresourceLayers::resolve(i, dst_sub);
        subresource_in_bounds(i, sub, "vkCmdBlitImage: destination")?;
        let (w, h) = i.extent_at(sub.mip_level);
        (
            i.ir_id,
            i.format,
            i.usage,
            sub,
            w,
            h,
            i.depth_at(sub.mip_level),
            i.dim,
        )
    };
    if src_sub.layer_count != dst_sub.layer_count {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: source and destination layer counts differ",
        ));
    }
    if src == dst {
        let src_layer_end = src_sub
            .base_array_layer
            .checked_add(src_sub.layer_count)
            .ok_or(GpuError::OutOfBounds)?;
        let dst_layer_end = dst_sub
            .base_array_layer
            .checked_add(dst_sub.layer_count)
            .ok_or(GpuError::OutOfBounds)?;
        let layers_overlap =
            src_sub.base_array_layer < dst_layer_end && dst_sub.base_array_layer < src_layer_end;
        if src_sub.mip_level == dst_sub.mip_level && layers_overlap {
            return Err(GpuError::Invalid(
                "vkCmdBlitImage: overlapping source and destination subresources of one image",
            ));
        }
    }
    // A BLIT converts; that is what distinguishes it from a copy. The specification lets the two formats
    // differ freely except in numeric class, and the host does exactly that — measured directly, it
    // resamples `Rgba8Unorm` into `Bgra8Unorm`, `Rgba8Srgb`, `R32Float` and `Rgba16Float`, and across a
    // texel-size change in both directions. Demanding identical formats was this surface's own rule, and
    // it refused the canonical case: blitting an image into a differently-formatted swapchain image.
    //
    // Vulkan requires both formats to have the same numeric class. Signed and unsigned integer formats
    // are distinct classes just as integer and normalized/float formats are; the executor has matching
    // sampler-less pipelines for valid Uint/Uint and Sint/Sint NEAREST pairs.
    if src_fmt.numeric_class() != dst_fmt.numeric_class() {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: source and destination formats are not of the same numeric class",
        ));
    }
    // Compressed sources have no per-texel byte layout, but the host sampler decodes them natively. They
    // can therefore feed the draw-based blit. Destinations still require a writable color attachment.
    // A compressed source is sampled natively by the host and therefore is a legal blit source. A
    // compressed destination is still impossible because the executor writes through a color attachment.
    if dst_fmt.bytes_per_texel().is_none() {
        return Err(GpuError::Unsupported(
            "vkCmdBlitImage: destination format has no packed colour texel (compressed or depth/stencil)",
        ));
    }
    if src_fmt.bytes_per_texel().is_none() && src_fmt.block_geometry().is_none() {
        return Err(GpuError::Unsupported(
            "vkCmdBlitImage: source format is depth/stencil",
        ));
    }
    // VK_FILTER_LINEAR requires the source format to support linear filtering, which the 32-bit float
    // formats do not without an optional device feature — measured on the host, which refuses exactly
    // these. Refused HERE so the caller gets an attributable answer at record time instead of a device
    // validation failure later, which is the same improvement the integer refusal above makes. A NEAREST
    // blit from one of these formats is fine and deliberately still records.
    if linear && !FILTERABLE.contains(&src_fmt) && src_fmt.block_geometry().is_none() {
        return Err(GpuError::Unsupported(
            "vkCmdBlitImage: VK_FILTER_LINEAR for a source format that cannot be linearly filtered",
        ));
    }
    if src_usage & texture_usage::COPY_SRC == 0 || dst_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: missing COPY_SRC/COPY_DST usage",
        ));
    }
    if src_extent.width == 0
        || src_extent.height == 0
        || src_extent.depth == 0
        || dst_extent.width == 0
        || dst_extent.height == 0
        || dst_extent.depth == 0
    {
        return Err(GpuError::OutOfBounds);
    }
    // Only a 3D image has a depth axis to span. Vulkan pins a non-3D region's z offsets at 0 and 1, so a
    // wider span is the application's error rather than a capability question.
    //
    // This has to come BEFORE the bounds check, and the ordering is the point rather than a detail — the
    // same lesson the packed-texel refusal above records. A 2D image's depth is one at every level, so a
    // two-slice span would otherwise fall through and come back `OutOfBounds`, which reads as "your
    // region is too big" about a region that is the right size on an image with no third axis at all.
    for (dim, extent, side) in [
        (src_dim, src_extent, "source"),
        (dst_dim, dst_extent, "destination"),
    ] {
        if dim != TextureDim::D3 && extent.depth > 1 {
            return Err(GpuError::Invalid(match side {
                "source" => {
                    "vkCmdBlitImage: source is not a 3D image and has no depth axis to span"
                }
                _ => "vkCmdBlitImage: destination is not a 3D image and has no depth axis to span",
            }));
        }
    }
    // Unequal D3 spans are legal core Vulkan. The host samples the source as a true 3D texture, so
    // NEAREST selects the source slice under each destination center and LINEAR interpolates along z.
    // No capability query lets a driver withdraw this by image shape, so it must reach the executor.
    // Checked add: a hostile `origin` near `u32::MAX` must be a truthful OutOfBounds, never an
    // `origin + extent` add-overflow panic.
    let in_bounds = |o: u32, e: u32, dim: u32| o.checked_add(e).is_some_and(|end| end <= dim);
    if !in_bounds(src_origin.x, src_extent.width, siw)
        || !in_bounds(src_origin.y, src_extent.height, sih)
        || !in_bounds(src_origin.z, src_extent.depth, sid)
        || !in_bounds(dst_origin.x, dst_extent.width, diw)
        || !in_bounds(dst_origin.y, dst_extent.height, dih)
        || !in_bounds(dst_origin.z, dst_extent.depth, did)
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
            src_origin,
            src_extent,
            dst: dst_ir,
            dst_sub: TextureSubresource {
                mip: dst_sub.mip_level,
                layer: dst_sub.base_array_layer + layer,
                aspect: dst_aspect,
            },
            dst_origin,
            dst_extent,
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
                color: color.map(f64::from),
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
