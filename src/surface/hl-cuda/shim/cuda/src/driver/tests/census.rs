use super::support::*;
use super::*;
#[test]
fn mem_host_get_flags_rejects_a_non_host_pointer() {
    let _g = guard();
    // A live pinned allocation reports flags 0 (the modeled allocator ignores host-alloc flags).
    let mut hp: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuMemAllocHost_v2(&mut hp, 32), CUDA_SUCCESS);
    let mut fl = 9u32;
    assert_eq!(cuMemHostGetFlags(&mut fl, hp), CUDA_SUCCESS);
    assert_eq!(fl, 0);
    // A pointer we never page-locked is INVALID_VALUE — not a fake success reporting flags for memory
    // that is not a host allocation the model owns.
    let mut junk = [0u8; 8];
    let foreign = junk.as_mut_ptr() as *mut c_void;
    assert_eq!(
        cuMemHostGetFlags(&mut fl, foreign),
        CUDA_ERROR_INVALID_VALUE
    );
    // Freeing the pinned allocation makes its pointer foreign again.
    assert_eq!(cuMemFreeHost(hp), CUDA_SUCCESS);
    assert_eq!(cuMemHostGetFlags(&mut fl, hp), CUDA_ERROR_INVALID_VALUE);
    assert_eq!(
        cuMemHostGetFlags(&mut fl, core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );
}

// ---- bring-up + query surface (no command sink needed) ----------------------------------------

#[test]
fn bringup_device_and_context_queries() {
    let _g = guard();

    // cuInit + driver version.
    assert_eq!(cuInit(0), CUDA_SUCCESS);
    let mut ver = -1i32;
    assert_eq!(cuDriverGetVersion(&mut ver), CUDA_SUCCESS);
    assert_eq!(ver, DRIVER_VERSION);
    assert_eq!(
        cuDriverGetVersion(core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );

    // error name / string lookups.
    let mut sp: *const c_char = core::ptr::null();
    assert_eq!(
        cuGetErrorName(CUDA_ERROR_OUT_OF_MEMORY, &mut sp),
        CUDA_SUCCESS
    );
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(sp) }.to_str().unwrap(),
        "CUDA_ERROR_OUT_OF_MEMORY"
    );
    assert_eq!(cuGetErrorString(CUDA_SUCCESS, &mut sp), CUDA_SUCCESS);
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(sp) }.to_str().unwrap(),
        "no error"
    );
    assert_eq!(
        cuGetErrorName(0, core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );

    // device enumeration + identity.
    let mut count = -1i32;
    assert_eq!(cuDeviceGetCount(&mut count), CUDA_SUCCESS);
    assert_eq!(count, 1);
    let mut d = -1i32;
    assert_eq!(cuDeviceGet(&mut d, 0), CUDA_SUCCESS);
    assert_eq!(d, 0);
    assert_eq!(cuDeviceGet(&mut d, 1), CUDA_ERROR_INVALID_VALUE); // no second device

    // GL interop enumerates that same logical device for each defined selector and obeys capacity.
    for list in 1..=3 {
        let mut gl_count = u32::MAX;
        let mut gl_device = -1;
        assert_eq!(
            unsafe { cuGLGetDevices_v2(&mut gl_count, &mut gl_device, 1, list) },
            CUDA_SUCCESS
        );
        assert_eq!((gl_count, gl_device), (1, 0));
    }
    let mut gl_count = u32::MAX;
    assert_eq!(
        unsafe { cuGLGetDevices_v2(&mut gl_count, core::ptr::null_mut(), 0, 1) },
        CUDA_SUCCESS
    );
    assert_eq!(gl_count, 0);
    assert_eq!(
        unsafe { cuGLGetDevices_v2(core::ptr::null_mut(), &mut d, 1, 1) },
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        unsafe { cuGLGetDevices_v2(&mut gl_count, &mut d, 1, 0) },
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        unsafe { cuGraphicsResourceSetMapFlags_v2(core::ptr::null_mut(), 0) },
        CUDA_ERROR_INVALID_HANDLE
    );
    let mut graphics_resource = 1usize as *mut c_void;
    assert_eq!(
        unsafe { cuGraphicsGLRegisterImage(&mut graphics_resource, 7, 0x8513, 0) },
        CUDA_ERROR_NOT_SUPPORTED
    );
    assert_eq!(graphics_resource, 1usize as *mut c_void);
    for target in [0x84f5, 0x8d41] {
        assert_eq!(
            unsafe { cuGraphicsGLRegisterImage(&mut graphics_resource, 7, target, 0) },
            CUDA_ERROR_NOT_SUPPORTED
        );
    }
    assert_eq!(
        unsafe { cuGraphicsGLRegisterImage(&mut graphics_resource, 7, 0xdead, 0) },
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        unsafe { cuGraphicsGLRegisterImage(&mut graphics_resource, 7, 0x0de1, 3) },
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(graphics_resource, 1usize as *mut c_void);
    let mut mapped_array = 1usize as *mut c_void;
    for resource in [
        core::ptr::null_mut(),
        1usize as *mut c_void,
        usize::MAX as *mut c_void,
    ] {
        assert_eq!(
            unsafe { cuGraphicsSubResourceGetMappedArray(&mut mapped_array, resource, 0, 0) },
            CUDA_ERROR_INVALID_HANDLE
        );
        assert_eq!(mapped_array, 1usize as *mut c_void);
    }
    assert_eq!(
        unsafe {
            cuGraphicsSubResourceGetMappedArray(
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                0,
                0,
            )
        },
        CUDA_ERROR_INVALID_VALUE
    );

    let want = ShimState::with(|s| s.ctx.device.clone());
    let mut name = [0 as c_char; 128];
    assert_eq!(cuDeviceGetName(name.as_mut_ptr(), 128, 0), CUDA_SUCCESS);
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
            .to_str()
            .unwrap(),
        want.name
    );

    let mut total = 0usize;
    assert_eq!(cuDeviceTotalMem_v2(&mut total, 0), CUDA_SUCCESS);
    assert_eq!(total, want.total_mem as usize);

    let (mut maj, mut min) = (-1i32, -1i32);
    assert_eq!(
        cuDeviceComputeCapability(&mut maj, &mut min, 0),
        CUDA_SUCCESS
    );
    assert_eq!((maj as u32, min as u32), want.compute_capability);

    // UUID (both spellings write the same 16 bytes).
    let mut u1 = [0u8; 16];
    let mut u2 = [0u8; 16];
    assert_eq!(
        cuDeviceGetUuid(u1.as_mut_ptr() as *mut c_void, 0),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuDeviceGetUuid_v2(u2.as_mut_ptr() as *mut c_void, 0),
        CUDA_SUCCESS
    );
    assert_eq!(u1, want.uuid);
    assert_eq!(u1, u2);

    // context create → set-current → get-device → destroy.
    let mut ctx: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut ctx, 0, 0), CUDA_SUCCESS);
    assert_eq!(cuCtxSetCurrent(ctx), CUDA_SUCCESS);
    let mut cd = -1i32;
    assert_eq!(cuCtxGetDevice(&mut cd), CUDA_SUCCESS);
    assert_eq!(cd, 0);
    assert_eq!(cuCtxDestroy_v2(ctx), CUDA_SUCCESS);
    // after destroy the current context is cleared.
    let mut cur: *mut c_void = 0x1 as *mut c_void;
    assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
    assert!(cur.is_null());

    // primary-context reset + set-flags (device-ordinal validated).
    assert_eq!(cuDevicePrimaryCtxSetFlags_v2(0, 4), CUDA_SUCCESS);
    assert_eq!(cuDevicePrimaryCtxReset_v2(0), CUDA_SUCCESS);
    assert_eq!(cuDevicePrimaryCtxReset_v2(1), CUDA_ERROR_INVALID_DEVICE);
    assert_eq!(
        cuDevicePrimaryCtxSetFlags_v2(1, 0),
        CUDA_ERROR_INVALID_DEVICE
    );
}

#[test]
fn pointer_get_attributes_batch_and_occupancy_flags_and_module_variants() {
    let _g = guard();

    // cuPointerGetAttributes: a batched query of several attributes for one live allocation.
    let ptr = record_alloc(2048);
    let mut mtype = 0u32;
    let mut ordinal = -1i32;
    let mut is_managed = 9u32;
    let attrs = [
        CU_POINTER_ATTRIBUTE_MEMORY_TYPE,
        CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL,
        CU_POINTER_ATTRIBUTE_IS_MANAGED,
    ];
    let data: [*mut c_void; 3] = [
        &mut mtype as *mut u32 as *mut c_void,
        &mut ordinal as *mut i32 as *mut c_void,
        &mut is_managed as *mut u32 as *mut c_void,
    ];
    assert_eq!(
        cuPointerGetAttributes(
            3,
            attrs.as_ptr() as *mut i32,
            data.as_ptr() as *mut *mut c_void,
            ptr
        ),
        CUDA_SUCCESS
    );
    assert_eq!(mtype, CU_MEMORYTYPE_DEVICE);
    assert_eq!(ordinal, 0);
    assert_eq!(is_managed, 0);
    // Null argument arrays are rejected.
    assert_eq!(
        cuPointerGetAttributes(
            1,
            core::ptr::null_mut(),
            data.as_ptr() as *mut *mut c_void,
            ptr
        ),
        CUDA_ERROR_INVALID_VALUE
    );

    // Occupancy WithFlags agrees with the non-flags form for the same function.
    let f = load_vecadd();
    let (mut mg1, mut bs1) = (-1i32, -1i32);
    let (mut mg2, mut bs2) = (-1i32, -1i32);
    assert_eq!(
        cuOccupancyMaxPotentialBlockSize(&mut mg1, &mut bs1, f, core::ptr::null_mut(), 0, 0),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuOccupancyMaxPotentialBlockSizeWithFlags(
            &mut mg2,
            &mut bs2,
            f,
            core::ptr::null_mut(),
            0,
            0,
            0
        ),
        CUDA_SUCCESS
    );
    assert_eq!((mg1, bs1), (mg2, bs2));

    // cuModuleLoadDataEx / cuModuleLoadFatBinary both accept the PTX image and resolve the entry (the
    // model treats a fatbin container and raw PTX text through the same load path).
    let img = std::ffi::CString::new(ptx::VECADD_PTX).unwrap();
    let name = std::ffi::CString::new("vecadd").unwrap();
    for load in ["ex", "fatbin"] {
        let mut m: *mut c_void = core::ptr::null_mut();
        let r = if load == "ex" {
            cuModuleLoadDataEx(
                &mut m,
                img.as_ptr() as *const c_void,
                0,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        } else {
            cuModuleLoadFatBinary(&mut m, img.as_ptr() as *const c_void)
        };
        assert_eq!(r, CUDA_SUCCESS, "load variant {load}");
        let mut func: *mut c_void = core::ptr::null_mut();
        assert_eq!(
            cuModuleGetFunction(&mut func, m, name.as_ptr()),
            CUDA_SUCCESS
        );
        assert!(!func.is_null());
    }

    // cuModuleLoad reads a module from a file; a missing path is FILE_NOT_FOUND.
    let path = std::env::temp_dir().join(format!("hl-cuda-mod-{}.ptx", std::process::id()));
    std::fs::write(&path, ptx::VECADD_PTX).unwrap();
    let cpath = std::ffi::CString::new(path.to_string_lossy().into_owned()).unwrap();
    let mut fm: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuModuleLoad(&mut fm, cpath.as_ptr()), CUDA_SUCCESS);
    let mut ff: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuModuleGetFunction(&mut ff, fm, name.as_ptr()),
        CUDA_SUCCESS
    );
    let missing = std::ffi::CString::new("/nonexistent/hl/does_not_exist.ptx").unwrap();
    assert_eq!(
        cuModuleLoad(&mut fm, missing.as_ptr()),
        CUDA_ERROR_FILE_NOT_FOUND
    );
    let _ = std::fs::remove_file(&path);

    // Stream + event destroy validate their handles.
    let mut stream: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuStreamCreate(&mut stream, 0), CUDA_SUCCESS);
    assert_eq!(cuStreamDestroy_v2(stream), CUDA_SUCCESS);
    assert_eq!(
        cuStreamDestroy_v2(0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );
    let mut ev: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuEventCreate(&mut ev, 0), CUDA_SUCCESS);
    assert_eq!(cuEventDestroy_v2(ev), CUDA_SUCCESS);
    assert_eq!(
        cuEventDestroy_v2(0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );
}

// ---- the IR-wired compute path over a LIVE socket executor ------------------------------------
//
// The entry points below (device alloc/free/pitch, every memcpy + memset variant, the three launch
// forms, and the ctx/stream/event synchronize barriers) lower to protocol `Cmd`s and submit through
// the process-global `RemoteCommandSink` over `$HL_GPU_EXEC`. To exercise their REAL effect (not just
// arg validation) we stand up a reference `CpuExecutor` behind a Unix socket, point the sink at it,
// and read the computed bytes back — the same host wiring `tests/e2e.rs` drives in-process.
