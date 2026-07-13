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

// ---- scalars -------------------------------------------------------------------------------------
pub type VkResult = i32;
pub type VkFlags = u32;
pub type VkBool32 = u32;

// A couple of VkResult values the bring-up path returns, spelled numerically to avoid depending on
// ash enum-to-i32 casts at every call site. Values are the stable Vulkan ABI (from `vk.xml`).
pub const VK_SUCCESS: VkResult = 0;
pub const VK_INCOMPLETE: VkResult = 5;
pub const VK_ERROR_OUT_OF_HOST_MEMORY: VkResult = -1;
pub const VK_ERROR_INITIALIZATION_FAILED: VkResult = -3;
/// The truthful failure a generated stub returns for a command from an extension the ICD does not
/// advertise (Phase-0 truthfulness; see `build.rs` + `crate::capability`).
pub const VK_ERROR_EXTENSION_NOT_PRESENT: VkResult = -7;
/// The truthful failure a generated stub returns for an unimplemented core command.
pub const VK_ERROR_FEATURE_NOT_PRESENT: VkResult = -8;
pub const VK_ERROR_INCOMPATIBLE_DRIVER: VkResult = -9;
