use super::*;

/// `vkCmdCopyImage` (one region, base subresource) — record an exact-size `CopyTextureToTexture`. Formats
/// must match; both usages present; both regions in-bounds; overlapping same-image self-copy rejected.
#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkImage,
    src_origin: (u32, u32),
    dst_origin: (u32, u32),
    extent: (u32, u32),
) -> Result<()> {
    let (src_ir, src_fmt, src_usage, siw, sih) = {
        let i = dev
            .images
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdCopyImage: unknown src VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    let (dst_ir, dst_fmt, dst_usage, diw, dih) = {
        let i = dev
            .images
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdCopyImage: unknown dst VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    if src_fmt != dst_fmt {
        return Err(GpuError::Invalid(
            "vkCmdCopyImage: source and destination formats differ",
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
    // A same-image overlapping self-copy is undefined; reject it (the reference does).
    if src == dst
        && src_origin.0 < dst_origin.0 + w
        && dst_origin.0 < src_origin.0 + w
        && src_origin.1 < dst_origin.1 + h
        && dst_origin.1 < src_origin.1 + h
    {
        return Err(GpuError::Invalid("vkCmdCopyImage: overlapping self-copy"));
    }
    let rec = dev.require_recording(cb)?;
    rec.enc.push(Enc::CopyTextureToTexture {
        src: src_ir,
        src_sub: TextureSubresource::base(),
        src_origin: Origin3d {
            x: src_origin.0,
            y: src_origin.1,
            z: 0,
        },
        dst: dst_ir,
        dst_sub: TextureSubresource::base(),
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
    Ok(())
}

/// `vkCmdBlitImage` (one region, base subresource) — record a scaled/filtered `BlitTexture`. Distinct
/// images, matching formats, both usages present, positive src/dst extents in-bounds. `linear` selects
/// the resampling filter (`VK_FILTER_LINEAR` → [`Filter::Linear`], else [`Filter::Nearest`]).
#[allow(clippy::too_many_arguments)]
pub fn cmd_blit_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkImage,
    src_origin: (u32, u32),
    src_extent: (u32, u32),
    dst_origin: (u32, u32),
    dst_extent: (u32, u32),
    linear: bool,
) -> Result<()> {
    if src == dst {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: src and dst image must differ",
        ));
    }
    let (src_ir, src_fmt, src_usage, siw, sih) = {
        let i = dev
            .images
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdBlitImage: unknown src VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    let (dst_ir, dst_fmt, dst_usage, diw, dih) = {
        let i = dev
            .images
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdBlitImage: unknown dst VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    if src_fmt != dst_fmt {
        return Err(GpuError::Invalid(
            "vkCmdBlitImage: source and destination formats differ",
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
    let rec = dev.require_recording(cb)?;
    rec.enc.push(Enc::BlitTexture {
        src: src_ir,
        src_sub: TextureSubresource::base(),
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
        dst_sub: TextureSubresource::base(),
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
    });
    Ok(())
}

/// `vkCmdResolveImage` (one region, base subresource) — a multisample-resolve.
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
    src_origin: (u32, u32),
    dst_origin: (u32, u32),
    extent: (u32, u32),
) -> Result<()> {
    let src_samples = dev
        .images
        .get(&src)
        .ok_or(GpuError::Invalid("vkCmdResolveImage: unknown src VkImage"))?
        .sample_count;
    if src_samples <= 1 {
        // Single-sample source: resolve degenerates to a content-moving copy.
        return cmd_copy_image(dev, cb, src, dst, src_origin, dst_origin, extent);
    }
    // Multisample source: emit the real resolve. Validate identically to a copy (formats/usages/bounds) so a
    // bad resolve is a truthful error, then push ResolveTexture instead of CopyTextureToTexture.
    let (src_ir, src_fmt, src_usage, siw, sih) = {
        let i = dev
            .images
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdResolveImage: unknown src VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    let (dst_ir, dst_fmt, dst_usage, diw, dih) = {
        let i = dev
            .images
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdResolveImage: unknown dst VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
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
    let rec = dev.require_recording(cb)?;
    rec.enc.push(Enc::ResolveTexture {
        src: src_ir,
        src_sub: TextureSubresource::base(),
        src_origin: Origin3d {
            x: src_origin.0,
            y: src_origin.1,
            z: 0,
        },
        dst: dst_ir,
        dst_sub: TextureSubresource::base(),
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
    Ok(())
}

// ---- clears ------------------------------------------------------------------------------------

/// `vkCmdClearColorImage` (base subresource) — record a full-extent `ClearRect` on the image. The image
/// must be `COPY_DST` (a transfer-clear target). Ported from `command.rs::vkCmdClearColorImage`.
pub fn cmd_clear_color_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    image: VkImage,
    color: [f32; 4],
) -> Result<()> {
    let (ir, usage, w, h) = {
        let i = dev
            .images
            .get(&image)
            .ok_or(GpuError::Invalid("vkCmdClearColorImage: unknown VkImage"))?;
        (i.ir_id, i.usage, i.width, i.height)
    };
    if usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdClearColorImage: image missing COPY_DST usage",
        ));
    }
    let rec = dev.require_recording(cb)?;
    rec.enc.push(Enc::ClearRect {
        texture: ir,
        x: 0,
        y: 0,
        w,
        h,
        color,
    });
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
    let rec = dev.require_recording(cb)?;
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
