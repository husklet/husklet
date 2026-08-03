use core::ffi::{c_char, c_void};
use std::sync::OnceLock;

use hl_cuda::model::graphics::GraphicsResource;
use hl_cuda::result::*;
use hl_cuda::service::graphics;
use hl_gpu::ExportId;

use crate::state::ShimState;

type GlExportBuffer = unsafe extern "C" fn(u32, *mut u64) -> i32;

#[link(name = "dl")]
extern "C" {
    fn dlopen(name: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

fn gl_export_buffer() -> Option<GlExportBuffer> {
    static FUNCTION: OnceLock<usize> = OnceLock::new();
    let pointer = *FUNCTION.get_or_init(|| unsafe {
        // Open the exact SONAME rather than relying on RTLD_GLOBAL visibility. Keeping the handle open
        // for process lifetime also guarantees the resolved bridge cannot dangle.
        let handle = dlopen(c"libEGL.so.1".as_ptr(), 2 /* RTLD_NOW */);
        if handle.is_null() { return 0; }
        dlsym(handle, c"hl_gl_export_buffer".as_ptr()) as usize
    });
    (pointer != 0).then(|| unsafe { core::mem::transmute::<usize, GlExportBuffer>(pointer) })
}

fn resources(ptr: *mut *mut c_void, count: u32) -> Option<Vec<GraphicsResource>> {
    if count == 0 { return Some(Vec::new()); }
    if ptr.is_null() { return None; }
    Some(unsafe { core::slice::from_raw_parts(ptr, count as usize) }
        .iter().map(|resource| GraphicsResource(*resource as u64)).collect())
}

#[no_mangle]
pub unsafe extern "C" fn cuGraphicsGLRegisterBuffer(
    resource: *mut *mut c_void,
    buffer: u32,
    flags: u32,
) -> i32 {
    if resource.is_null() || buffer == 0 || flags > 2 { return CUDA_ERROR_INVALID_VALUE; }
    let Some(export_buffer) = gl_export_buffer() else { return CUDA_ERROR_NOT_SUPPORTED; };
    let mut export = 0;
    if export_buffer(buffer, &mut export) != 0 { return CUDA_ERROR_INVALID_HANDLE; }
    ShimState::with_context(|state| match graphics::register_buffer(&mut state.ctx, &mut state.sink, ExportId(export)) {
        Ok(handle) => { *resource = handle.0 as *mut c_void; CUDA_SUCCESS }
        Err(error) => DriverStatus::from(&error).code(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn cuGraphicsMapResources(count: u32, list: *mut *mut c_void, _stream: *mut c_void) -> i32 {
    let Some(resources) = resources(list, count) else { return CUDA_ERROR_INVALID_VALUE; };
    ShimState::with_context(|state| match graphics::map_resources(&mut state.ctx, &mut state.sink, &resources) {
        Ok(()) => CUDA_SUCCESS,
        Err(error) => DriverStatus::from(&error).code(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn cuGraphicsResourceGetMappedPointer_v2(
    pointer: *mut u64,
    size: *mut usize,
    resource: *mut c_void,
) -> i32 {
    if pointer.is_null() || size.is_null() || resource.is_null() { return CUDA_ERROR_INVALID_VALUE; }
    ShimState::with_context(|state| match graphics::mapped_pointer(&state.ctx, GraphicsResource(resource as u64)) {
        Ok((mapped, bytes)) => { *pointer = mapped.0; *size = bytes as usize; CUDA_SUCCESS }
        Err(error) => DriverStatus::from(&error).code(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn cuGraphicsUnmapResources(count: u32, list: *mut *mut c_void, _stream: *mut c_void) -> i32 {
    let Some(resources) = resources(list, count) else { return CUDA_ERROR_INVALID_VALUE; };
    ShimState::with_context(|state| match graphics::unmap_resources(&mut state.ctx, &mut state.sink, &resources) {
        Ok(()) => CUDA_SUCCESS,
        Err(error) => DriverStatus::from(&error).code(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn cuGraphicsUnregisterResource(resource: *mut c_void) -> i32 {
    if resource.is_null() { return CUDA_ERROR_INVALID_HANDLE; }
    ShimState::with_context(|state| match graphics::unregister_resource(&mut state.ctx, &mut state.sink, GraphicsResource(resource as u64)) {
        Ok(()) => CUDA_SUCCESS,
        Err(error) => DriverStatus::from(&error).code(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn cuGraphicsResourceSetMapFlags_v2(resource: *mut c_void, flags: u32) -> i32 {
    if resource.is_null() { return CUDA_ERROR_INVALID_HANDLE; }
    ShimState::with_context(|state| match graphics::set_map_flags(&mut state.ctx, GraphicsResource(resource as u64), flags) {
        Ok(()) => CUDA_SUCCESS,
        Err(error) => DriverStatus::from(&error).code(),
    })
}

/// GL and CUDA replay through Husklet's same single logical GPU, device 0.
#[no_mangle]
pub unsafe extern "C" fn cuGLGetDevices_v2(count: *mut u32, devices: *mut i32, capacity: u32, list: u32) -> i32 {
    if count.is_null() || !(1..=3).contains(&list) || (capacity > 0 && devices.is_null()) { return CUDA_ERROR_INVALID_VALUE; }
    let written = u32::from(capacity > 0);
    *count = written;
    if written != 0 { *devices = 0; }
    CUDA_SUCCESS
}
