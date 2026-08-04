use super::*;

/// `VkBindBufferMemoryInfo` — one `vkBindBufferMemory2` binding.
#[repr(C)]
pub struct VkBindBufferMemoryInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub buffer: u64,
    pub memory: u64,
    pub memory_offset: VkDeviceSize,
}

/// `VkBindImageMemoryInfo` — one `vkBindImageMemory2` binding.
#[repr(C)]
pub struct VkBindImageMemoryInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub image: u64,
    pub memory: u64,
    pub memory_offset: VkDeviceSize,
}

/// `VkBufferMemoryRequirementsInfo2` — the `vkGetBufferMemoryRequirements2` input (the queried buffer).
#[repr(C)]
pub struct VkBufferMemoryRequirementsInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub buffer: u64,
}

/// `VkImageMemoryRequirementsInfo2` — the `vkGetImageMemoryRequirements2` input (the queried image).
#[repr(C)]
pub struct VkImageMemoryRequirementsInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub image: u64,
}

/// `VkMemoryRequirements2` — the `...Requirements2` output: base `VkMemoryRequirements` plus a chain of
/// further OUTPUT structures the implementation is required to fill in.
#[repr(C)]
pub struct VkMemoryRequirements2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub memory_requirements: VkMemoryRequirements,
}

/// `VK_STRUCTURE_TYPE_MEMORY_DEDICATED_REQUIREMENTS` (core 1.1).
pub const VK_STRUCTURE_TYPE_MEMORY_DEDICATED_REQUIREMENTS: i32 = 1_000_127_000;

/// `VkMemoryDedicatedRequirements` — chained onto `VkMemoryRequirements2` to ask whether this resource
/// wants, or insists on, its own `VkDeviceMemory` rather than a suballocation.
///
/// Both fields are OUTPUTS. A caller is not required to initialise an output structure, so a driver that
/// does not write them hands back whatever the caller's stack held, and a `VkBool32` that is neither 0
/// nor 1 is what comes out.
#[repr(C)]
pub struct VkMemoryDedicatedRequirements {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub prefers_dedicated_allocation: u32,
    pub requires_dedicated_allocation: u32,
}

impl VkMemoryRequirements2 {
    /// Answer every OUTPUT structure chained onto this one.
    ///
    /// Only `VkMemoryDedicatedRequirements` exists in core, and this driver never needs or prefers a
    /// dedicated allocation — every resource is a suballocation of ordinary host memory — so both
    /// answers are `VK_FALSE`. Writing them is not optional just because the answer is "no": the fields
    /// are outputs, a caller may legally pass the structure uninitialised, and a driver that skips them
    /// returns the caller's own stack as a capability. That is not hypothetical here —
    /// `dEQP-VK.memory.requirements.dedicated_allocation.buffer.regular` failed
    /// `validValueVkBool32(m_allUsageFlagsPrefersDedicatedAllocation)`, which fires only when the value
    /// read is neither 0 nor 1.
    pub fn answer_chain(&mut self) {
        let mut node = self.p_next as *mut VkBaseOutStructure;
        while let Some(n) = unsafe { node.as_mut() } {
            if n.s_type == VK_STRUCTURE_TYPE_MEMORY_DEDICATED_REQUIREMENTS {
                if let Some(d) = unsafe { (node as *mut VkMemoryDedicatedRequirements).as_mut() } {
                    d.prefers_dedicated_allocation = 0;
                    d.requires_dedicated_allocation = 0;
                }
            }
            node = n.p_next;
        }
    }
}

#[repr(C)]
pub struct VkSemaphoreCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
}

#[repr(C)]
pub struct VkSwapchainCreateInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub surface: u64,
    pub min_image_count: u32,
    pub image_format: i32,
    pub image_color_space: i32,
    pub image_extent: VkExtent2D,
    pub image_array_layers: u32,
    pub image_usage: VkFlags,
    pub image_sharing_mode: i32,
    pub queue_family_index_count: u32,
    pub p_queue_family_indices: *const u32,
    pub pre_transform: VkFlags,
    pub composite_alpha: VkFlags,
    pub present_mode: i32,
    pub clipped: VkBool32,
    pub old_swapchain: u64,
}

/// `VkWaylandSurfaceCreateInfoKHR` (`VK_KHR_wayland_surface`) — the app's native wayland handles the ICD
/// captures at `vkCreateWaylandSurfaceKHR`: `display` is the app's `wl_display*` and `surface` its
/// `wl_surface*`, both on the app's OWN `libwayland-client` connection. The shim records these (never
/// dereferences them here) so `vkQueuePresentKHR` can marshal the presented frame onto that `wl_surface`.
#[repr(C)]
pub struct VkWaylandSurfaceCreateInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    /// `struct wl_display*` — the app's connection.
    pub display: *mut c_void,
    /// `struct wl_surface*` — the app window's surface.
    pub surface: *mut c_void,
}

#[repr(C)]
pub struct VkPresentInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub wait_semaphore_count: u32,
    pub p_wait_semaphores: *const u64,
    pub swapchain_count: u32,
    pub p_swapchains: *const u64,
    pub p_image_indices: *const u32,
    pub p_results: *mut i32,
}

pub const VK_STRUCTURE_TYPE_PRESENT_ID_KHR: i32 = 1_000_294_000;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PRESENT_ID_FEATURES_KHR: i32 = 1_000_294_001;

#[repr(C)]
pub struct VkPresentIdKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub swapchain_count: u32,
    pub p_present_ids: *const u64,
}

// ==================================================================================================
// transfer-path structs (buffer/image copies, blits, clears, pipeline barriers) — layout from vk.xml
// ==================================================================================================
