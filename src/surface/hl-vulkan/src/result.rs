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
            // **Vulkan has no code for a transient contention refusal, and this mapping is lossy.**
            // Recorded rather than quietly collapsed, because the distinction is real and is lost here:
            // the identical call from the same device succeeds once the holder unmaps, and nothing in
            // `VkResult` says so. The candidates and why each was rejected --
            //   `VK_ERROR_MEMORY_MAP_FAILED` is already this driver's code for `OutOfBounds`, so reusing
            //     it would make a contention refusal indistinguishable from a bounds violation;
            //   `VK_NOT_READY` is a SUCCESS-class code, and returning success for a refused command is
            //     the worst available answer;
            //   `VK_ERROR_VALIDATION_FAILED_EXT` asserts the caller was wrong, which is the specific
            //     falsehood that makes someone "fix" a correct program.
            // `VK_ERROR_UNKNOWN` is chosen because it is the only one that claims nothing false. The
            // consequence is a constraint on the design, not just a comment: Vulkan-side sharing must
            // PREVENT this condition through the map protocol rather than report it, because a Vulkan
            // caller cannot act on what it cannot distinguish. Revisit alongside the wire "retry later"
            // acknowledgement code that `hl-gpu`'s header classification also wants.
            GpuError::MappedElsewhere { .. } => VK_ERROR_UNKNOWN,
            // A REFUSAL is not a lost device. `TransportError::refusal()` is true exactly when the host
            // received the frame, understood it, and declined it: the connection is not retired, the
            // runtime rolled the batch back atomically, and the next request is as likely to succeed as
            // before. Reporting that as VK_ERROR_DEVICE_LOST told every application its device was
            // unrecoverable over one refused frame — which is why the conformance suite aborted its
            // process 425 times, and why a refusal cascaded into hundreds of unrelated failures behind
            // it. hl-gl's result map has drawn this distinction since it was added; this one had not.
            //
            // VK_ERROR_DEVICE_LOST is not even a legal result for vkCreateImage or vkCreateBuffer, which
            // is what those refusals were being reported as.
            //
            // OUT_OF_DEVICE_MEMORY is the closest LEGAL and recoverable code, and it is legal on every
            // command that can carry a refusal, including vkQueueSubmit. It is not precise, and it
            // cannot be: the host knows which typed error it refused with and the acknowledgement byte
            // carries only "no", so the reason is destroyed at the wire. Widening that byte is what
            // would let this be exact.
            GpuError::Transport(failure) if failure.refusal() => {
                // The acknowledgement now carries a reason CLASS, so a refusal maps to the same result
                // the identical error would have produced had it been caught locally, instead of the
                // nearest legal code. An older host states no class and still lands on
                // OUT_OF_DEVICE_MEMORY, which is what this returned for every refusal before.
                use hl_gpu::transport::model::header::RefusalKind;
                match failure.refusal_kind() {
                    Some(RefusalKind::Unsupported) => VK_ERROR_FEATURE_NOT_PRESENT,
                    Some(RefusalKind::OutOfBounds) => VK_ERROR_MEMORY_MAP_FAILED,
                    Some(RefusalKind::UnknownId) => VK_ERROR_UNKNOWN,
                    // Grouped with `Invalid`, which is the class a kernel-lowering refusal carried before
                // `RefusalKind::Kernel` existed; Vulkan's reported codes are unchanged by that split.
                Some(RefusalKind::Invalid) | Some(RefusalKind::Kernel) => {
                    VK_ERROR_INITIALIZATION_FAILED
                }
                    Some(RefusalKind::ResourceLimit) | Some(RefusalKind::Unstated) | None => {
                        VK_ERROR_OUT_OF_DEVICE_MEMORY
                    }
                }
            }
            GpuError::Decode(_) | GpuError::Transport(_) | GpuError::Panicked(_) => {
                VK_ERROR_DEVICE_LOST
            }
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

    /// A host refusal must NOT be a lost device. The two differ in blast radius: a refusal belongs to the
    /// one frame that provoked it and leaves the connection usable, and reporting it as DEVICE_LOST told
    /// applications to tear everything down — the conformance suite aborts its process on it.
    #[test]
    fn a_host_refusal_is_recoverable_and_not_a_device_loss() {
        use super::VK_ERROR_OUT_OF_DEVICE_MEMORY;
        use hl_gpu::transport::model::error::TransportPhase;

        let refused = GpuError::Transport(TransportError::Rejected {
            phase: TransportPhase::Acknowledgement,
            acknowledgement: 0,
        });
        assert!(
            matches!(&refused, GpuError::Transport(f) if f.refusal()),
            "the fixture must actually be a refusal or this test proves nothing"
        );
        assert_eq!(Status::from_error(&refused), VK_ERROR_OUT_OF_DEVICE_MEMORY);
        assert_ne!(Status::from_error(&refused), VK_ERROR_DEVICE_LOST);
    }

    /// A classified refusal maps to the SAME result the identical error would have produced had the
    /// driver caught it locally — that agreement is the point, because whether a limit was checked guest
    /// side or host side is invisible to the application and must not change what it is told.
    #[test]
    fn a_classified_refusal_maps_like_the_local_error() {
        use hl_gpu::transport::model::error::TransportPhase;
        use hl_gpu::transport::model::header::{
            ACK_FAIL, ACK_INVALID, ACK_OUT_OF_BOUNDS, ACK_RESOURCE_LIMIT, ACK_UNKNOWN_ID,
            ACK_UNSUPPORTED,
        };

        let refused = |ack: u8| {
            Status::from_error(&GpuError::Transport(TransportError::Rejected {
                phase: TransportPhase::Acknowledgement,
                acknowledgement: ack,
            }))
        };
        assert_eq!(
            refused(ACK_UNSUPPORTED),
            Status::from_error(&GpuError::Unsupported("x"))
        );
        assert_eq!(
            refused(ACK_RESOURCE_LIMIT),
            Status::from_error(&GpuError::ResourceLimit("x"))
        );
        assert_eq!(
            refused(ACK_OUT_OF_BOUNDS),
            Status::from_error(&GpuError::OutOfBounds)
        );
        assert_eq!(
            refused(ACK_INVALID),
            Status::from_error(&GpuError::Invalid("x"))
        );
        assert_eq!(
            refused(ACK_UNKNOWN_ID),
            Status::from_error(&GpuError::UnknownId { kind: "x", id: 1 })
        );
        // An older host states no class. That must stay exactly what it was before classes existed, and
        // it must never be a device loss.
        assert_eq!(refused(ACK_FAIL), super::VK_ERROR_OUT_OF_DEVICE_MEMORY);
        assert_ne!(refused(ACK_FAIL), VK_ERROR_DEVICE_LOST);
    }

    /// The consumer-side form of the protocol contract: NO acknowledgement value may turn a refusal into
    /// a lost device — not the ones this host sends today, and not the ones it does not. A driver that
    /// recognised only the current failure value would fall through to the terminal path the moment a
    /// host began classifying, and would do it silently, looking like the classification working right
    /// up until a newly classified refusal tore down a device.
    #[test]
    fn no_acknowledgement_value_makes_a_refusal_a_device_loss() {
        use hl_gpu::transport::model::error::TransportPhase;
        for ack in 0u8..=255 {
            let result = Status::from_error(&GpuError::Transport(TransportError::Rejected {
                phase: TransportPhase::Acknowledgement,
                acknowledgement: ack,
            }));
            assert_ne!(
                result, VK_ERROR_DEVICE_LOST,
                "acknowledgement {ack} was treated as a lost device"
            );
        }
    }

    #[test]
    fn transport_loss_is_reported_as_device_loss() {
        let error = GpuError::Transport(TransportError::ApiLost {
            detail: "executor generation changed".into(),
        });

        assert_eq!(Status::from_error(&error), VK_ERROR_DEVICE_LOST);
    }
}
