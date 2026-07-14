//! hl-vulkan — the self-contained Vulkan guest driver crate.
//!
//! It does exactly ONE thing (goal.md, OVERVIEW-v2 §4/§5): **lower** an intercepted Vulkan operation
//! into the neutral hl-GPU IR and submit it through a [`hl_gpu::CommandSink`]. The host GPU computes;
//! this crate never touches Metal/Vulkan-runtime types. Its keystone is that a `VkShaderModule` **is**
//! SPIR-V and the IR shader ABI ([`hl_gpu::Cmd::CreateShader`]) is **also** SPIR-V — so Vulkan shaders
//! forward with ZERO translation (see [`adapter::spirv`]).
//!
//! ## Layering (v2 doctrine — mirrored across the cuda/vulkan/gl drivers)
//! * [`model`] — the Vulkan object model + its invariants: the physical-device descriptor
//!   ([`model::instance`]), the [`model::device::Device`] aggregate (the per-device handle tables +
//!   id counters), and the per-family records ([`model::memory`], [`model::pipeline`],
//!   [`model::descriptor`], [`model::command`], [`model::queue`]). Owned values; no `Cmd`
//!   construction, no transport.
//! * [`service`] — one Vulkan operation family per file: [`service::create`] (resource creation),
//!   [`service::record`] (`vkCmd*` recording), [`service::submit`] (`vkQueueSubmit`), and
//!   [`service::present`] (WSI). Each takes `&mut Device` + `&mut dyn CommandSink`, mutates the model,
//!   and submits the protocol `Cmd`s that operation lowers to. This is the tested lowering surface.
//! * [`adapter`] — external, tech-named mechanisms: [`adapter::spirv`] (a `VkShaderModule`'s `pCode`
//!   SPIR-V words → the IR shader payload, forwarded verbatim; plus SPIR-V header validation and
//!   `OpEntryPoint` name extraction).
//! * [`result`] — the Vulkan `VkResult` contract + the `GpuError` → `VkResult` map.
//!
//! ## Handles
//! To keep the lowering layer memory-safe and directly testable without FFI, non-dispatchable Vulkan
//! handles ([`VkBuffer`], [`VkImage`], …) are modeled as opaque `u64` typedefs (exactly a real ICD's
//! non-dispatchable-handle representation), minted by [`model::device::Device`]. The shim cdylib
//! (later pass) hands the same `u64` down across the C ABI.
//!
//! ## Scope of this staging pass
//! FULLY lowered: instance/physical-device/device create, `vkCreateBuffer`, `vkAllocateMemory` +
//! `vkBindBufferMemory`, `vkCreateImage`, `vkCreateSampler`, `vkCreateShaderModule` (SPIR-V forwarded),
//! `vkCreate{Compute,Graphics}Pipelines`, descriptor set → bind group, `vkCmd*` recording,
//! `vkQueueSubmit`, and `vkQueuePresentKHR`. Deferred to later passes (called out in the module docs):
//! the injectable ICD shim cdylib (`shim/`), the `build.rs` dual-arch cross-compile, and the
//! `hl_jit::Driver` plug (`Vulkan::new`/`inject`). Those are wiring, not lowering.

pub mod adapter;
#[cfg(feature = "jit")]
pub mod driver;
pub mod model;
pub mod result;
pub mod service;

// ---- Vulkan handle typedefs (non-dispatchable handles = opaque u64; VK_NULL_HANDLE == 0) ----------
// Ported from `hl-shim-vk/src/types.rs`. The lowering layer works with plain `u64`s (as the C ABI
// does) rather than `*mut c_void` dispatchable pointers, so it is testable without FFI.
pub type VkBuffer = u64;
pub type VkDeviceMemory = u64;
pub type VkImage = u64;
pub type VkSampler = u64;
pub type VkShaderModule = u64;
pub type VkPipeline = u64;
pub type VkPipelineLayout = u64;
pub type VkDescriptorSetLayout = u64;
pub type VkDescriptorPool = u64;
pub type VkDescriptorSet = u64;
pub type VkCommandBuffer = u64;
pub type VkFence = u64;
pub type VkSemaphore = u64;
pub type VkEvent = u64;
pub type VkQueryPool = u64;
pub type VkSurfaceKHR = u64;
pub type VkSwapchainKHR = u64;
pub type VkDescriptorUpdateTemplate = u64;
pub type VkPipelineCache = u64;

/// `VK_NULL_HANDLE` — the reserved null non-dispatchable handle.
pub const VK_NULL_HANDLE: u64 = 0;

// Ergonomic re-exports: downstream (and the shim) read `hl_vulkan::{Device, PhysicalDeviceDesc, …}`.
pub use model::device::Device;
pub use model::instance::{Instance, PhysicalDeviceDesc};
pub use model::pipeline::PipelineKind;

// The host-side driver plug (`engine.add(Vulkan::new(..))`). Behind the `jit` feature so the guest ICD
// shim never pulls hl-jit into its `.so`.
#[cfg(feature = "jit")]
pub use driver::{Arch, Vulkan, VulkanSpec};
