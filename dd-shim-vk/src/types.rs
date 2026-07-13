//! The handful of Vulkan C-ABI aliases the hand-written entry points spell in their signatures.
//!
//! Dispatchable handles are opaque pointers (to our [`crate::handle::Dispatchable`] objects);
//! non-dispatchable handles are 64-bit. The big by-value structs the real bodies read/write
//! (`VkPhysicalDeviceProperties`, `VkInstanceCreateInfo`, …) come from `ash::vk` — spec-exact,
//! `#[repr(C)]`, so the ABI matches a real loader/app byte-for-byte — rather than being
//! hand-transcribed here. `VkResult`/enum *values* likewise come from `ash::vk`.

use core::ffi::c_void;

// ---- dispatchable handles (pointer to a loader-magic'd object) -----------------------------------
pub type VkInstance = *mut c_void;
pub type VkPhysicalDevice = *mut c_void;
pub type VkDevice = *mut c_void;
pub type VkQueue = *mut c_void;
pub type VkCommandBuffer = *mut c_void;

// ---- non-dispatchable handles (opaque u64) -------------------------------------------------------
pub type VkCommandPool = u64;
pub type VkBuffer = u64;
pub type VkDeviceMemory = u64;
pub type VkImage = u64;
pub type VkImageView = u64;
pub type VkShaderModule = u64;
pub type VkPipeline = u64;
pub type VkPipelineLayout = u64;
pub type VkRenderPass = u64;
pub type VkFramebuffer = u64;
pub type VkDescriptorSetLayout = u64;
pub type VkDescriptorPool = u64;
pub type VkDescriptorSet = u64;
pub type VkFence = u64;
pub type VkSemaphore = u64;
pub type VkSampler = u64;
pub type VkEvent = u64;
pub type VkQueryPool = u64;
pub type VkBufferView = u64;
pub type VkPipelineCache = u64;

// ---- scalars -------------------------------------------------------------------------------------
pub type VkResult = i32;
pub type VkFlags = u32;
pub type VkBool32 = u32;

// A couple of VkResult values the bring-up path returns, spelled numerically to avoid depending on
// ash enum-to-i32 casts at every call site. Values are the stable Vulkan ABI (from `vk.xml`).
pub const VK_SUCCESS: VkResult = 0;
/// A fence is unsignaled, or a command buffer is not ready to record/execute (spec: `VK_NOT_READY` = 1).
pub const VK_NOT_READY: VkResult = 1;
/// `vkWaitForFences` reached its timeout before the fence(s) signaled (spec: `VK_TIMEOUT` = 2).
pub const VK_TIMEOUT: VkResult = 2;
pub const VK_INCOMPLETE: VkResult = 5;
pub const VK_ERROR_OUT_OF_HOST_MEMORY: VkResult = -1;
/// An allocation or binding could not be satisfied from device memory (spec: `-2`).
pub const VK_ERROR_OUT_OF_DEVICE_MEMORY: VkResult = -2;
pub const VK_ERROR_INITIALIZATION_FAILED: VkResult = -3;
/// The synchronous executor connection was lost or rejected submitted work.
pub const VK_ERROR_DEVICE_LOST: VkResult = -4;
/// `vkMapMemory` could not map the requested range — bad range / already mapped / not host-visible (`-5`).
pub const VK_ERROR_MEMORY_MAP_FAILED: VkResult = -5;
/// The truthful failure a generated stub returns for a command from an extension the ICD does not
/// advertise (Phase-0 truthfulness; see `build.rs` + `crate::capability`).
pub const VK_ERROR_EXTENSION_NOT_PRESENT: VkResult = -7;
/// The truthful failure a generated stub returns for an unimplemented core command.
pub const VK_ERROR_FEATURE_NOT_PRESENT: VkResult = -8;
pub const VK_ERROR_INCOMPATIBLE_DRIVER: VkResult = -9;
/// A descriptor pool has no room for the requested sets (spec: `-1000069000`).
pub const VK_ERROR_OUT_OF_POOL_MEMORY: VkResult = -1000069000;
/// A pipeline/object could not be created for a reason without a more specific code (spec: `-13`).
/// Used to reject a graphics pipeline whose shader stages/render pass are invalid rather than
/// substituting a default (zero) module.
pub const VK_ERROR_UNKNOWN: VkResult = -13;
/// The referenced presentation surface no longer exists (`VK_KHR_surface`).
pub const VK_ERROR_SURFACE_LOST_KHR: VkResult = -1000000000;
/// The supplied native window is already owned by another Vulkan surface.
pub const VK_ERROR_NATIVE_WINDOW_IN_USE_KHR: VkResult = -1000000001;
/// A swapchain can no longer acquire images because it has been replaced or resized.
pub const VK_ERROR_OUT_OF_DATE_KHR: VkResult = -1000001004;
