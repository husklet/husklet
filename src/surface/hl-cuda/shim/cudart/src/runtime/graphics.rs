use core::ffi::{c_char, c_void};
use std::sync::OnceLock;
use hl_cuda::model::graphics::GraphicsResource;
use hl_cuda::result::{RuntimeStatus, CUDART_ERROR_INVALID_RESOURCE_HANDLE, CUDART_ERROR_INVALID_VALUE, CUDART_ERROR_NOT_SUPPORTED, CUDART_SUCCESS};
use hl_cuda::service::graphics;
use hl_gpu::ExportId;
use crate::state::ShimState;

type GlExportBuffer = unsafe extern "C" fn(u32, *mut u64) -> i32;
const GL_TEXTURE_2D: u32 = 0x0de1;
const GL_TEXTURE_3D: u32 = 0x806f;
const GL_TEXTURE_CUBE_MAP: u32 = 0x8513;
const GL_TEXTURE_2D_ARRAY: u32 = 0x8c1a;
const GL_TEXTURE_RECTANGLE: u32 = 0x84f5;
const GL_RENDERBUFFER: u32 = 0x8d41;
#[link(name = "dl")]
extern "C" { fn dlopen(name: *const c_char, flags: i32) -> *mut c_void; fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void; }

fn gl_export_buffer() -> Option<GlExportBuffer> {
    static FUNCTION: OnceLock<usize> = OnceLock::new();
    let pointer = *FUNCTION.get_or_init(|| unsafe {
        let handle = dlopen(c"libEGL.so.1".as_ptr(), 2);
        if handle.is_null() { return 0; }
        dlsym(handle, c"hl_gl_export_buffer".as_ptr()) as usize
    });
    (pointer != 0).then(|| unsafe { core::mem::transmute(pointer) })
}

fn resources(ptr: *mut *mut c_void, count: i32) -> Option<Vec<GraphicsResource>> {
    if count < 0 || (count > 0 && ptr.is_null()) { return None; }
    Some(if count == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(ptr, count as usize) }.iter().map(|p| GraphicsResource(*p as u64)).collect() })
}

#[no_mangle]
pub unsafe extern "C" fn cudaGraphicsGLRegisterBuffer(resource: *mut *mut c_void, buffer: u32, flags: u32) -> i32 {
    if resource.is_null() || buffer == 0 || flags > 2 { return CUDART_ERROR_INVALID_VALUE; }
    let Some(bridge) = gl_export_buffer() else { return CUDART_ERROR_NOT_SUPPORTED; };
    let mut export = 0;
    if bridge(buffer, &mut export) != 0 { return CUDART_ERROR_INVALID_RESOURCE_HANDLE; }
    ShimState::with(|state| match graphics::register_buffer(&mut state.ctx, &mut state.sink, ExportId(export)) {
        Ok(handle) => { *resource = handle.0 as *mut c_void; CUDART_SUCCESS }
        Err(error) => state.fail(RuntimeStatus::from(&error).code()),
    })
}

#[no_mangle]
pub unsafe extern "C" fn cudaGraphicsGLRegisterImage(resource: *mut *mut c_void, image: u32, target: u32, flags: u32) -> i32 {
    if resource.is_null() || image == 0 || !matches!(target, GL_TEXTURE_2D | GL_TEXTURE_3D | GL_TEXTURE_CUBE_MAP | GL_TEXTURE_2D_ARRAY | GL_TEXTURE_RECTANGLE | GL_RENDERBUFFER) || !matches!(flags, 0 | 1 | 2 | 4 | 8) {
        return CUDART_ERROR_INVALID_VALUE;
    }
    ShimState::with(|state| state.fail(CUDART_ERROR_NOT_SUPPORTED))
}

#[no_mangle]
pub unsafe extern "C" fn cudaGraphicsMapResources(count: i32, list: *mut *mut c_void, stream: *mut c_void) -> i32 {
    let Some(resources) = resources(list, count) else { return CUDART_ERROR_INVALID_VALUE; };
    ShimState::with(|state| {
        let Some(stream) = state.stream(stream) else { return state.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE); };
        match graphics::map_resources(&mut state.ctx, &mut state.sink, &resources, stream) { Ok(()) => CUDART_SUCCESS, Err(e) => state.fail(RuntimeStatus::from(&e).code()) }
    })
}

#[no_mangle]
pub unsafe extern "C" fn cudaGraphicsResourceGetMappedPointer(pointer: *mut *mut c_void, size: *mut usize, resource: *mut c_void) -> i32 {
    if pointer.is_null() || size.is_null() || resource.is_null() { return CUDART_ERROR_INVALID_VALUE; }
    ShimState::with(|state| match graphics::mapped_pointer(&state.ctx, GraphicsResource(resource as u64)) {
        Ok((p, bytes)) => { *pointer = p.0 as *mut c_void; *size = bytes as usize; CUDART_SUCCESS }
        Err(e) => state.fail(RuntimeStatus::from(&e).code()),
    })
}

#[no_mangle]
pub unsafe extern "C" fn cudaGraphicsSubResourceGetMappedArray(array: *mut *mut c_void, resource: *mut c_void, array_index: u32, mip_level: u32) -> i32 {
    if array.is_null() { return CUDART_ERROR_INVALID_VALUE; }
    let _ = (array_index, mip_level);
    let _ = resource;
    ShimState::with(|state| state.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE))
}

#[no_mangle]
pub unsafe extern "C" fn cudaGraphicsUnmapResources(count: i32, list: *mut *mut c_void, stream: *mut c_void) -> i32 {
    let Some(resources) = resources(list, count) else { return CUDART_ERROR_INVALID_VALUE; };
    ShimState::with(|state| {
        let Some(stream) = state.stream(stream) else { return state.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE); };
        match graphics::unmap_resources(&mut state.ctx, &mut state.sink, &resources, stream) { Ok(()) => CUDART_SUCCESS, Err(e) => state.fail(RuntimeStatus::from(&e).code()) }
    })
}

#[no_mangle]
pub unsafe extern "C" fn cudaGraphicsUnregisterResource(resource: *mut c_void) -> i32 {
    if resource.is_null() { return CUDART_ERROR_INVALID_RESOURCE_HANDLE; }
    ShimState::with(|state| match graphics::unregister_resource(&mut state.ctx, &mut state.sink, GraphicsResource(resource as u64)) { Ok(()) => CUDART_SUCCESS, Err(e) => state.fail(RuntimeStatus::from(&e).code()) })
}

#[no_mangle]
pub unsafe extern "C" fn cudaGraphicsResourceSetMapFlags(resource: *mut c_void, flags: u32) -> i32 {
    if resource.is_null() { return CUDART_ERROR_INVALID_RESOURCE_HANDLE; }
    ShimState::with(|state| match graphics::set_map_flags(&mut state.ctx, GraphicsResource(resource as u64), flags) {
        Ok(()) => CUDART_SUCCESS,
        Err(error) => state.fail(RuntimeStatus::from(&error).code()),
    })
}
