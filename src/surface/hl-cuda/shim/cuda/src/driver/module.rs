use super::*;
#[no_mangle]
pub extern "C" fn cuModuleLoadData(module: *mut *mut c_void, image: *const c_void) -> i32 {
    if module.is_null() || image.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // The driver API hands `image` without a length; a PTX image is nul-terminated text, so read to nul.
    let Some(img) = (unsafe { CInput::string(image as *const c_char) }) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    ShimState::with_context(|s| match s.ctx.load_module(&img) {
        Ok(id) => {
            let h = s.intern_module(id);
            unsafe { *module = h };
            CUDA_SUCCESS
        }
        Err(e) => DriverStatus::from(&e).code(),
    })
}

#[no_mangle]
pub extern "C" fn cuModuleGetFunction(
    hfunc: *mut *mut c_void,
    hmod: *mut c_void,
    name: *const c_char,
) -> i32 {
    if hfunc.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let Some(nm) = (unsafe { CInput::string(name) }) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let Ok(nm) = std::str::from_utf8(&nm) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    ShimState::with_context(|s| {
        let Some(module_id) = s.module_id(hmod) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match load_module::module_get_function(&s.ctx, module_id, nm) {
            Ok(f) => {
                let h = s.intern_function(f, nm);
                unsafe { *hfunc = h };
                CUDA_SUCCESS
            }
            Err(e) => DriverStatus::from(&e).code(),
        }
    })
}

/// `cuModuleLoad(module, fname)` — load a module from a file on the guest filesystem. Reads the file and
/// loads it through the same [`CudaContext::load_module`] path as `cuModuleLoadData` (so a fatbin
/// container or raw PTX text both work). A missing file is `CUDA_ERROR_FILE_NOT_FOUND`.
#[no_mangle]
pub extern "C" fn cuModuleLoad(module: *mut *mut c_void, fname: *const c_char) -> i32 {
    if module.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let Some(path) = (unsafe { CInput::string(fname) }) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let Ok(path) = std::str::from_utf8(&path) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return CUDA_ERROR_FILE_NOT_FOUND;
    };
    ShimState::with_context(|s| match s.ctx.load_module(&bytes) {
        Ok(id) => {
            let h = s.intern_module(id);
            unsafe { *module = h };
            CUDA_SUCCESS
        }
        Err(e) => DriverStatus::from(&e).code(),
    })
}

/// `cuModuleLoadDataEx(module, image, n, options, optionValues)` — the JIT-option form. The modeled
/// module load ignores JIT options (the executor compiles PTX itself), so it shares `cuModuleLoadData`.
#[no_mangle]
pub extern "C" fn cuModuleLoadDataEx(
    module: *mut *mut c_void,
    image: *const c_void,
    n: u32,
    o: *mut i32,
    ov: *mut *mut c_void,
) -> i32 {
    let _ = (n, o, ov);
    cuModuleLoadData(module, image)
}

/// `cuModuleLoadFatBinary(module, image)` — load from an nvcc fatbin container. The shared
/// [`CudaContext::load_module`] already walks a fatbin to recover its embedded PTX, so this is
/// `cuModuleLoadData` on the same image.
#[no_mangle]
pub extern "C" fn cuModuleLoadFatBinary(module: *mut *mut c_void, image: *const c_void) -> i32 {
    cuModuleLoadData(module, image)
}

/// `cuModuleUnload(m)` — the modeled context keeps a module's parsed source for the process lifetime (a
/// launch may still reference it), so unload validates the handle and is otherwise a no-op. A bogus handle
/// is `CUDA_ERROR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn cuModuleUnload(m: *mut c_void) -> i32 {
    ShimState::with_context(|s| {
        if s.module_id(m).is_some() {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

/// `cuModuleGetGlobal_v2(dptr, bytes, m, name)` — resolve a `__device__`/`__constant__` global symbol to
/// its backing device pointer + byte size. The size comes from the module's parsed PTX `.global`/`.const`
/// declaration; the backing buffer is created lazily on first lookup (see [`load_module::module_get_global`]).
/// A symbol the module does not declare is `CUDA_ERROR_NOT_FOUND` (never a fake pointer).
#[no_mangle]
pub extern "C" fn cuModuleGetGlobal_v2(
    dptr: *mut u64,
    bytes: *mut usize,
    m: *mut c_void,
    name: *const c_char,
) -> i32 {
    let Some(nm) = (unsafe { CInput::string(name) }) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let Ok(nm) = std::str::from_utf8(&nm) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    ShimState::with_context(|s| {
        let Some(module_id) = s.module_id(m) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match load_module::module_get_global(&mut s.ctx, &mut s.sink, module_id, nm) {
            Ok(Some((ptr, size))) => {
                if !dptr.is_null() {
                    unsafe { *dptr = ptr.0 };
                }
                if !bytes.is_null() {
                    unsafe { *bytes = size as usize };
                }
                CUDA_SUCCESS
            }
            Ok(None) => CUDA_ERROR_NOT_FOUND,
            Err(e) => DriverStatus::from(&e).code(),
        }
    })
}

/// `cuModuleGetLoadingMode(mode)` — the driver's module loading mode. The model loads modules eagerly.
#[no_mangle]
pub extern "C" fn cuModuleGetLoadingMode(mode: *mut i32) -> i32 {
    if mode.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *mode = CU_MODULE_EAGER_LOADING };
    CUDA_SUCCESS
}

/// `cuModuleGetTexRef(t, m, name)` — the PTX model has no texture-reference variables, so a valid module
/// honestly reports the symbol absent (`CUDA_ERROR_NOT_FOUND`) rather than handing back a null texref as a
/// false success. A bogus module handle is `CUDA_ERROR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn cuModuleGetTexRef(
    t: *mut *mut c_void,
    m: *mut c_void,
    name: *const c_char,
) -> i32 {
    let _ = (t, name);
    ShimState::with_context(|s| {
        if s.module_id(m).is_some() {
            CUDA_ERROR_NOT_FOUND
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

/// `cuModuleGetSurfRef(s, m, name)` — as `cuModuleGetTexRef`, for surface references (also unmodeled).
#[no_mangle]
pub extern "C" fn cuModuleGetSurfRef(
    sref: *mut *mut c_void,
    m: *mut c_void,
    name: *const c_char,
) -> i32 {
    let _ = (sref, name);
    ShimState::with_context(|s| {
        if s.module_id(m).is_some() {
            CUDA_ERROR_NOT_FOUND
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

// ==================================================================================================
// IR-wired: unified + peer copies (single-device model → device→device)
// ==================================================================================================

/// `cuMemcpy(dst, src, n)` — a unified-addressing copy. Both pointers live in the one unified VA, so a
/// generic copy is a device→device copy (a pointer that is not a live device allocation is a hard error,
/// never a fake success).
#[no_mangle]
pub extern "C" fn cuMemcpy(dst: u64, src: u64, n: usize) -> i32 {
    cuMemcpyDtoD_v2(dst, src, n)
}

/// `cuMemcpyAsync(dst, src, n, stream)` — the stream-ordered unified copy; validates the stream, then
/// records the same on-device copy as [`cuMemcpy`].
#[no_mangle]
pub extern "C" fn cuMemcpyAsync(dst: u64, src: u64, n: usize, s: *mut c_void) -> i32 {
    cuMemcpyDtoDAsync_v2(dst, src, n, s)
}

/// `cuMemcpyPeer(dst, dstCtx, src, srcCtx, n)` — a peer (cross-context) copy. The model has a single
/// device with one unified VA, so a peer copy is a device→device copy; the contexts are irrelevant.
#[no_mangle]
pub extern "C" fn cuMemcpyPeer(
    dst: u64,
    dctx: *mut c_void,
    src: u64,
    sctx: *mut c_void,
    n: usize,
) -> i32 {
    let _ = (dctx, sctx);
    cuMemcpyDtoD_v2(dst, src, n)
}

/// `cuMemcpyPeerAsync(dst, dstCtx, src, srcCtx, n, stream)` — the stream-ordered peer copy (single-device
/// model → stream-ordered device→device).
#[no_mangle]
pub extern "C" fn cuMemcpyPeerAsync(
    dst: u64,
    dctx: *mut c_void,
    src: u64,
    sctx: *mut c_void,
    n: usize,
    s: *mut c_void,
) -> i32 {
    let _ = (dctx, sctx);
    cuMemcpyDtoDAsync_v2(dst, src, n, s)
}

// ==================================================================================================
// IR-wired: kernel launch
// ==================================================================================================
