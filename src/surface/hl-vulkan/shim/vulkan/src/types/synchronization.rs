use super::*;

/// `VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO` (the pNext node selecting a timeline semaphore).
pub const VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO: i32 = 1_000_207_002;
/// `VK_SEMAPHORE_TYPE_TIMELINE` (`VkSemaphoreTypeCreateInfo::semaphoreType`; BINARY = 0).
pub const VK_SEMAPHORE_TYPE_TIMELINE: i32 = 1;

/// `VkQueryResultFlagBits` (stable ABI).
pub const VK_QUERY_RESULT_64_BIT: u32 = 0x1;
pub const VK_QUERY_RESULT_WAIT_BIT: u32 = 0x2;
pub const VK_QUERY_RESULT_WITH_AVAILABILITY_BIT: u32 = 0x4;
pub const VK_QUERY_RESULT_PARTIAL_BIT: u32 = 0x8;
/// `VK_SEMAPHORE_WAIT_ANY_BIT` (`VkSemaphoreWaitFlags`).
pub const VK_SEMAPHORE_WAIT_ANY_BIT: u32 = 0x1;
pub const VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT: u32 = 0x1;
pub const VK_SEMAPHORE_IMPORT_TEMPORARY_BIT: u32 = 0x1;
pub const VK_STRUCTURE_TYPE_IMPORT_SEMAPHORE_FD_INFO_KHR: i32 = 1_000_079_000;
pub const VK_STRUCTURE_TYPE_SEMAPHORE_GET_FD_INFO_KHR: i32 = 1_000_079_001;

#[repr(C)]
pub struct VkEventCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
}

/// `VkSemaphoreTypeCreateInfo` — a `VkSemaphoreCreateInfo` pNext selecting BINARY/TIMELINE + initial value.
#[repr(C)]
pub struct VkSemaphoreTypeCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub semaphore_type: i32,
    pub initial_value: u64,
}

#[repr(C)]
pub struct VkSemaphoreSignalInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub semaphore: u64,
    pub value: u64,
}

/// `VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO` — the `VkSubmitInfo` pNext carrying the per-wait /
/// per-signal timeline VALUES paired positionally with `VkSubmitInfo::pWaitSemaphores` / `pSignalSemaphores`.
pub const VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO: i32 = 1_000_207_003;

/// `VkTimelineSemaphoreSubmitInfo` (`VK_KHR_timeline_semaphore`) — a `VkSubmitInfo` pNext supplying the
/// timeline counter values for the submit's wait/signal semaphore arrays. `pSignalSemaphoreValues[i]` is
/// the value queue completion advances `VkSubmitInfo::pSignalSemaphores[i]` to (ignored for a binary
/// semaphore); `pWaitSemaphoreValues[i]` is the value the submit waits `pWaitSemaphores[i]` to reach.
#[repr(C)]
pub struct VkTimelineSemaphoreSubmitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub wait_semaphore_value_count: u32,
    pub p_wait_semaphore_values: *const u64,
    pub signal_semaphore_value_count: u32,
    pub p_signal_semaphore_values: *const u64,
}

#[repr(C)]
pub struct VkSemaphoreWaitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub semaphore_count: u32,
    pub p_semaphores: *const u64,
    pub p_values: *const u64,
}

#[repr(C)]
pub struct VkSemaphoreGetFdInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub semaphore: u64,
    pub handle_type: u32,
}

#[repr(C)]
pub struct VkImportSemaphoreFdInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub semaphore: u64,
    pub flags: VkFlags,
    pub handle_type: u32,
    pub fd: i32,
}

#[repr(C)]
pub struct VkQueryPoolCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub query_type: i32,
    pub query_count: u32,
    pub pipeline_statistics: VkFlags,
}

// ==================================================================================================
// descriptor-update-template / pipeline-cache / device-queue-2 / WSI-surface structs (from vk.xml)
// ==================================================================================================
