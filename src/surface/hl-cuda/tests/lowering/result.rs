use super::*;

// ---------------------------------------------------------------------------------------------------
// result mapping
// ---------------------------------------------------------------------------------------------------

#[test]
fn gpu_error_maps_to_curesult() {
    assert_eq!(
        result::DriverStatus::from(&GpuError::Kernel("x".into())).code(),
        result::CUDA_ERROR_INVALID_PTX
    );
    assert_eq!(
        result::DriverStatus::from(&GpuError::Unsupported("x")).code(),
        result::CUDA_ERROR_NOT_SUPPORTED
    );
    assert_eq!(
        result::DriverStatus::from(&GpuError::Invalid("x")).code(),
        result::CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        result::RuntimeStatus::from(&GpuError::Kernel("x".into())).code(),
        result::CUDART_ERROR_INVALID_PTX
    );
}
