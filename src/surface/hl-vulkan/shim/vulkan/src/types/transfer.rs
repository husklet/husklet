use super::*;

/// `VK_IMAGE_ASPECT_COLOR_BIT` (the only aspect the software oracle materializes).
pub const VK_IMAGE_ASPECT_COLOR_BIT: u32 = 0x0000_0001;
/// `VK_FILTER_LINEAR` (`vkCmdBlitImage` filter; `VK_FILTER_NEAREST` = 0).
pub const VK_FILTER_LINEAR: i32 = 1;

/// A `pNext`-chain input node header (`{ sType, pNext }`, const) — every extension struct begins with it.
#[repr(C)]
pub struct VkBaseInStructure {
    pub s_type: i32,
    pub p_next: *const VkBaseInStructure,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkOffset3D {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// `VkImageSubresourceLayers` — the (aspect, mip, layers) a copy/blit region reads or writes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkImageSubresourceLayers {
    pub aspect_mask: VkFlags,
    pub mip_level: u32,
    pub base_array_layer: u32,
    pub layer_count: u32,
}

#[repr(C)]
pub struct VkBufferCopy {
    pub src_offset: VkDeviceSize,
    pub dst_offset: VkDeviceSize,
    pub size: VkDeviceSize,
}

#[repr(C)]
pub struct VkBufferImageCopy {
    pub buffer_offset: VkDeviceSize,
    pub buffer_row_length: u32,
    pub buffer_image_height: u32,
    pub image_subresource: VkImageSubresourceLayers,
    pub image_offset: VkOffset3D,
    pub image_extent: VkExtent3D,
}

#[repr(C)]
pub struct VkImageCopy {
    pub src_subresource: VkImageSubresourceLayers,
    pub src_offset: VkOffset3D,
    pub dst_subresource: VkImageSubresourceLayers,
    pub dst_offset: VkOffset3D,
    pub extent: VkExtent3D,
}

#[repr(C)]
pub struct VkImageBlit {
    pub src_subresource: VkImageSubresourceLayers,
    pub src_offsets: [VkOffset3D; 2],
    pub dst_subresource: VkImageSubresourceLayers,
    pub dst_offsets: [VkOffset3D; 2],
}

/// `VkClearColorValue` is a 16-byte union; the color-clear path reads it as `float32[4]`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkClearColorValue {
    pub float32: [f32; 4],
}

/// `VkClearDepthStencilValue` — the depth (`f32`) + stencil (`u32`) a `vkCmdClearDepthStencilImage` clears to.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkClearDepthStencilValue {
    pub depth: f32,
    pub stencil: u32,
}

#[repr(C)]
pub struct VkClearAttachment {
    pub aspect_mask: VkFlags,
    pub color_attachment: u32,
    pub clear_value: VkClearValue,
}

#[repr(C)]
pub struct VkClearRect {
    pub rect: VkRect2D,
    pub base_array_layer: u32,
    pub layer_count: u32,
}

// ---- the `...2` copy/blit wrappers (core 1.3 / VK_KHR_copy_commands2) ----------------------------
// Each `vkCmd*2` takes a single `Vk*Info2` aggregate whose `pRegions` array holds `Vk*Copy2`/`Vk*Blit2`
// structs — the same region payload as the v1 command, prefixed by a `{ sType, pNext }` node header. The
// `...2` entry points read these and delegate to the identical v1 lowering. Layout from vk.xml.

/// `VkBufferCopy2` — one `vkCmdCopyBuffer2` region ( `VkBufferCopy` + chain header).
#[repr(C)]
pub struct VkBufferCopy2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_offset: VkDeviceSize,
    pub dst_offset: VkDeviceSize,
    pub size: VkDeviceSize,
}

/// `VkCopyBufferInfo2` — the `vkCmdCopyBuffer2` argument aggregate.
#[repr(C)]
pub struct VkCopyBufferInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_buffer: u64,
    pub dst_buffer: u64,
    pub region_count: u32,
    pub p_regions: *const VkBufferCopy2,
}

/// `VkBufferImageCopy2` — one `vkCmdCopyBufferToImage2` region (`VkBufferImageCopy` + chain header).
#[repr(C)]
pub struct VkBufferImageCopy2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub buffer_offset: VkDeviceSize,
    pub buffer_row_length: u32,
    pub buffer_image_height: u32,
    pub image_subresource: VkImageSubresourceLayers,
    pub image_offset: VkOffset3D,
    pub image_extent: VkExtent3D,
}

/// `VkCopyBufferToImageInfo2` — the `vkCmdCopyBufferToImage2` argument aggregate.
#[repr(C)]
pub struct VkCopyBufferToImageInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_buffer: u64,
    pub dst_image: u64,
    pub dst_image_layout: i32,
    pub region_count: u32,
    pub p_regions: *const VkBufferImageCopy2,
}

/// `VkImageBlit2` — one `vkCmdBlitImage2` region (`VkImageBlit` + chain header).
#[repr(C)]
pub struct VkImageBlit2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_subresource: VkImageSubresourceLayers,
    pub src_offsets: [VkOffset3D; 2],
    pub dst_subresource: VkImageSubresourceLayers,
    pub dst_offsets: [VkOffset3D; 2],
}

/// `VkBlitImageInfo2` — the `vkCmdBlitImage2` argument aggregate.
#[repr(C)]
pub struct VkBlitImageInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_image: u64,
    pub src_image_layout: i32,
    pub dst_image: u64,
    pub dst_image_layout: i32,
    pub region_count: u32,
    pub p_regions: *const VkImageBlit2,
    pub filter: i32,
}

/// `VkImageMemoryBarrier` (legacy / core 1.0) — an image's `oldLayout → newLayout` transition.
#[repr(C)]
pub struct VkImageMemoryBarrier {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_access_mask: VkFlags,
    pub dst_access_mask: VkFlags,
    pub old_layout: i32,
    pub new_layout: i32,
    pub src_queue_family_index: u32,
    pub dst_queue_family_index: u32,
    pub image: u64,
    pub subresource_range: VkImageSubresourceRange,
}

/// `VkImageMemoryBarrier2` (synchronization2 / core 1.3) — per-barrier 64-bit stage/access masks.
#[repr(C)]
pub struct VkImageMemoryBarrier2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_stage_mask: u64,
    pub src_access_mask: u64,
    pub dst_stage_mask: u64,
    pub dst_access_mask: u64,
    pub old_layout: i32,
    pub new_layout: i32,
    pub src_queue_family_index: u32,
    pub dst_queue_family_index: u32,
    pub image: u64,
    pub subresource_range: VkImageSubresourceRange,
}

/// `VkDependencyInfo` — the `vkCmdPipelineBarrier2` argument aggregating the sync2 barrier arrays.
#[repr(C)]
pub struct VkDependencyInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub dependency_flags: VkFlags,
    pub memory_barrier_count: u32,
    pub p_memory_barriers: *const c_void,
    pub buffer_memory_barrier_count: u32,
    pub p_buffer_memory_barriers: *const c_void,
    pub image_memory_barrier_count: u32,
    pub p_image_memory_barriers: *const VkImageMemoryBarrier2,
}

// ==================================================================================================
// sync + query object structs (events, timeline semaphores, query pools) — layout from vk.xml
// ==================================================================================================
