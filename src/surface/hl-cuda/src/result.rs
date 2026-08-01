//! CUDA result-code contract: the `CUresult` (driver API) + `cudaError_t` (runtime API) values the
//! guest libs return, and the map from a lowering [`hl_gpu::GpuError`] onto them.
//!
//! Numeric values match NVIDIA's published `cuda.h` / `driver_types.h` (the stable ABI a CUDA app
//! compiles against); they are re-declared clean-room here, ported from `hl-shim-cuda/src/result.rs`.
//! Only the subset the hand-written entry points reference is declared — generated stubs (later pass)
//! never inspect a value, they return `CUDA_SUCCESS`. The `GpuError` → code map is what the shim
//! cdylibs (later) will use to turn a lowering error into the `CUresult` the guest expects.

use hl_gpu::transport::model::header::RefusalKind;
use hl_gpu::GpuError;

// ---- CUresult (returned as i32 across the C ABI) -------------------------------------------------
pub const CUDA_SUCCESS: i32 = 0;
pub const CUDA_ERROR_INVALID_VALUE: i32 = 1;
pub const CUDA_ERROR_OUT_OF_MEMORY: i32 = 2;
pub const CUDA_ERROR_NOT_INITIALIZED: i32 = 3;
pub const CUDA_ERROR_INVALID_DEVICE: i32 = 101;
pub const CUDA_ERROR_INVALID_IMAGE: i32 = 200;
pub const CUDA_ERROR_INVALID_CONTEXT: i32 = 201;
/// The resource is mapped, and not by this caller. CUDA's own vocabulary for a graphics-interop
/// resource that cannot be touched because a map is outstanding.
pub const CUDA_ERROR_ALREADY_MAPPED: i32 = 211;
pub const CUDA_ERROR_UNSUPPORTED_LIMIT: i32 = 215;
/// Peer access between two contexts is not supported by the device (the single simulated device has no
/// peers), returned by `cuCtxEnablePeerAccess`.
pub const CUDA_ERROR_PEER_ACCESS_UNSUPPORTED: i32 = 217;
pub const CUDA_ERROR_INVALID_PTX: i32 = 218;
pub const CUDA_ERROR_FILE_NOT_FOUND: i32 = 301;
pub const CUDA_ERROR_INVALID_HANDLE: i32 = 400;
pub const CUDA_ERROR_NOT_FOUND: i32 = 500;
pub const CUDA_ERROR_NOT_READY: i32 = 600;
/// Peer access was never enabled between the contexts, returned by `cuCtxDisablePeerAccess`.
pub const CUDA_ERROR_PEER_ACCESS_NOT_ENABLED: i32 = 705;
pub const CUDA_ERROR_NOT_SUPPORTED: i32 = 801;
pub const CUDA_ERROR_UNKNOWN: i32 = 999;

// ---- CUdriverProcAddressQueryResult — the `status` out-param `cuGetProcAddress_v2` fills (from cuda.h).
pub const CU_GET_PROC_ADDRESS_SUCCESS: i32 = 0;
pub const CU_GET_PROC_ADDRESS_SYMBOL_NOT_FOUND: i32 = 1;

/// The driver version `cuDriverGetVersion` reports: `major*1000 + minor*10`. 12020 == CUDA 12.2.
pub const DRIVER_VERSION: i32 = 12020;

/// `cuCtxGetApiVersion` reports the classic 3.2 driver-API version (the value real drivers return for a
/// context created through the modern API).
pub const CTX_API_VERSION: u32 = 3020;

// ---- CUdevice_attribute (the set `cuDeviceGetAttribute` answers; values from cuda.h) --------------
pub const CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK: i32 = 1;
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X: i32 = 2;
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Y: i32 = 3;
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Z: i32 = 4;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X: i32 = 5;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Y: i32 = 6;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Z: i32 = 7;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK: i32 = 8;
pub const CU_DEVICE_ATTRIBUTE_TOTAL_CONSTANT_MEMORY: i32 = 9;
pub const CU_DEVICE_ATTRIBUTE_WARP_SIZE: i32 = 10;
pub const CU_DEVICE_ATTRIBUTE_MAX_PITCH: i32 = 11;
pub const CU_DEVICE_ATTRIBUTE_MAX_REGISTERS_PER_BLOCK: i32 = 12;
pub const CU_DEVICE_ATTRIBUTE_CLOCK_RATE: i32 = 13;
pub const CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT: i32 = 14;
pub const CU_DEVICE_ATTRIBUTE_GPU_OVERLAP: i32 = 15;
pub const CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT: i32 = 16;
pub const CU_DEVICE_ATTRIBUTE_KERNEL_EXEC_TIMEOUT: i32 = 17;
pub const CU_DEVICE_ATTRIBUTE_INTEGRATED: i32 = 18;
pub const CU_DEVICE_ATTRIBUTE_CAN_MAP_HOST_MEMORY: i32 = 19;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_MODE: i32 = 20;
pub const CU_DEVICE_ATTRIBUTE_MAXIMUM_TEXTURE1D_WIDTH: i32 = 21;
pub const CU_DEVICE_ATTRIBUTE_CONCURRENT_KERNELS: i32 = 31;
pub const CU_DEVICE_ATTRIBUTE_ECC_ENABLED: i32 = 32;
pub const CU_DEVICE_ATTRIBUTE_PCI_BUS_ID: i32 = 33;
pub const CU_DEVICE_ATTRIBUTE_PCI_DEVICE_ID: i32 = 34;
pub const CU_DEVICE_ATTRIBUTE_TCC_DRIVER: i32 = 35;
pub const CU_DEVICE_ATTRIBUTE_MEMORY_CLOCK_RATE: i32 = 36;
pub const CU_DEVICE_ATTRIBUTE_GLOBAL_MEMORY_BUS_WIDTH: i32 = 37;
pub const CU_DEVICE_ATTRIBUTE_L2_CACHE_SIZE: i32 = 38;
pub const CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_MULTIPROCESSOR: i32 = 39;
pub const CU_DEVICE_ATTRIBUTE_ASYNC_ENGINE_COUNT: i32 = 40;
pub const CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING: i32 = 41;
pub const CU_DEVICE_ATTRIBUTE_PCI_DOMAIN_ID: i32 = 50;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR: i32 = 75;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR: i32 = 76;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_MULTIPROCESSOR: i32 = 81;
/// Cooperative (grid-synchronizing) launch support. The kernel IR has no grid-wide barrier, so the
/// modeled device reports this absent and `cuLaunchCooperativeKernel` is `CUDA_ERROR_NOT_SUPPORTED`.
pub const CU_DEVICE_ATTRIBUTE_COOPERATIVE_LAUNCH: i32 = 95;
pub const CU_DEVICE_ATTRIBUTE_MANAGED_MEMORY: i32 = 83;
pub const CU_DEVICE_ATTRIBUTE_MULTI_GPU_BOARD: i32 = 84;
pub const CU_DEVICE_ATTRIBUTE_CONCURRENT_MANAGED_ACCESS: i32 = 89;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_PREEMPTION_SUPPORTED: i32 = 90;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK_OPTIN: i32 = 97;
pub const CU_DEVICE_ATTRIBUTE_PAGEABLE_MEMORY_ACCESS: i32 = 101;
pub const CU_DEVICE_ATTRIBUTE_DIRECT_MANAGED_MEM_ACCESS_FROM_HOST: i32 = 108;
pub const CU_DEVICE_ATTRIBUTE_MEMORY_POOLS_SUPPORTED: i32 = 115;
/// `CU_DEVICE_ATTRIBUTE_MAX` — one past the last `CUdevice_attribute` the reported driver version
/// ([`DRIVER_VERSION`], CUDA 12.2) defines. `cuDeviceGetAttribute` answers 0 for an in-range attribute
/// the model does not track (a real driver likewise reports "feature absent") and
/// `CUDA_ERROR_INVALID_VALUE` for anything outside — an enum no driver knows is not a capability query
/// that can be answered, and answering 0 makes "unknown" indistinguishable from "present but zero".
pub const CU_DEVICE_ATTRIBUTE_MAX: i32 = 150;

// ---- CUpointer_attribute + CUmemorytype (values from cuda.h) -------------------------------------
pub const CU_POINTER_ATTRIBUTE_CONTEXT: i32 = 1;
pub const CU_POINTER_ATTRIBUTE_MEMORY_TYPE: i32 = 2;
pub const CU_POINTER_ATTRIBUTE_DEVICE_POINTER: i32 = 3;
pub const CU_POINTER_ATTRIBUTE_HOST_POINTER: i32 = 4;
pub const CU_POINTER_ATTRIBUTE_SYNC_MEMOPS: i32 = 6;
pub const CU_POINTER_ATTRIBUTE_BUFFER_ID: i32 = 7;
pub const CU_POINTER_ATTRIBUTE_IS_MANAGED: i32 = 8;
pub const CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL: i32 = 9;
pub const CU_POINTER_ATTRIBUTE_RANGE_START_ADDR: i32 = 11;
pub const CU_POINTER_ATTRIBUTE_RANGE_SIZE: i32 = 12;
pub const CU_POINTER_ATTRIBUTE_MAPPED: i32 = 13;
pub const CU_MEMORYTYPE_DEVICE: u32 = 2;

// ---- CUfunction_attribute (the set `cuFuncGetAttribute`/`cuFuncSetAttribute` answer; from cuda.h) --
pub const CU_FUNC_ATTRIBUTE_MAX_THREADS_PER_BLOCK: i32 = 0;
pub const CU_FUNC_ATTRIBUTE_SHARED_SIZE_BYTES: i32 = 1;
pub const CU_FUNC_ATTRIBUTE_CONST_SIZE_BYTES: i32 = 2;
pub const CU_FUNC_ATTRIBUTE_LOCAL_SIZE_BYTES: i32 = 3;
pub const CU_FUNC_ATTRIBUTE_NUM_REGS: i32 = 4;
pub const CU_FUNC_ATTRIBUTE_PTX_VERSION: i32 = 5;
pub const CU_FUNC_ATTRIBUTE_BINARY_VERSION: i32 = 6;
pub const CU_FUNC_ATTRIBUTE_CACHE_MODE_CA: i32 = 7;
pub const CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES: i32 = 8;
pub const CU_FUNC_ATTRIBUTE_PREFERRED_SHARED_MEMORY_CARVEOUT: i32 = 9;

// ---- CUlimit — `CU_LIMIT_MAX` is one past the last valid `CUlimit`; the limit table has this many slots.
pub const CU_LIMIT_MAX: i32 = 7;

// ---- CUmoduleLoadingMode — the loading mode `cuModuleGetLoadingMode` reports (value from cuda.h).
pub const CU_MODULE_EAGER_LOADING: i32 = 1;

// ---- cudaError_t (runtime API; the subset the runtime entry points return) -----------------------
pub const CUDART_SUCCESS: i32 = 0; // cudaSuccess
pub const CUDART_ERROR_INVALID_VALUE: i32 = 1; // cudaErrorInvalidValue
pub const CUDART_ERROR_MEMORY_ALLOCATION: i32 = 2; // cudaErrorMemoryAllocation
pub const CUDART_ERROR_INITIALIZATION: i32 = 3; // cudaErrorInitializationError
pub const CUDART_ERROR_INVALID_DEVICE_FUNCTION: i32 = 98; // cudaErrorInvalidDeviceFunction
pub const CUDART_ERROR_INVALID_DEVICE: i32 = 101; // cudaErrorInvalidDevice
pub const CUDART_ERROR_INVALID_KERNEL_IMAGE: i32 = 200; // cudaErrorInvalidKernelImage
/// Runtime-API counterpart of `CUDA_ERROR_ALREADY_MAPPED`.
pub const CUDART_ERROR_ALREADY_MAPPED: i32 = 27;
pub const CUDART_ERROR_INVALID_PTX: i32 = 218; // cudaErrorInvalidPtx
pub const CUDART_ERROR_INVALID_RESOURCE_HANDLE: i32 = 400; // cudaErrorInvalidResourceHandle
pub const CUDART_ERROR_SYMBOL_NOT_FOUND: i32 = 500; // cudaErrorSymbolNotFound
pub const CUDART_ERROR_NOT_SUPPORTED: i32 = 801; // cudaErrorNotSupported
pub const CUDART_ERROR_UNKNOWN: i32 = 999; // cudaErrorUnknown

/// Map a lowering [`GpuError`] onto the `CUresult` a driver-API entry point returns. A kernel that uses
/// an instruction/space/type outside hl's modeled subset is `CUDA_ERROR_NOT_SUPPORTED` (the executor
/// genuinely cannot run it) — matching a real driver — while an invalid-argument/handle error maps to
/// the closest `CUDA_ERROR_*`.
pub struct DriverStatus<'a>(&'a GpuError);

impl<'a> From<&'a GpuError> for DriverStatus<'a> {
    fn from(error: &'a GpuError) -> Self {
        Self(error)
    }
}

impl DriverStatus<'_> {
    pub fn code(self) -> i32 {
        let code = match self.0 {
            GpuError::Unsupported(_) => CUDA_ERROR_NOT_SUPPORTED,
            GpuError::Kernel(_) => CUDA_ERROR_INVALID_PTX,
            GpuError::UnknownId { .. } | GpuError::DuplicateId { .. } => CUDA_ERROR_INVALID_HANDLE,
            GpuError::OutOfBounds => CUDA_ERROR_INVALID_VALUE,
            GpuError::ResourceLimit(_) => CUDA_ERROR_OUT_OF_MEMORY,
            // A TIMING refusal, and CUDA has the precise word for it. `CUDA_ERROR_NOT_READY` was the
            // other candidate and is worse here: it says "come back later" without saying why, whereas
            // `ALREADY_MAPPED` names the condition, which is what lets a caller act on it instead of
            // spinning. Deliberately NOT folded in with the `Invalid` arms — the identical call from the
            // same context succeeds once the holder unmaps, and a caller that cannot tell a timing
            // refusal from a malformed one will "fix" a correct program.
            GpuError::MappedElsewhere { .. } => CUDA_ERROR_ALREADY_MAPPED,
            // A host that received a complete request and REFUSED it has not lost anything: the batch
            // was rejected atomically and the connection is still there. The acknowledgement carries the
            // CLASS of the refusal, so each arm below is the code this driver raises for the same
            // condition locally — where the failure was detected stops being visible to the caller.
            //
            // Keyed on `refusal()`, never on a particular acknowledgement byte: a host that classifies
            // more finely than this guest understands must still land here and be reported as a refused
            // call, not fall through to the transport-death arm below and become a lost device.
            GpuError::Transport(failure) if failure.refusal() => match failure.refusal_kind() {
                Some(RefusalKind::Unsupported) => CUDA_ERROR_NOT_SUPPORTED,
                Some(RefusalKind::ResourceLimit) => CUDA_ERROR_OUT_OF_MEMORY,
                Some(RefusalKind::UnknownId) => CUDA_ERROR_INVALID_HANDLE,
                Some(RefusalKind::Kernel) => CUDA_ERROR_INVALID_PTX,
                Some(RefusalKind::MappedElsewhere) => CUDA_ERROR_ALREADY_MAPPED,
                Some(RefusalKind::Invalid) | Some(RefusalKind::OutOfBounds) => {
                    CUDA_ERROR_INVALID_VALUE
                }
                // The host declined and named no reason. `CUDA_ERROR_UNKNOWN` is the honest answer, and
                // it is the only approximation left.
                Some(RefusalKind::Unstated) | None => CUDA_ERROR_UNKNOWN,
            },
            GpuError::Decode(_) | GpuError::Transport(_) | GpuError::Panicked(_) => {
                CUDA_ERROR_UNKNOWN
            }
            GpuError::Invalid(_)
            | GpuError::BadEnum { .. }
            | GpuError::BadTag(_)
            | GpuError::NonFinite(_)
            | GpuError::NonCanonicalBool(_)
            | GpuError::Utf8
            | GpuError::ShortBuffer
            | GpuError::TrailingBytes => CUDA_ERROR_INVALID_VALUE,
        };
        hl_log::hl_error!(hl_log::tag::SHIM, "cu err={:?} -> {}", self.0, code);
        code
    }
}

/// Map a lowering [`GpuError`] onto the `cudaError_t` a runtime-API entry point returns.
pub struct RuntimeStatus<'a>(&'a GpuError);

impl<'a> From<&'a GpuError> for RuntimeStatus<'a> {
    fn from(error: &'a GpuError) -> Self {
        Self(error)
    }
}

impl RuntimeStatus<'_> {
    pub fn code(self) -> i32 {
        let code = match self.0 {
            GpuError::Unsupported(_) => CUDART_ERROR_NOT_SUPPORTED,
            GpuError::Kernel(_) => CUDART_ERROR_INVALID_PTX,
            GpuError::UnknownId { .. } | GpuError::DuplicateId { .. } => {
                CUDART_ERROR_INVALID_RESOURCE_HANDLE
            }
            GpuError::ResourceLimit(_) => CUDART_ERROR_MEMORY_ALLOCATION,
            // Same contract as `DriverStatus`, in runtime-API codes: a classified refusal reports what
            // the identical local failure would report. Keyed on `refusal()`, not on an ack value.
            GpuError::Transport(failure) if failure.refusal() => match failure.refusal_kind() {
                Some(RefusalKind::Unsupported) => CUDART_ERROR_NOT_SUPPORTED,
                Some(RefusalKind::ResourceLimit) => CUDART_ERROR_MEMORY_ALLOCATION,
                Some(RefusalKind::UnknownId) => CUDART_ERROR_INVALID_RESOURCE_HANDLE,
                Some(RefusalKind::Kernel) => CUDART_ERROR_INVALID_PTX,
                Some(RefusalKind::MappedElsewhere) => CUDART_ERROR_ALREADY_MAPPED,
                Some(RefusalKind::Invalid) | Some(RefusalKind::OutOfBounds) => {
                    CUDART_ERROR_INVALID_VALUE
                }
                Some(RefusalKind::Unstated) | None => CUDART_ERROR_UNKNOWN,
            },
            GpuError::Decode(_) => CUDART_ERROR_UNKNOWN,
            _ => CUDART_ERROR_INVALID_VALUE,
        };
        hl_log::hl_error!(hl_log::tag::SHIM, "cudart err={:?} -> {}", self.0, code);
        code
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_gpu::transport::model::header::{RefusalKind, ACK_FAIL, ACK_OK};
    use hl_gpu::transport::TransportPhase;
    use hl_gpu::{GpuError, TransportError};

    fn refused(acknowledgement: u8) -> GpuError {
        GpuError::Transport(TransportError::Rejected {
            phase: TransportPhase::Acknowledgement,
            acknowledgement,
        })
    }

    #[test]
    fn transport_loss_is_reported_as_driver_failure() {
        let error = GpuError::Transport(TransportError::ApiLost {
            detail: "executor generation changed".into(),
        });

        assert_eq!(DriverStatus::from(&error).code(), CUDA_ERROR_UNKNOWN);
    }

    /// A host that refused because it could not lower the kernel must reach the caller as
    /// `CUDA_ERROR_INVALID_PTX` — the same code this driver raises when it rejects the PTX itself — not
    /// as the generic `CUDA_ERROR_UNKNOWN` every transport failure used to collapse into. Measured
    /// against the shipped executor, `red.global.min.s32` produced 999 before this map existed.
    #[test]
    fn a_refused_kernel_lowering_reports_invalid_ptx_not_unknown() {
        let error = refused(RefusalKind::Kernel.ack());

        assert_eq!(DriverStatus::from(&error).code(), CUDA_ERROR_INVALID_PTX);
        assert_eq!(
            RuntimeStatus::from(&error).code(),
            CUDART_ERROR_INVALID_PTX
        );
        // The point of the change: it is no longer the unclassified answer.
        assert_ne!(DriverStatus::from(&error).code(), CUDA_ERROR_UNKNOWN);
    }

    /// Each class reports what the identical locally-detected error reports, so where the failure was
    /// detected is invisible to the caller. The local column is the assertion — writing the constants
    /// twice would just restate the map.
    #[test]
    fn every_refusal_class_agrees_with_the_local_error_for_the_same_condition() {
        for (kind, local) in [
            (RefusalKind::Unsupported, GpuError::Unsupported("x".into())),
            (RefusalKind::ResourceLimit, GpuError::ResourceLimit("x".into())),
            (RefusalKind::OutOfBounds, GpuError::OutOfBounds),
            (RefusalKind::Invalid, GpuError::Invalid("x".into())),
            (RefusalKind::Kernel, GpuError::Kernel("x".into())),
        ] {
            assert_eq!(
                DriverStatus::from(&refused(kind.ack())).code(),
                DriverStatus::from(&local).code(),
                "refusal class {kind:?} must report what the local error reports"
            );
        }
    }

    /// The caution the wire's authors paid for: key on the REFUSAL, never on the particular
    /// acknowledgement value. A host that classifies more finely than this guest understands must still
    /// be treated as having refused the call — reported as a plain failure the caller can act on — and
    /// must NOT fall through to the transport-death arm and become an unrecoverable device.
    #[test]
    fn an_unrecognised_refusal_class_is_still_a_refusal() {
        let future = refused(200);

        assert_eq!(
            RefusalKind::from_ack(200),
            RefusalKind::Unstated,
            "an unknown class reads as unstated"
        );
        assert_eq!(DriverStatus::from(&future).code(), CUDA_ERROR_UNKNOWN);
        // A refusal is recoverable: the batch was rejected atomically and the connection survived. The
        // guard is that it took the refusal arm at all, which `refusal()` — not the byte — decides.
        let GpuError::Transport(failure) = &future else {
            panic!("constructed a transport failure");
        };
        assert!(failure.refusal(), "any Rejected acknowledgement is a refusal");
    }

    /// The unclassified refusal an OLDER host sends keeps meaning exactly what it meant before.
    #[test]
    fn an_unstated_refusal_from_an_older_host_is_unchanged() {
        assert_eq!(RefusalKind::from_ack(ACK_FAIL), RefusalKind::Unstated);
        assert_eq!(
            DriverStatus::from(&refused(ACK_FAIL)).code(),
            CUDA_ERROR_UNKNOWN
        );
        assert_ne!(ACK_OK, RefusalKind::Kernel.ack(), "a refusal is never ACK_OK");
    }
}
