//! Vulkan result-code contract: the `VkResult` values the ICD returns across the C ABI, and the map
//! from a lowering [`hl_gpu::GpuError`] onto them.
//!
//! Numeric values are the stable Vulkan ABI (from `vk.xml` — the ABI a Vulkan app compiles against),
//! re-declared clean-room here, ported from `hl-shim-vk/src/types.rs`. The `GpuError` → `VkResult`
//! map is what the ICD shim cdylib (later pass) uses to turn a lowering error into the `VkResult` the
//! guest expects. This is the `hl-cuda` `result.rs` analogue.

use hl_gpu::GpuError;

// ---- VkResult (returned as i32 across the C ABI) -------------------------------------------------
pub const VK_SUCCESS: i32 = 0;
/// A fence is unsignaled, or a command buffer is not ready to record/execute.
pub const VK_NOT_READY: i32 = 1;
/// `vkWaitForFences` reached its timeout before the fence(s) signaled.
pub const VK_TIMEOUT: i32 = 2;
/// A two-call enumeration's output array was too small; a truncated result was written.
pub const VK_INCOMPLETE: i32 = 5;
pub const VK_ERROR_OUT_OF_HOST_MEMORY: i32 = -1;
/// An allocation or binding could not be satisfied from device memory.
pub const VK_ERROR_OUT_OF_DEVICE_MEMORY: i32 = -2;
pub const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
/// The synchronous executor connection was lost or rejected submitted work.
pub const VK_ERROR_DEVICE_LOST: i32 = -4;
/// `vkMapMemory` could not map the requested range — bad range / already mapped / not host-visible.
pub const VK_ERROR_MEMORY_MAP_FAILED: i32 = -5;
/// The truthful failure a stub returns for a command from an unadvertised extension.
pub const VK_ERROR_EXTENSION_NOT_PRESENT: i32 = -7;
/// The truthful failure a stub returns for an unimplemented core command.
pub const VK_ERROR_FEATURE_NOT_PRESENT: i32 = -8;
pub const VK_ERROR_INCOMPATIBLE_DRIVER: i32 = -9;
/// A descriptor pool has no room for the requested sets.
pub const VK_ERROR_OUT_OF_POOL_MEMORY: i32 = -1000069000;
/// A pipeline/object could not be created for a reason without a more specific code — used to reject
/// a pipeline whose shader stages / render pass are invalid rather than substituting a default module.
pub const VK_ERROR_UNKNOWN: i32 = -13;
/// The referenced presentation surface no longer exists (`VK_KHR_surface`).
pub const VK_ERROR_SURFACE_LOST_KHR: i32 = -1000000000;
/// A swapchain can no longer acquire images because it has been replaced or resized.
pub const VK_ERROR_OUT_OF_DATE_KHR: i32 = -1000001004;

/// The Vulkan API version this ICD advertises: **Vulkan 1.3.0**. Packed like `VK_MAKE_API_VERSION`:
/// `(variant<<29) | (major<<22) | (minor<<12) | patch`.
///
/// 1.3 is the highest version whose mandatory COMMAND set this driver genuinely performs. The census test
/// `every_core_mandated_command_at_the_advertised_version_is_lowered` (shim `src/tests.rs`) checks that
/// against the Khronos registry, so raising this constant past what is implemented fails the build's tests
/// rather than a client's frame.
///
/// It was 1.4, which was false: seven commands core 1.4 mandates — the push-descriptor family and
/// `vkCmdBindDescriptorSets2` — were silent `void` no-ops, and `void` cannot report a failure, so a client
/// using them mis-rendered with no error at all. The push-descriptor family is now really implemented
/// (`shim/vulkan/src/compute/push_descriptor.rs`), but 1.4 also mandates `dynamicRenderingLocalRead`
/// attachment remapping, `maintenance5`/`maintenance6`, `pipelineRobustness` and line-stipple rasterization
/// that nothing below the shim implements.
///
/// KNOWN RESIDUAL, accepted deliberately: even at 1.3 the mandatory FEATURE bits are not all satisfied
/// (`inlineUniformBlock`, `vulkanMemoryModel`, `subgroupSizeControl`, `maintenance4` and others are
/// reported `VK_FALSE`, and `robustBufferAccess` is gated on executor negotiation). Every individual claim
/// a client can read is truthful, and `vkCreateDevice` refuses to enable a feature this driver does not
/// implement, so an unmet requirement is a hard `VK_ERROR_FEATURE_NOT_PRESENT` at device creation and never
/// a wrong pixel. Dropping to 1.0 to make the feature set literally true would withdraw the whole promoted
/// 1.1–1.3 command surface this driver does implement and that its clients use, which buys honesty in one
/// number at the cost of capability that is really there. The version is therefore a claim about commands,
/// enforced; features are claimed one bit at a time, and refused one bit at a time.
///
/// Lowering costs no client: an application may always request a HIGHER instance `apiVersion` than a
/// driver supports — see `vkCreateInstance` in the shim, which clamps instead of rejecting. Verified
/// against the real loader 1.4.341: an app requesting 1.4.0 gets `VK_SUCCESS` and enumerates this device
/// at 1.3.0.
pub const HL_API_VERSION: u32 = make_api_version(0, 1, 3, 0);
/// `driverVersion` — hl's own driver revision (packed like an api version), increment 1.
pub const HL_DRIVER_VERSION: u32 = make_api_version(0, 0, 1, 0);

/// `VK_MAKE_API_VERSION(variant, major, minor, patch)` — the stable Vulkan version packing.
pub const fn make_api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
    (variant << 29) | (major << 22) | (minor << 12) | patch
}

/// Map a lowering [`GpuError`] onto the `VkResult` an ICD entry point returns. A command that uses an
/// operation outside hl's modeled subset is `VK_ERROR_FEATURE_NOT_PRESENT` (the executor genuinely
/// cannot run it) — matching a real driver — while an unknown/duplicate handle maps to
/// `VK_ERROR_UNKNOWN`, a resource-limit to out-of-device-memory, and an invalid argument to
/// `VK_ERROR_INITIALIZATION_FAILED`.
pub struct Status;

impl Status {
    pub fn from_error(e: &GpuError) -> i32 {
        match e {
            GpuError::Unsupported(_) => VK_ERROR_FEATURE_NOT_PRESENT,
            GpuError::UnknownId { .. } | GpuError::DuplicateId { .. } => VK_ERROR_UNKNOWN,
            GpuError::ResourceLimit(_) => VK_ERROR_OUT_OF_DEVICE_MEMORY,
            GpuError::OutOfBounds => VK_ERROR_MEMORY_MAP_FAILED,
            GpuError::Kernel(_) => VK_ERROR_UNKNOWN,
            GpuError::Decode(_) | GpuError::Transport(_) => VK_ERROR_DEVICE_LOST,
            GpuError::Invalid(_)
            | GpuError::BadEnum { .. }
            | GpuError::BadTag(_)
            | GpuError::NonFinite(_)
            | GpuError::NonCanonicalBool(_)
            | GpuError::Utf8
            | GpuError::ShortBuffer
            | GpuError::TrailingBytes => VK_ERROR_INITIALIZATION_FAILED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Status, VK_ERROR_DEVICE_LOST};
    use hl_gpu::{GpuError, TransportError};

    #[test]
    fn transport_loss_is_reported_as_device_loss() {
        let error = GpuError::Transport(TransportError::ApiLost {
            detail: "executor generation changed".into(),
        });

        assert_eq!(Status::from_error(&error), VK_ERROR_DEVICE_LOST);
    }
}
