use super::*;
#[no_mangle]
pub extern "C" fn cuMemGetInfo_v2(free_out: *mut usize, total_out: *mut usize) -> i32 {
    ShimState::with_context(|s| {
        let (free, total) = s.mem_info();
        if !free_out.is_null() {
            unsafe { *free_out = free };
        }
        if !total_out.is_null() {
            unsafe { *total_out = total };
        }
        CUDA_SUCCESS
    })
}

#[no_mangle]
pub extern "C" fn cuMemGetAddressRange_v2(pbase: *mut u64, psize: *mut usize, dptr: u64) -> i32 {
    ShimState::with_context(|s| match s.ctx.mem.containing(DevicePtr(dptr)) {
        Some((base, size)) => {
            if !pbase.is_null() {
                unsafe { *pbase = base };
            }
            if !psize.is_null() {
                unsafe { *psize = size as usize };
            }
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_VALUE, // not a live device allocation
    })
}

/// Fill one pointer attribute into `data`. Every allocation the modeled driver hands out is device
/// memory in the unified VA (there is no managed/host alloc path yet), so the memory-type / managed /
/// ordinal answers are truthful for what the model knows. An attribute we cannot honestly answer for a
/// pointer that is not a live allocation returns `CUDA_ERROR_INVALID_VALUE`.
///
/// # Safety
/// `data` must point at a writable buffer large enough for `attr`'s value type.
unsafe fn pointer_attr(attr: i32, data: *mut c_void, ptr: u64) -> i32 {
    if data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let (found, base, size, managed, cur_ctx) = ShimState::with(|s| {
        let m = s.ctx.mem.containing(DevicePtr(ptr));
        (
            m.is_some(),
            m.map(|x| x.0).unwrap_or(0),
            m.map(|x| x.1).unwrap_or(0),
            s.ctx.mem.is_managed(DevicePtr(ptr)),
            s.current_ctx() as usize,
        )
    });
    match attr {
        CU_POINTER_ATTRIBUTE_CONTEXT => *(data as *mut usize) = cur_ctx,
        CU_POINTER_ATTRIBUTE_MEMORY_TYPE => {
            if !found {
                return CUDA_ERROR_INVALID_VALUE;
            }
            *(data as *mut u32) = CU_MEMORYTYPE_DEVICE;
        }
        CU_POINTER_ATTRIBUTE_DEVICE_POINTER => {
            if !found {
                return CUDA_ERROR_INVALID_VALUE;
            }
            *(data as *mut u64) = ptr;
        }
        CU_POINTER_ATTRIBUTE_HOST_POINTER => *(data as *mut *mut c_void) = core::ptr::null_mut(),
        CU_POINTER_ATTRIBUTE_IS_MANAGED => *(data as *mut u32) = managed as u32,
        CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL => *(data as *mut i32) = 0,
        // These three are answers *about the containing allocation*, so a pointer that is inside none
        // has no honest answer. Letting the miss fall through wrote base = 0 / size = 0 under a success
        // status, which tells the caller the zero is valid: "not a live allocation" and "an allocation
        // at address 0 of length 0" become the same observation, and code that sizes a copy from
        // RANGE_SIZE gets a silent zero-length transfer instead of an error where the mistake was made.
        CU_POINTER_ATTRIBUTE_BUFFER_ID => {
            if !found {
                return CUDA_ERROR_INVALID_VALUE;
            }
            *(data as *mut u64) = base;
        }
        CU_POINTER_ATTRIBUTE_SYNC_MEMOPS => *(data as *mut i32) = 1,
        // MAPPED is a genuine yes/no about whether the pointer is backed, so `found` IS the answer here
        // rather than a precondition for it.
        CU_POINTER_ATTRIBUTE_MAPPED => *(data as *mut i32) = found as i32,
        CU_POINTER_ATTRIBUTE_RANGE_START_ADDR => {
            if !found {
                return CUDA_ERROR_INVALID_VALUE;
            }
            *(data as *mut u64) = base;
        }
        CU_POINTER_ATTRIBUTE_RANGE_SIZE => {
            if !found {
                return CUDA_ERROR_INVALID_VALUE;
            }
            *(data as *mut usize) = size as usize;
        }
        _ => return CUDA_ERROR_NOT_SUPPORTED,
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuPointerGetAttribute(data: *mut c_void, attr: i32, ptr: u64) -> i32 {
    if data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if let Err(code) = ShimState::with(|s| s.require_context()) {
        return code;
    }
    unsafe { pointer_attr(attr, data, ptr) }
}

#[no_mangle]
pub extern "C" fn cuPointerGetAttributes(
    n: u32,
    attrs: *mut i32,
    data: *mut *mut c_void,
    ptr: u64,
) -> i32 {
    if attrs.is_null() || data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if let Err(code) = ShimState::with(|s| s.require_context()) {
        return code;
    }
    for i in 0..n as usize {
        let attr = unsafe { *attrs.add(i) };
        let slot = unsafe { *data.add(i) };
        let r = unsafe { pointer_attr(attr, slot, ptr) };
        // An unsupported attribute is skipped (its slot is left untouched), matching the driver's
        // batch semantics; any hard error aborts the batch.
        if r != CUDA_SUCCESS && r != CUDA_ERROR_NOT_SUPPORTED {
            return r;
        }
    }
    CUDA_SUCCESS
}

// ==================================================================================================
// IR-wired: memory
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuMemAlloc_v2(dptr: *mut u64, bytesize: usize) -> i32 {
    if dptr.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        match allocate::mem_alloc(&mut s.ctx, &mut s.sink, bytesize as u64) {
            Ok(p) => {
                unsafe { *dptr = p.0 };
                CUDA_SUCCESS
            }
            Err(e) => DriverStatus::from(&e).code(),
        }
    })
}

#[no_mangle]
pub extern "C" fn cuMemFree_v2(dptr: u64) -> i32 {
    ShimState::with_context(|s| match allocate::mem_free(&mut s.ctx, &mut s.sink, DevicePtr(dptr)) {
        Ok(()) => CUDA_SUCCESS,
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

#[no_mangle]
pub extern "C" fn cuMemcpyHtoD_v2(dst: u64, src: *const c_void, n: usize) -> i32 {
    // `CInput::bytes` yields an empty slice for a null pointer, which is a fair contract for a borrow
    // helper but silently turns an n-byte request into a zero-byte copy: the bounds check then passes
    // trivially, an empty WriteBuffer is submitted, and the caller is told the copy succeeded while the
    // destination still holds what it held before. The device→host direction already refuses a null
    // host pointer, so the two directions disagreed about the same mistake. n == 0 stays legal.
    if src.is_null() && n != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let host = unsafe { CInput::bytes(src, n) };
    ShimState::with_context(|s| {
        match transfer::memcpy_htod(&mut s.ctx, &mut s.sink, DevicePtr(dst), host) {
            Ok(()) => CUDA_SUCCESS,
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        }
    })
}

#[no_mangle]
pub extern "C" fn cuMemcpyDtoD_v2(dst: u64, src: u64, n: usize) -> i32 {
    ShimState::with_context(|s| {
        match transfer::memcpy_dtod(
            &mut s.ctx,
            &mut s.sink,
            DevicePtr(dst),
            DevicePtr(src),
            n as u64,
        ) {
            Ok(()) => CUDA_SUCCESS,
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        }
    })
}

/// `cuMemcpyDtoH_v2` resolves the device source and reads `n` bytes back through the sink's device→host
/// readback path (`CommandSink::read_buffer`), copying them into the caller's host `dst`. A dangling source
/// or a failed readback → `CUDA_ERROR_INVALID_VALUE`.
#[no_mangle]
pub extern "C" fn cuMemcpyDtoH_v2(dst: *mut c_void, src: u64, n: usize) -> i32 {
    if dst.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match transfer::read_dtoh(&s.ctx, &mut s.sink, DevicePtr(src), n) {
        Ok(bytes) => {
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len())
            };
            CUDA_SUCCESS
        }
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

// ==================================================================================================
// IR-wired: memory (host pinned/registered, managed, pitched, memset, async copies)
// ==================================================================================================

/// `cuMemAllocHost_v2(pp, size)` — a page-locked host allocation. Hands back the base of a real host
/// buffer the model owns (usable directly as a `cuMemcpy*` host source/destination).
#[no_mangle]
pub extern "C" fn cuMemAllocHost_v2(pp: *mut *mut c_void, size: usize) -> i32 {
    if pp.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match s.ctx.host_alloc(size) {
        Some(base) => {
            unsafe { *pp = base as *mut c_void };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_OUT_OF_MEMORY,
    })
}

/// `cuMemHostAlloc(pp, size, flags)` — the flagged pinned-allocation form; the modeled semantics do not
/// depend on the (portable / mapped / write-combined) flags, so it shares `cuMemAllocHost`'s body.
#[no_mangle]
pub extern "C" fn cuMemHostAlloc(pp: *mut *mut c_void, size: usize, _flags: u32) -> i32 {
    cuMemAllocHost_v2(pp, size)
}

/// `cuMemFreeHost(p)` — free a pinned allocation. A bogus / already-freed pointer is `INVALID_VALUE`.
#[no_mangle]
pub extern "C" fn cuMemFreeHost(p: *mut c_void) -> i32 {
    if p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match s.ctx.host_free(p as u64) {
        Ok(()) => CUDA_SUCCESS,
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

/// `cuMemHostRegister_v2(p, size, flags)` — page-lock an existing guest host range. Registering the same
/// base twice is `INVALID_VALUE`.
#[no_mangle]
pub extern "C" fn cuMemHostRegister_v2(p: *mut c_void, size: usize, _flags: u32) -> i32 {
    if p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match s.ctx.host_register(p as u64, size as u64) {
        Ok(()) => CUDA_SUCCESS,
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

/// `cuMemHostUnregister(p)` — unlock a previously registered host range. An unregistered base is
/// `INVALID_VALUE`.
#[no_mangle]
pub extern "C" fn cuMemHostUnregister(p: *mut c_void) -> i32 {
    if p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match s.ctx.host_unregister(p as u64) {
        Ok(()) => CUDA_SUCCESS,
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

/// `cuMemHostGetDevicePointer_v2(pdptr, p, flags)` — the device pointer that maps host allocation `p`
/// (lazily creating its backing device buffer). A pointer that is not a live host allocation is
/// `INVALID_VALUE`.
#[no_mangle]
pub extern "C" fn cuMemHostGetDevicePointer_v2(
    pdptr: *mut u64,
    p: *mut c_void,
    _flags: u32,
) -> i32 {
    if pdptr.is_null() || p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(
        |s| match s.ctx.host_get_device_pointer(&mut s.sink, p as u64) {
            Ok(ptr) => {
                unsafe { *pdptr = ptr.0 };
                CUDA_SUCCESS
            }
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        },
    )
}

/// `cuMemAllocManaged(dptr, bytesize, flags)` — a managed (unified) allocation: a device buffer that is
/// also host-addressable in the model.
#[no_mangle]
pub extern "C" fn cuMemAllocManaged(dptr: *mut u64, bytesize: usize, _flags: u32) -> i32 {
    if dptr.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        match allocate::mem_alloc_managed(&mut s.ctx, &mut s.sink, bytesize as u64) {
            Ok(p) => {
                unsafe { *dptr = p.0 };
                CUDA_SUCCESS
            }
            Err(e) => DriverStatus::from(&e).code(),
        }
    })
}

/// `cuMemAllocPitch_v2(dptr, pPitch, widthBytes, height, elementSizeBytes)` — a 2D allocation with a
/// 512-byte-aligned row pitch, returned through `pPitch`.
#[no_mangle]
pub extern "C" fn cuMemAllocPitch_v2(
    dptr: *mut u64,
    p_pitch: *mut usize,
    width_bytes: usize,
    height: usize,
    element_size: u32,
) -> i32 {
    if dptr.is_null() || p_pitch.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        match allocate::mem_alloc_pitch(
            &mut s.ctx,
            &mut s.sink,
            width_bytes as u64,
            height as u64,
            element_size,
        ) {
            Ok((p, pitch)) => {
                unsafe {
                    *dptr = p.0;
                    *p_pitch = pitch as usize;
                }
                CUDA_SUCCESS
            }
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        }
    })
}

/// Shared memset body: lower the `(value, width, count)` fill into the device buffer at `dst`. The
/// expansion happens inside [`transfer::memset_elements`], which bounds `width * count` (checked, against
/// the destination allocation) BEFORE allocating the fill buffer — so a huge `n` can never overflow
/// `width * n` nor drive an unbounded multi-GiB `Vec` here.
fn memset_sync(dst: u64, value: u64, width: usize, n: usize) -> i32 {
    if n == 0 {
        return CUDA_SUCCESS;
    }
    ShimState::with_context(|s| {
        match transfer::memset_elements(&mut s.ctx, &mut s.sink, DevicePtr(dst), value, width, n) {
            Ok(()) => CUDA_SUCCESS,
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        }
    })
}

/// Shared stream-ordered memset body: validate the stream, then lower the same bounded fill as
/// [`memset_sync`].
fn memset_stream(dst: u64, value: u64, width: usize, n: usize, hstream: *mut c_void) -> i32 {
    if n == 0 {
        return CUDA_SUCCESS;
    }
    ShimState::with_context(|s| {
        let Some(st) = s.stream(hstream) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match transfer::memset_elements_async(
            &mut s.ctx,
            &mut s.sink,
            st,
            DevicePtr(dst),
            value,
            width,
            n,
        ) {
            Ok(()) => CUDA_SUCCESS,
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        }
    })
}

#[no_mangle]
pub extern "C" fn cuMemsetD8_v2(dst: u64, uc: u8, n: usize) -> i32 {
    memset_sync(dst, uc as u64, 1, n)
}

#[no_mangle]
pub extern "C" fn cuMemsetD16_v2(dst: u64, us: u16, n: usize) -> i32 {
    memset_sync(dst, us as u64, 2, n)
}

#[no_mangle]
pub extern "C" fn cuMemsetD32_v2(dst: u64, ui: u32, n: usize) -> i32 {
    memset_sync(dst, ui as u64, 4, n)
}

#[no_mangle]
pub extern "C" fn cuMemsetD8Async(dst: u64, uc: u8, n: usize, s: *mut c_void) -> i32 {
    memset_stream(dst, uc as u64, 1, n, s)
}

#[no_mangle]
pub extern "C" fn cuMemsetD16Async(dst: u64, us: u16, n: usize, s: *mut c_void) -> i32 {
    memset_stream(dst, us as u64, 2, n, s)
}

#[no_mangle]
pub extern "C" fn cuMemsetD32Async(dst: u64, ui: u32, n: usize, s: *mut c_void) -> i32 {
    memset_stream(dst, ui as u64, 4, n, s)
}

/// `cuMemcpyHtoDAsync_v2(dst, src, n, stream)` — stream-ordered HtoD; records the same `WriteBuffer` as
/// the synchronous `cuMemcpyHtoD` once the stream is validated.
#[no_mangle]
pub extern "C" fn cuMemcpyHtoDAsync_v2(
    dst: u64,
    src: *const c_void,
    n: usize,
    s: *mut c_void,
) -> i32 {
    // Same hole as the synchronous form above.
    if src.is_null() && n != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let host = unsafe { CInput::bytes(src, n) };
    ShimState::with_context(|st| {
        let Some(stream) = st.stream(s) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match transfer::memcpy_htod_async(&mut st.ctx, &mut st.sink, stream, DevicePtr(dst), host) {
            Ok(()) => CUDA_SUCCESS,
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        }
    })
}

/// `cuMemcpyDtoDAsync_v2(dst, src, n, stream)` — stream-ordered on-device copy.
#[no_mangle]
pub extern "C" fn cuMemcpyDtoDAsync_v2(dst: u64, src: u64, n: usize, s: *mut c_void) -> i32 {
    ShimState::with_context(|st| {
        let Some(stream) = st.stream(s) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match transfer::memcpy_dtod_async(
            &mut st.ctx,
            &mut st.sink,
            stream,
            DevicePtr(dst),
            DevicePtr(src),
            n as u64,
        ) {
            Ok(()) => CUDA_SUCCESS,
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        }
    })
}

/// `cuMemcpyDtoHAsync_v2(dst, src, n, stream)` — stream-ordered device→host readback; reads the bytes back
/// through the sink like the synchronous `cuMemcpyDtoH`.
#[no_mangle]
pub extern "C" fn cuMemcpyDtoHAsync_v2(
    dst: *mut c_void,
    src: u64,
    n: usize,
    s: *mut c_void,
) -> i32 {
    if dst.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|st| {
        let Some(stream) = st.stream(s) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match transfer::read_dtoh_async(&st.ctx, &mut st.sink, stream, DevicePtr(src), n) {
            Ok(bytes) => {
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len())
                };
                CUDA_SUCCESS
            }
            Err(_) => CUDA_ERROR_INVALID_VALUE,
        }
    })
}

// ==================================================================================================
// IR-wired: module (PTX)
// ==================================================================================================
