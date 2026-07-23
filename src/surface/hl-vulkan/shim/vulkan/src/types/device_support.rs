use super::*;

/// `VkDescriptorUpdateTemplateEntry` — one entry mapping a `pData` slice (`offset`/`stride`) to a
/// `(dstBinding, dstArrayElement)` of a given descriptor class.
#[repr(C)]
pub struct VkDescriptorUpdateTemplateEntry {
    pub dst_binding: u32,
    pub dst_array_element: u32,
    pub descriptor_count: u32,
    pub descriptor_type: i32,
    pub offset: usize,
    pub stride: usize,
}

/// `VkDescriptorUpdateTemplateCreateInfo` — the entry table + template type (`DESCRIPTOR_SET` = 0).
#[repr(C)]
pub struct VkDescriptorUpdateTemplateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub descriptor_update_entry_count: u32,
    pub p_descriptor_update_entries: *const VkDescriptorUpdateTemplateEntry,
    pub template_type: i32,
    pub descriptor_set_layout: u64,
    pub pipeline_bind_point: i32,
    pub pipeline_layout: u64,
    pub set: u32,
}

/// `VkPipelineCacheCreateInfo` — the optional serialized `initialData` blob a cache is restored from.
#[repr(C)]
pub struct VkPipelineCacheCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub initial_data_size: usize,
    pub p_initial_data: *const c_void,
}

/// `VkDeviceQueueInfo2` — the `(flags, queueFamilyIndex, queueIndex)` a `vkGetDeviceQueue2` retrieves.
#[repr(C)]
pub struct VkDeviceQueueInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub queue_family_index: u32,
    pub queue_index: u32,
}

/// `VkImageSubresource` — the `(aspect, mip, arrayLayer)` a `vkGetImageSubresourceLayout` queries.
#[repr(C)]
pub struct VkImageSubresource {
    pub aspect_mask: VkFlags,
    pub mip_level: u32,
    pub array_layer: u32,
}

/// `VkSubresourceLayout` — the linear byte layout (offset/size/pitches) written back to the app.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkSubresourceLayout {
    pub offset: VkDeviceSize,
    pub size: VkDeviceSize,
    pub row_pitch: VkDeviceSize,
    pub array_pitch: VkDeviceSize,
    pub depth_pitch: VkDeviceSize,
}

/// `VkSurfaceCapabilitiesKHR` (`VK_KHR_surface`) — the min/max image count, extents, transforms + usage.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkSurfaceCapabilitiesKHR {
    pub min_image_count: u32,
    pub max_image_count: u32,
    pub current_extent: VkExtent2D,
    pub min_image_extent: VkExtent2D,
    pub max_image_extent: VkExtent2D,
    pub max_image_array_layers: u32,
    pub supported_transforms: VkFlags,
    pub current_transform: VkFlags,
    pub supported_composite_alpha: VkFlags,
    pub supported_usage_flags: VkFlags,
}

/// `VkSurfaceFormatKHR` — one `(format, colorSpace)` a surface presents.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkSurfaceFormatKHR {
    pub format: i32,
    pub color_space: i32,
}

// ==================================================================================================
// maintenance1-5 / private-data / host-image-copy / promoted-`...2` structs (layout from vk.xml)
// ==================================================================================================

/// `VkDescriptorSetLayoutSupport` — `vkGetDescriptorSetLayoutSupport` writes `supported` (whether a set
/// of the queried layout can be created). The bring-up model accepts any layout within the reported
/// limits, so this is `VK_TRUE`.
#[repr(C)]
pub struct VkDescriptorSetLayoutSupport {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub supported: VkBool32,
}

/// `VkDeviceBufferMemoryRequirements` (`vkGetDeviceBufferMemoryRequirements` input) — the buffer's
/// create info, from which the requirements are derived WITHOUT creating the buffer.
#[repr(C)]
pub struct VkDeviceBufferMemoryRequirements {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub p_create_info: *const VkBufferCreateInfo,
}

/// `VkDeviceImageMemoryRequirements` (`vkGetDeviceImageMemoryRequirements` input) — the image's create
/// info + the plane aspect (single-plane here).
#[repr(C)]
pub struct VkDeviceImageMemoryRequirements {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub p_create_info: *const VkImageCreateInfo,
    pub plane_aspect: i32,
}

/// `VkImageSubresource2(KHR)` — the `vkGetImageSubresourceLayout2` input (a `VkImageSubresource` + chain).
#[repr(C)]
pub struct VkImageSubresource2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub image_subresource: VkImageSubresource,
}

/// `VkSubresourceLayout2(KHR)` — the `vkGetImageSubresourceLayout2` output (base layout + chain).
#[repr(C)]
pub struct VkSubresourceLayout2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub subresource_layout: VkSubresourceLayout,
}

/// `VkImageCopy2` — one `vkCmdCopyImage2` region (`VkImageCopy` + chain header).
#[repr(C)]
pub struct VkImageCopy2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_subresource: VkImageSubresourceLayers,
    pub src_offset: VkOffset3D,
    pub dst_subresource: VkImageSubresourceLayers,
    pub dst_offset: VkOffset3D,
    pub extent: VkExtent3D,
}

/// `VkCopyImageInfo2` — the `vkCmdCopyImage2` argument aggregate.
#[repr(C)]
pub struct VkCopyImageInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_image: u64,
    pub src_image_layout: i32,
    pub dst_image: u64,
    pub dst_image_layout: i32,
    pub region_count: u32,
    pub p_regions: *const VkImageCopy2,
}

/// `VkCopyImageToBufferInfo2` — the `vkCmdCopyImageToBuffer2` argument aggregate (reuses `VkBufferImageCopy2`).
#[repr(C)]
pub struct VkCopyImageToBufferInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_image: u64,
    pub src_image_layout: i32,
    pub dst_buffer: u64,
    pub region_count: u32,
    pub p_regions: *const VkBufferImageCopy2,
}
