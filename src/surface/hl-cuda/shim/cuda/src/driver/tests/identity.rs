//! Device-identity coherence + the capabilities the driver must NOT advertise.

use super::support::guard;
use super::*;

/// `libcuda` must report the launcher-configured identity (`HL_CUDA_NAME` / `HL_CUDA_CC`), the same
/// values `libnvidia-ml` and `libcudart` answer. Reporting a hardcoded name and compute capability here
/// while NVML reports the configured ones makes the device self-contradictory to any application that
/// reads both (`nvidia-smi` versus `cuDeviceGetName`).
#[test]
fn configured_device_identity_is_reported_by_the_driver_api() {
    // Hold the serializing guard across the env mutation so no other test rebuilds its state from it.
    let _serial = guard();
    std::env::set_var("HL_CUDA_NAME", "NVIDIA GeForce RTX 3060");
    std::env::set_var("HL_CUDA_CC", "7.5");
    crate::state::reset(); // rebuild the process-global state, re-reading the env

    let mut name = [0 as c_char; 128];
    assert_eq!(cuDeviceGetName(name.as_mut_ptr(), 128, 0), CUDA_SUCCESS);
    let reported = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    assert_eq!(reported, "NVIDIA GeForce RTX 3060");

    let (mut major, mut minor) = (-1i32, -1i32);
    assert_eq!(
        cuDeviceComputeCapability(&mut major, &mut minor, 0),
        CUDA_SUCCESS
    );
    assert_eq!((major, minor), (7, 5));

    // The attribute form must agree with the dedicated query.
    let (mut attr_major, mut attr_minor) = (-1i32, -1i32);
    assert_eq!(
        cuDeviceGetAttribute(
            &mut attr_major,
            CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR,
            0
        ),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuDeviceGetAttribute(
            &mut attr_minor,
            CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR,
            0
        ),
        CUDA_SUCCESS
    );
    assert_eq!((attr_major, attr_minor), (7, 5));

    std::env::remove_var("HL_CUDA_NAME");
    std::env::remove_var("HL_CUDA_CC");
    crate::state::reset();
}

/// A cooperative launch promises every block is co-resident so `grid.sync()` works. The kernel IR has no
/// grid-wide barrier, so the promise cannot be kept: the entry point must return
/// `CUDA_ERROR_NOT_SUPPORTED` and the device attribute must report the feature absent. Running the kernel
/// as an ordinary launch would silently drop every grid synchronization.
#[test]
fn cooperative_launch_is_unsupported_and_not_advertised() {
    let _serial = guard();
    let func = super::support::load_vecadd();

    let mut advertised = -1i32;
    assert_eq!(
        cuDeviceGetAttribute(&mut advertised, CU_DEVICE_ATTRIBUTE_COOPERATIVE_LAUNCH, 0),
        CUDA_SUCCESS
    );
    assert_eq!(advertised, 0, "cooperative launch must not be advertised");

    assert_eq!(
        cuLaunchCooperativeKernel(
            func,
            1,
            1,
            1,
            4,
            1,
            1,
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        ),
        CUDA_ERROR_NOT_SUPPORTED
    );
}
