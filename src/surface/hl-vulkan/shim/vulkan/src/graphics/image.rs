use super::*;

// ==================================================================================================
// images + image views + samplers
// ==================================================================================================

pub extern "C" fn vkCreateImage(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_image: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkImageCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_image.is_null() {
        unsafe { *p_image = 0 };
    }
    ShimState::with_sink(|dev, sink| {
        let cube = ci.flags & 0x10 != 0;
        let dim = match ci.image_type {
            // A 1D image is materialized as a real `TextureDim::D1`, not collapsed onto a 1-row 2D
            // image: a `sampler1D` binding expects a D1 view and rejects anything else at bind time.
            // Refusing it outright was an under-claim of a capability the executor has, and Vulkan
            // requires 1D optimal images of an ordinary format.
            0 => TextureDim::D1,
            1 => TextureDim::D2,
            2 => TextureDim::D3,
            _ => return VK_ERROR_INITIALIZATION_FAILED,
        };
        if ci.extent.depth == 0
            || (dim == TextureDim::D3 && ci.array_layers != 1)
            || (dim != TextureDim::D3 && ci.extent.depth != 1)
            || (cube && dim != TextureDim::D2)
        {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        if cube && (ci.array_layers < 6 || ci.array_layers % 6 != 0) {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        match create::create_image_geometry(
            dev,
            sink,
            ci.extent.width,
            ci.extent.height.max(1),
            ci.extent.depth,
            ci.array_layers.max(1),
            ci.mip_levels.max(1),
            if cube { TextureDim::Cube } else { dim },
            ci.format as u32,
            ci.usage,
            // `VkImageCreateInfo::samples` is a `VkSampleCountFlagBits` whose bit VALUE is the sample count.
            // `create::create_image` collapses 0 to single-sample, so an absent field stays byte-identical.
            ci.samples as u32,
        ) {
            Ok(h) => {
                if !p_image.is_null() {
                    unsafe { *p_image = h };
                }
                VK_SUCCESS
            }
            Err(e) => Status::from_error(&e),
        }
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

pub extern "C" fn vkDestroyImage(_device: *mut c_void, image: u64, _p_allocator: *const c_void) {
    ShimState::with_sink(|dev, sink| {
        let _ = create::destroy_image(dev, sink, image);
    });
}

pub extern "C" fn vkGetImageMemoryRequirements(
    _device: *mut c_void,
    image: u64,
    p_memory_requirements: *mut c_void,
) {
    let Some(out) = (unsafe { (p_memory_requirements as *mut VkMemoryRequirements).as_mut() })
    else {
        return;
    };
    // A render-target image is host-owned; report a plausible footprint (width*height*bytes_per_texel) so a
    // probing app's allocation math is sane, even though hl binds no VkDeviceMemory to it. The size MUST be
    // format-aware: a blind *4 over-reports a 1-byte R8 coverage atlas 4x, and once GPUI grows its glyph
    // atlas that inflated requirement crosses gpu-alloc's 2 GiB max-allocation ceiling and spuriously
    // OutOfMemory-device-losts wgpu with the real heap wide open.
    let size = ShimState::with_device(|dev| {
        dev.images
            .get(&image)
            .map(|i| {
                let bytes = i.format.bytes_per_texel().unwrap_or(4) as u64;
                (0..i.mip_levels).fold(0u64, |total, mip| {
                    let width = (i.width >> mip).max(1) as u64;
                    let height = (i.height >> mip).max(1) as u64;
                    let depth = (i.depth >> mip).max(1) as u64;
                    total.saturating_add(
                        width
                            .saturating_mul(height)
                            .saturating_mul(depth)
                            .saturating_mul(i.layers as u64)
                            .saturating_mul(bytes),
                    )
                })
            })
            .unwrap_or(0)
    })
    .unwrap_or(0);
    out.size = size;
    out.alignment = 256;
    // Every advertised memory type can back this image (all our memory is host RAM): expose the full
    // set so gpu-alloc picks a suitable (e.g. DEVICE_LOCAL) type. See PhysicalDeviceDesc::memory_types.
    out.memory_type_bits = StateStore::with(|s| s.physical_device().all_memory_type_bits());
}

/// `vkGetImageSubresourceLayout` — report the linear byte layout (offset/size/rowPitch) of `image`'s
/// subresource. Modeled images are single-mip single-layer RGBA8 2D targets (rowPitch = width*4). Leaves
/// the output zeroed on an unknown image (the caller must have queried a valid, linear-tiled image).
pub extern "C" fn vkGetImageSubresourceLayout(
    _device: *mut c_void,
    image: u64,
    _p_subresource: *const c_void,
    p_layout: *mut c_void,
) {
    let Some(out) = (unsafe { (p_layout as *mut VkSubresourceLayout).as_mut() }) else {
        return;
    };
    *out = VkSubresourceLayout::default();
    if let Some(Ok(l)) = ShimState::with_device(|d| d.image_subresource_layout(image)) {
        out.offset = l.offset;
        out.size = l.size;
        out.row_pitch = l.row_pitch;
        out.array_pitch = l.array_pitch;
        out.depth_pitch = l.depth_pitch;
    }
}

pub extern "C" fn vkBindImageMemory(
    _device: *mut c_void,
    _image: u64,
    _memory: u64,
    _memory_offset: u64,
) -> VkResult {
    // Images are host-owned render targets in this model (no explicit VkDeviceMemory backing); the bind
    // is a no-op that succeeds so a conventional create→bind flow proceeds.
    VK_SUCCESS
}

// ---- bind-memory-2 / memory-requirements-2 for images (core 1.1 / KHR) — delegate to the v1 bodies

/// `vkBindImageMemory2` — bind each `VkBindImageMemoryInfo` via the v1 [`vkBindImageMemory`] body (a
/// host-owned render-target image binds as a no-op success). Returns the first error (else `VK_SUCCESS`).
pub extern "C" fn vkBindImageMemory2(
    device: *mut c_void,
    bind_info_count: u32,
    p_bind_infos: *const c_void,
) -> VkResult {
    if p_bind_infos.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe {
        std::slice::from_raw_parts(
            p_bind_infos as *const VkBindImageMemoryInfo,
            bind_info_count as usize,
        )
    };
    let mut result = VK_SUCCESS;
    for bi in infos {
        let r = vkBindImageMemory(device, bi.image, bi.memory, bi.memory_offset);
        if r != VK_SUCCESS {
            result = r;
        }
    }
    result
}

/// `vkBindImageMemory2KHR` — the `VK_KHR_bind_memory2` alias of [`vkBindImageMemory2`].
pub extern "C" fn vkBindImageMemory2KHR(
    device: *mut c_void,
    bind_info_count: u32,
    p_bind_infos: *const c_void,
) -> VkResult {
    vkBindImageMemory2(device, bind_info_count, p_bind_infos)
}

/// `vkGetImageMemoryRequirements2` — read `VkImageMemoryRequirementsInfo2` and fill the base
/// `VkMemoryRequirements` via the v1 [`vkGetImageMemoryRequirements`] body, then answer the chained outputs.
pub extern "C" fn vkGetImageMemoryRequirements2(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    let Some(info) = (unsafe { (p_info as *const VkImageMemoryRequirementsInfo2).as_ref() }) else {
        return;
    };
    let Some(out) = (unsafe { (p_memory_requirements as *mut VkMemoryRequirements2).as_mut() })
    else {
        return;
    };
    vkGetImageMemoryRequirements(
        device,
        info.image,
        &mut out.memory_requirements as *mut _ as *mut c_void,
    );
    // The chain is not "preserved" by leaving it alone: everything on it is an OUTPUT this driver owes
    // the caller, and skipping it returns the caller's uninitialised stack as an answer.
    out.answer_chain();
}

/// `vkGetImageMemoryRequirements2KHR` — the `VK_KHR_get_memory_requirements2` alias.
pub extern "C" fn vkGetImageMemoryRequirements2KHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    vkGetImageMemoryRequirements2(device, p_info, p_memory_requirements)
}

/// Which wire texture dimension a `VkImageViewType` over an image of `image_dim` with `layer_count`
/// layers resolves to, or `None` when this driver cannot view that combination.
///
/// Extracted from `vkCreateImageView` so it can be tested: the entry point needs a live device and a
/// host sink, so an inline match here was reachable only from a running system, and the 1D arm below was
/// missing for exactly as long as nothing could notice.
///
/// Vulkan 1D images are represented by one-row 2D textures in the executor. A plain 1D view selects one
/// layer and becomes a D2 view; a 1D-array view remains logical [`TextureDim::D1`] here and becomes a
/// D2Array view when its range has multiple layers. Keeping the logical dimension is load-bearing: shader
/// lowering uses it to expand scalar 1D coordinates while preserving the separate array index.
fn view_dimension(view_type: i32, image_dim: TextureDim, layer_count: u32) -> Option<TextureDim> {
    Some(match view_type {
        0 if image_dim == TextureDim::D1 && layer_count == 1 => TextureDim::D1,
        4 if image_dim == TextureDim::D1 => TextureDim::D1,
        1 if matches!(image_dim, TextureDim::D2 | TextureDim::Cube) && layer_count == 1 => {
            TextureDim::D2
        }
        5 if matches!(image_dim, TextureDim::D2 | TextureDim::Cube) => TextureDim::D2,
        3 if image_dim == TextureDim::Cube && layer_count == 6 => TextureDim::Cube,
        6 if image_dim == TextureDim::Cube => TextureDim::Cube,
        2 if image_dim == TextureDim::D3 => TextureDim::D3,
        _ => return None,
    })
}

pub extern "C" fn vkCreateImageView(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_view: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkImageViewCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_view.is_null() {
        unsafe { *p_view = 0 };
    }
    let handle = StateStore::with(|s| {
        let (h, ir_id, mut image, view) = {
            let dev = s.device_mut()?;
            let image = dev.images.get(&ci.image)?.clone();
            let base_mip = ci.subresource_range.base_mip_level;
            let base_layer = ci.subresource_range.base_array_layer;
            let mip_count = if ci.subresource_range.level_count == u32::MAX {
                image.mip_levels.checked_sub(base_mip)?
            } else {
                ci.subresource_range.level_count
            };
            let layer_count = if ci.subresource_range.layer_count == u32::MAX {
                image.layers.checked_sub(base_layer)?
            } else {
                ci.subresource_range.layer_count
            };
            if mip_count == 0
                || layer_count == 0
                || base_mip.checked_add(mip_count)? > image.mip_levels
                || base_layer.checked_add(layer_count)? > image.layers
            {
                return None;
            }
            let dim = view_dimension(ci.view_type, image.dim, layer_count)?;
            if image.dim == TextureDim::D3 && (base_layer != 0 || layer_count != 1) {
                return None;
            }
            if matches!(dim, TextureDim::Cube)
                && (layer_count < 6
                    || !layer_count.is_multiple_of(6)
                    || !base_layer.is_multiple_of(6))
            {
                return None;
            }
            let format = Format(ci.format as u32).wire()?;
            if format != image.format {
                return None;
            }
            let aspect = match (format, ci.subresource_range.aspect_mask) {
                (TextureFormat::Depth32Float, 2) => TextureAspect::DepthOnly,
                (TextureFormat::Depth24PlusStencil8, 2) => TextureAspect::DepthOnly,
                (TextureFormat::Depth24PlusStencil8, 4) => TextureAspect::StencilOnly,
                (TextureFormat::Depth24PlusStencil8, 6) => TextureAspect::All,
                (TextureFormat::Depth32Float | TextureFormat::Depth24PlusStencil8, _) => {
                    return None
                }
                (_, 1) => TextureAspect::All,
                _ => return None,
            };
            let h = dev.alloc_handle();
            let ir_id = dev.alloc_ir();
            let view_layer_count = if image.dim == TextureDim::D3 {
                (image.depth >> base_mip).max(1)
            } else {
                layer_count
            };
            (
                h,
                ir_id,
                image,
                TextureViewDesc {
                    texture: dev.images.get(&ci.image)?.ir_id,
                    dim,
                    format,
                    aspect,
                    base_mip,
                    mip_count,
                    base_layer,
                    layer_count: view_layer_count,
                },
            )
        };
        s.sink.submit(&[Cmd::CreateTextureView(ir_id, view)]).ok()?;
        image.ir_id = ir_id;
        image.width = (image.width >> ci.subresource_range.base_mip_level).max(1);
        image.height = (image.height >> ci.subresource_range.base_mip_level).max(1);
        image.depth = (image.depth >> ci.subresource_range.base_mip_level).max(1);
        image.mip_levels = if ci.subresource_range.level_count == u32::MAX {
            image
                .mip_levels
                .saturating_sub(ci.subresource_range.base_mip_level)
        } else {
            ci.subresource_range.level_count
        };
        image.layers = if ci.subresource_range.layer_count == u32::MAX {
            image
                .layers
                .saturating_sub(ci.subresource_range.base_array_layer)
        } else {
            ci.subresource_range.layer_count
        };
        s.device_mut()?.images.insert(h, image);
        s.image_views.insert(h, h);
        Some(h)
    });
    match handle {
        Some(h) => {
            if !p_view.is_null() {
                unsafe { *p_view = h };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

pub extern "C" fn vkDestroyImageView(
    _device: *mut c_void,
    image_view: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        let Some(alias) = s.image_views.remove(&image_view) else {
            return;
        };
        let Some(image) = s.device_mut().and_then(|dev| dev.images.remove(&alias)) else {
            return;
        };
        let _ = s.sink.submit(&[Cmd::DestroyTextureView(image.ir_id)]);
    });
}

pub extern "C" fn vkCreateSampler(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_sampler: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkSamplerCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let h = ShimState::with_sink(|dev, sink| {
        create::create_sampler_full(
            dev,
            sink,
            ci.min_filter as u32,
            ci.mag_filter as u32,
            ci.mipmap_mode as u32,
            [
                ci.address_mode_u as u32,
                ci.address_mode_v as u32,
                ci.address_mode_w as u32,
            ],
            [ci.min_lod, ci.max_lod],
            ci.border_color as u32,
            (ci.compare_enable != 0).then_some(ci.compare_op as u32),
        )
    });
    match h {
        Some(handle) => {
            if !p_sampler.is_null() {
                unsafe { *p_sampler = handle };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

pub extern "C" fn vkDestroySampler(
    _device: *mut c_void,
    sampler: u64,
    _p_allocator: *const c_void,
) {
    ShimState::with_sink(|dev, sink| {
        let _ = create::destroy_sampler(dev, sink, sampler);
    });
}

#[cfg(test)]
mod view_dimension_tests {
    use super::{view_dimension, TextureDim};

    /// The 1D arm this file was missing. Image creation and the format query both learned 1D; the view
    /// path did not, so a 1D image was creatable and then unviewable — and an image clear needs a view.
    #[test]
    fn a_single_layer_1d_image_has_a_1d_view() {
        assert_eq!(
            view_dimension(0, TextureDim::D1, 1),
            Some(TextureDim::D1),
            "VK_IMAGE_VIEW_TYPE_1D over a 1D image"
        );
    }

    /// Vulkan 1D arrays are backed by one-row 2D arrays. The neutral D1 view keeps Vulkan's logical
    /// dimension while the executor selects D2Array when more than one layer is named.
    #[test]
    fn a_layered_1d_image_has_a_1d_array_view() {
        assert_eq!(view_dimension(0, TextureDim::D1, 2), None, "plain 1D type");
        assert_eq!(
            view_dimension(4, TextureDim::D1, 1),
            Some(TextureDim::D1),
            "single-layer 1D_ARRAY type"
        );
        assert_eq!(
            view_dimension(4, TextureDim::D1, 4),
            Some(TextureDim::D1),
            "layered 1D_ARRAY type"
        );
    }

    /// The pre-existing arms must be untouched by the extraction — this is the control that the refactor
    /// moved the match rather than rewriting it.
    #[test]
    fn the_existing_view_types_still_resolve_as_before() {
        assert_eq!(view_dimension(1, TextureDim::D2, 1), Some(TextureDim::D2));
        assert_eq!(view_dimension(5, TextureDim::D2, 4), Some(TextureDim::D2));
        assert_eq!(
            view_dimension(3, TextureDim::Cube, 6),
            Some(TextureDim::Cube)
        );
        assert_eq!(
            view_dimension(6, TextureDim::Cube, 12),
            Some(TextureDim::Cube)
        );
        assert_eq!(view_dimension(2, TextureDim::D3, 1), Some(TextureDim::D3));
        // And the mismatches stay refused.
        assert_eq!(view_dimension(1, TextureDim::D3, 1), None);
        assert_eq!(view_dimension(3, TextureDim::D2, 6), None);
        assert_eq!(view_dimension(2, TextureDim::D2, 1), None);
        assert_eq!(view_dimension(99, TextureDim::D2, 1), None);
    }
}
