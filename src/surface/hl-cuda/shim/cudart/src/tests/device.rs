//! `cudaGetDeviceProperties` — the configured device identity and the capabilities it must not claim.

use super::super::runtime::*;
use core::ffi::{c_char, c_void};

/// `cudaGetDeviceProperties` must report the launcher-configured identity (`HL_CUDA_NAME` /
/// `HL_CUDA_CC`) — the same device `libcuda` and `libnvidia-ml` describe — and must NOT advertise
/// `cooperativeLaunch`: the kernel IR has no grid-wide barrier, so a cooperative-groups `grid.sync()`
/// could not be honoured, and `CU_DEVICE_ATTRIBUTE_COOPERATIVE_LAUNCH` reports 0 as well.
#[test]
fn device_properties_report_the_configured_identity_and_no_cooperative_launch() {
    use crate::runtime::DevicePropOffset as Offset;

    let _serial = crate::state::serial();
    std::env::set_var("HL_CUDA_NAME", "NVIDIA GeForce RTX 3060");
    std::env::set_var("HL_CUDA_CC", "7.5");
    crate::state::reset();

    let mut buf = vec![0u8; Offset::SIZE];
    assert_eq!(
        cudaGetDeviceProperties(buf.as_mut_ptr() as *mut c_void, 0),
        0
    );
    let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char) }
        .to_string_lossy()
        .into_owned();
    assert_eq!(name, "NVIDIA GeForce RTX 3060");
    let read = |at: usize| i32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
    assert_eq!(read(Offset::MAJOR), 7);
    assert_eq!(read(Offset::MINOR), 5);
    assert_eq!(
        read(Offset::COOPERATIVE_LAUNCH),
        0,
        "cooperativeLaunch must not be advertised: the IR has no grid-wide barrier"
    );

    std::env::remove_var("HL_CUDA_NAME");
    std::env::remove_var("HL_CUDA_CC");
    crate::state::reset();
}

#[test]
fn graphics_map_and_unmap_reject_a_destroyed_stream() {
    let _serial = crate::state::serial();
    crate::state::reset();
    let mut stream = core::ptr::null_mut();
    assert_eq!(cudaStreamCreate(&mut stream), 0);
    assert_eq!(cudaStreamDestroy(stream), 0);

    assert_eq!(
        unsafe { cudaGraphicsMapResources(0, core::ptr::null_mut(), stream) },
        400
    );
    assert_eq!(
        unsafe { cudaGraphicsUnmapResources(0, core::ptr::null_mut(), stream) },
        400
    );
}

// Local copies of the result codes the assertions above reference (kept crate-private).
