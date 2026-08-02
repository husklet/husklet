use super::*;

/// `VkCommandBufferBeginInfo` — the `vkBeginCommandBuffer` info. Only `flags` is consumed by the model
/// (the `ONE_TIME_SUBMIT` bit decides whether a completed submit returns the buffer to `Executable` for
/// re-submission or leaves it non-resubmittable); `pInheritanceInfo` is validated-then-ignored.
#[repr(C)]
pub struct VkCommandBufferBeginInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub p_inheritance_info: *const c_void,
}

/// `VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT` — the buffer is recorded for a single submission.
pub const VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT: VkFlags = 0x0000_0001;

/// `VkCommandBufferSubmitInfo` — one command buffer of a `vkQueueSubmit2` batch (a dispatchable handle).
#[repr(C)]
pub struct VkCommandBufferSubmitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub command_buffer: *mut c_void,
    pub device_mask: u32,
}

/// `VkSemaphoreSubmitInfo` (sync2) — one wait/signal entry of a `vkQueueSubmit2` batch. Carries the
/// timeline `value` inline (unlike v1's side-array `VkTimelineSemaphoreSubmitInfo`).
#[repr(C)]
pub struct VkSemaphoreSubmitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub semaphore: u64,
    pub value: u64,
    pub stage_mask: u64,
    pub device_index: u32,
}

/// `VkSubmitInfo2` — the `vkQueueSubmit2` batch (sync2). The command-buffer array and the signal
/// semaphore infos (for queue-side timeline signals) are consumed; the wait infos are validated-then-
/// ignored (the synchronous model has already satisfied them).
#[repr(C)]
pub struct VkSubmitInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub wait_semaphore_info_count: u32,
    pub p_wait_semaphore_infos: *const c_void,
    pub command_buffer_info_count: u32,
    pub p_command_buffer_infos: *const VkCommandBufferSubmitInfo,
    pub signal_semaphore_info_count: u32,
    pub p_signal_semaphore_infos: *const c_void,
}

/// `VkMappedMemoryRange` — one range for `vkFlush/InvalidateMappedMemoryRanges` (`size == VK_WHOLE_SIZE`
/// means from `offset` to the end of the allocation).
#[repr(C)]
pub struct VkMappedMemoryRange {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub memory: u64,
    pub offset: VkDeviceSize,
    pub size: VkDeviceSize,
}

/// `VkMemoryMapInfo(KHR)` — the `vkMapMemory2` argument aggregate.
#[repr(C)]
pub struct VkMemoryMapInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub memory: u64,
    pub offset: VkDeviceSize,
    pub size: VkDeviceSize,
}

/// `VkMemoryUnmapInfo(KHR)` — the `vkUnmapMemory2` argument aggregate.
#[repr(C)]
pub struct VkMemoryUnmapInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub memory: u64,
}

/// `VkPhysicalDeviceImageFormatInfo2` — the `vkGetPhysicalDeviceImageFormatProperties2` input.
#[repr(C)]
pub struct VkPhysicalDeviceImageFormatInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub format: i32,
    pub image_type: i32,
    pub tiling: i32,
    pub usage: VkFlags,
    pub flags: VkFlags,
}

/// `VkImageFormatProperties2` — the `...ImageFormatProperties2` output (base props + preserved chain).
#[repr(C)]
pub struct VkImageFormatProperties2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub image_format_properties: VkImageFormatProperties,
}

/// `VkAttachmentDescription2` — one attachment of a `VkRenderPassCreateInfo2` (`+ sType/pNext` vs v1).
#[repr(C)]
pub struct VkAttachmentDescription2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub format: i32,
    pub samples: i32,
    pub load_op: i32,
    pub store_op: i32,
    pub stencil_load_op: i32,
    pub stencil_store_op: i32,
    pub initial_layout: i32,
    pub final_layout: i32,
}

#[repr(C)]
pub struct VkAttachmentReference2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub attachment: u32,
    pub layout: i32,
    pub aspect_mask: VkFlags,
}

#[repr(C)]
pub struct VkSubpassDescription2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub pipeline_bind_point: i32,
    pub view_mask: u32,
    pub input_attachment_count: u32,
    pub p_input_attachments: *const VkAttachmentReference2,
    pub color_attachment_count: u32,
    pub p_color_attachments: *const VkAttachmentReference2,
    pub p_resolve_attachments: *const VkAttachmentReference2,
    pub p_depth_stencil_attachment: *const VkAttachmentReference2,
    pub preserve_attachment_count: u32,
    pub p_preserve_attachments: *const u32,
}

/// `VkRenderPassCreateInfo2` — the `vkCreateRenderPass2` argument (only the attachment table is read for
/// the bring-up single-target clear/format bookkeeping).
#[repr(C)]
pub struct VkRenderPassCreateInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub attachment_count: u32,
    pub p_attachments: *const VkAttachmentDescription2,
    pub subpass_count: u32,
    pub p_subpasses: *const c_void,
    pub dependency_count: u32,
    pub p_dependencies: *const c_void,
    pub correlated_view_mask_count: u32,
    pub p_correlated_view_masks: *const u32,
}

/// `VkCalibratedTimestampInfoKHR` — one queried time domain of `vkGetCalibratedTimestamps`.
#[repr(C)]
pub struct VkCalibratedTimestampInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub time_domain: i32,
}

/// `VK_TIME_DOMAIN_DEVICE_KHR` — the only calibrateable time domain the modeled device reports.
pub const VK_TIME_DOMAIN_DEVICE_KHR: i32 = 0;
