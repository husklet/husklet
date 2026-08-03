//! The generated export census and a round trip through every hand-written runtime entry point.

use super::super::runtime::*;
use super::super::*;
use core::ffi::{c_char, c_void};

use super::support::*;

#[test]
fn surface_is_complete_and_matches_the_census() {
    assert_eq!(
        CUDART_ENTRYPOINTS, 62,
        "CUDA runtime surface drifted from the golden 62"
    );
    assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
    // The whole surface has real hand-written bodies — no generated default stubs remain.
    assert_eq!(GENERATED_STUBS, 0, "cudart still has default stubs");
    let mut semaphore = core::ptr::null_mut();
    assert_eq!(
        cudaImportExternalSemaphore(&mut semaphore, core::ptr::null()),
        CUDART_ERR_INVALID_VALUE
    );
    assert_eq!(
        cudaSignalExternalSemaphoresAsync(
            core::ptr::null(),
            core::ptr::null(),
            1,
            core::ptr::null_mut()
        ),
        CUDART_ERR_INVALID_VALUE
    );
    assert_eq!(
        cudaWaitExternalSemaphoresAsync(
            core::ptr::null(),
            core::ptr::null(),
            1,
            core::ptr::null_mut()
        ),
        CUDART_ERR_INVALID_VALUE
    );
}

#[test]
fn runtime_entry_points_roundtrip() {
    let _serial = crate::state::serial();
    crate::state::reset();

    // device enumeration
    let mut count = -1i32;
    assert_eq!(cudaGetDeviceCount(&mut count), 0);
    assert_eq!(count, 1);
    let mut dev = -1i32;
    assert_eq!(cudaGetDevice(&mut dev), 0);
    assert_eq!(dev, 0);
    assert_eq!(cudaSetDevice(0), 0);
    assert_eq!(cudaSetDevice(1), CUDART_ERR_INVALID_DEVICE); // no second device
    assert_eq!(cudaGLSetGLDevice(0), 0);
    assert_eq!(cudaGLSetGLDevice(1), CUDART_ERR_INVALID_DEVICE);
    assert_eq!(
        unsafe { cudaGraphicsResourceSetMapFlags(core::ptr::null_mut(), 0) },
        CUDART_ERR_INVALID_RESOURCE_HANDLE
    );
    let mut graphics_resource = 1usize as *mut c_void;
    assert_eq!(
        unsafe { cudaGraphicsGLRegisterImage(&mut graphics_resource, 7, 0x8513, 0) },
        CUDART_ERR_INVALID_VALUE
    );
    assert_eq!(graphics_resource, 1usize as *mut c_void);
    assert_eq!(
        unsafe { cudaGraphicsGLRegisterImage(&mut graphics_resource, 7, 0x0de1, 3) },
        CUDART_ERR_INVALID_VALUE
    );
    assert_eq!(graphics_resource, 1usize as *mut c_void);
    let mut mapped_array = 1usize as *mut c_void;
    assert_eq!(
        unsafe {
            cudaGraphicsSubResourceGetMappedArray(&mut mapped_array, core::ptr::null_mut(), 0, 0)
        },
        CUDART_ERR_INVALID_VALUE
    );
    assert_eq!(mapped_array, 1usize as *mut c_void);

    // versions
    let mut ver = 0i32;
    assert_eq!(cudaDriverGetVersion(&mut ver), 0);
    assert_eq!(ver, 12020);
    assert_eq!(cudaRuntimeGetVersion(&mut ver), 0);
    assert_eq!(ver, 12020);

    // device properties: name at offset 0, major/minor readable at their fixed offsets
    let mut buf = vec![0u8; 4096];
    assert_eq!(
        cudaGetDeviceProperties(buf.as_mut_ptr() as *mut c_void, 0),
        0
    );
    let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char) }
        .to_string_lossy()
        .into_owned();
    assert!(name.contains("CUDA-sim"), "unexpected device name: {name}");
    assert_eq!(
        cudaGetDeviceProperties(core::ptr::null_mut(), 0),
        CUDART_ERR_INVALID_DEVICE
    );

    // PCI bus id
    let mut pci = [0 as c_char; 32];
    assert_eq!(cudaDeviceGetPCIBusId(pci.as_mut_ptr(), 32, 0), 0);
    let pci_s = unsafe { std::ffi::CStr::from_ptr(pci.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    assert_eq!(pci_s, "0000:00:00.0");

    // func attributes: an unregistered/null func has no device entry to describe, so it is
    // cudaErrorInvalidDeviceFunction — never plausible constants reported as success.
    let mut fattr = vec![0u8; 256];
    assert_eq!(
        cudaFuncGetAttributes(fattr.as_mut_ptr() as *mut c_void, core::ptr::null()),
        98 /* cudaErrorInvalidDeviceFunction */
    );
    assert_eq!(
        cudaFuncGetAttributes(core::ptr::null_mut(), core::ptr::null()),
        CUDART_ERR_INVALID_VALUE
    );

    // A REGISTERED host stub resolves to its real device kernel, so cudaFuncGetAttributes reports the
    // kernel's TRUE register + static-shared figures (recovered from the module PTX by the same
    // front-end the driver-API cuFuncGetAttribute uses) — not a fabricated constant.
    {
        let fatbin = make_fatbin(hl_cuda::adapter::ptx::VECADD_PTX);
        let handle = __cudaRegisterFatBinary(fatbin.as_ptr() as *mut c_void);
        assert!(!handle.is_null(), "vecadd fatbin registers");
        static STUB: u8 = 0;
        let host_fn = &STUB as *const u8 as *const c_void;
        let dev_name = std::ffi::CString::new("vecadd").unwrap();
        __cudaRegisterFunction(
            handle,
            host_fn as *const c_char,
            core::ptr::null_mut(),
            dev_name.as_ptr(),
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        let mut ra = vec![0u8; 256];
        assert_eq!(
            cudaFuncGetAttributes(ra.as_mut_ptr() as *mut c_void, host_fn),
            0
        );
        // CudaFuncAttributes #[repr(C)]: shared_size_bytes @0 (usize), num_regs @28 (i32).
        let shared = usize::from_le_bytes(ra[0..8].try_into().unwrap());
        let num_regs = i32::from_le_bytes(ra[28..32].try_into().unwrap());
        assert!(num_regs > 0, "vecadd uses registers, got {num_regs}");
        assert_eq!(shared, 0, "vecadd declares no static shared memory");
        // Dynamic shared memory is not
        // expressible in the kernel IR (the PTX front-end rejects `.extern .shared`), so the opt-in
        // maximum must be reported as 0 rather than advertising bytes no kernel can be given.
        let at = crate::runtime::FuncAttrOffset::MAX_DYNAMIC_SHARED_SIZE_BYTES;
        let max_dynamic = i32::from_le_bytes(ra[at..at + 4].try_into().unwrap());
        assert_eq!(
            max_dynamic, 0,
            "dynamic shared memory must not be advertised"
        );

        // __cudaRegisterVar binds a __device__/__constant__ global; hl's PTX model parses only kernel
        // entries, so it is an honest no-op (must not panic across the C ABI).
        let var_name = std::ffi::CString::new("gCounter").unwrap();
        __cudaRegisterVar(
            handle,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            var_name.as_ptr(),
            0,
            4,
            0,
            1,
        );
        // __cudaUnregisterFatBinary drops the handle binding (the module stays resident); a bogus/null
        // handle is a silent no-op, never a crash.
        __cudaUnregisterFatBinary(handle);
        __cudaUnregisterFatBinary(core::ptr::null_mut());
    }

    // cudaGetDeviceProperties_v2 fills the same struct as the unversioned alias: name @0, major/minor set.
    let mut p2 = vec![0u8; 4096];
    assert_eq!(
        cudaGetDeviceProperties_v2(p2.as_mut_ptr() as *mut c_void, 0),
        0
    );
    let n2 = unsafe { std::ffi::CStr::from_ptr(p2.as_ptr() as *const c_char) }
        .to_string_lossy()
        .into_owned();
    assert!(n2.contains("CUDA-sim"), "v2 name: {n2}");
    assert_eq!(
        cudaGetDeviceProperties_v2(core::ptr::null_mut(), 0),
        CUDART_ERR_INVALID_DEVICE
    );

    // cudaStreamCreateWithFlags mints a usable stream (shares cudaStreamCreate's body).
    let mut sf: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cudaStreamCreateWithFlags(&mut sf, 0x1 /* cudaStreamNonBlocking */),
        0
    );
    assert!(!sf.is_null());
    assert_eq!(cudaStreamQuery(sf), 0);
    assert_eq!(cudaStreamDestroy(sf), 0);

    // cudaHostAlloc hands back real writable pinned host memory (shares cudaMallocHost's body).
    let mut ha: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cudaHostAlloc(&mut ha, 128, 0x2 /* cudaHostAllocMapped */),
        0
    );
    assert!(!ha.is_null());
    unsafe { *(ha as *mut u8).add(64) = 0x5A };
    assert_eq!(unsafe { *(ha as *mut u8).add(64) }, 0x5A);
    assert_eq!(cudaFreeHost(ha), 0);

    // cudaPeekAtLastError reads the sticky error WITHOUT clearing it; cudaGetLastError clears it;
    // cudaDeviceReset clears it back to success. A failing call sets the sticky error truthfully.
    assert_eq!(cudaSetDevice(7), CUDART_ERR_INVALID_DEVICE); // sets last_error = 101
    assert_eq!(cudaPeekAtLastError(), CUDART_ERR_INVALID_DEVICE);
    assert_eq!(
        cudaPeekAtLastError(),
        CUDART_ERR_INVALID_DEVICE,
        "peek does not clear"
    );
    assert_eq!(cudaGetLastError(), CUDART_ERR_INVALID_DEVICE); // reads + clears
    assert_eq!(cudaPeekAtLastError(), 0, "cleared after get");
    // reset restores a clean slate (device 0, no sticky error)
    assert_eq!(cudaSetDevice(9), CUDART_ERR_INVALID_DEVICE);
    assert_eq!(cudaDeviceReset(), 0);
    assert_eq!(cudaPeekAtLastError(), 0, "reset clears the sticky error");
    assert_eq!(cudaGetDevice(&mut dev), 0);
    assert_eq!(dev, 0);

    // pinned host memory (no command sink needed)
    let mut hp: *mut c_void = core::ptr::null_mut();
    assert_eq!(cudaMallocHost(&mut hp, 4096), 0);
    assert!(!hp.is_null());
    assert_eq!(cudaFreeHost(hp), 0);
    assert_eq!(cudaFreeHost(core::ptr::null_mut()), 0); // free(NULL) is a valid no-op

    // memory info: free <= total, total == advertised default (8 GiB)
    let (mut free, mut total) = (0usize, 0usize);
    assert_eq!(cudaMemGetInfo(&mut free, &mut total), 0);
    assert_eq!(total, 8usize << 30);
    assert!(free <= total);

    // streams: create / query(ready) / destroy; a bogus handle is a resource-handle error
    let mut stream: *mut c_void = core::ptr::null_mut();
    assert_eq!(cudaStreamCreate(&mut stream), 0);
    assert!(!stream.is_null());
    assert_eq!(cudaStreamQuery(stream), 0);
    let bogus = 999usize as *mut c_void;
    assert_eq!(cudaStreamQuery(bogus), CUDART_ERR_INVALID_RESOURCE_HANDLE);
    assert_eq!(cudaStreamDestroy(stream), 0);

    // events: create → record → query(ready) → elapsed; unrecorded → NotReady; bad handle errors
    let (mut a, mut b): (*mut c_void, *mut c_void) = (core::ptr::null_mut(), core::ptr::null_mut());
    assert_eq!(cudaEventCreate(&mut a), 0);
    assert_eq!(cudaEventCreateWithFlags(&mut b, 0), 0);
    assert_eq!(cudaEventQuery(a), CUDART_ERR_NOT_READY); // created but not recorded
    assert_eq!(cudaEventRecord(a, core::ptr::null_mut()), 0);
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert_eq!(cudaEventRecord(b, core::ptr::null_mut()), 0);
    assert_eq!(cudaEventQuery(a), 0); // recorded → complete
    assert_eq!(cudaEventSynchronize(b), 0);
    let mut ms = -1.0f32;
    assert_eq!(cudaEventElapsedTime(&mut ms, a, b), 0);
    assert!(ms >= 0.0, "elapsed must be non-negative, got {ms}");
    assert_eq!(
        cudaEventRecord(bogus, core::ptr::null_mut()),
        CUDART_ERR_INVALID_RESOURCE_HANDLE
    );
    assert_eq!(cudaStreamWaitEvent(stream_default(), a, 0), 0);
    assert_eq!(cudaEventDestroy(a), 0);
    assert_eq!(cudaEventDestroy(b), 0);
    assert_eq!(cudaEventDestroy(bogus), CUDART_ERR_INVALID_RESOURCE_HANDLE);

    // error string / name round-trip
    let inv = unsafe { std::ffi::CStr::from_ptr(cudaGetErrorName(1)) }.to_string_lossy();
    assert_eq!(inv, "cudaErrorInvalidValue");
    let ok = unsafe { std::ffi::CStr::from_ptr(cudaGetErrorString(0)) }.to_string_lossy();
    assert_eq!(ok, "no error");

    // <<<>>> call-config stack push/pop round-trip
    let g = Dim3 { x: 4, y: 1, z: 1 };
    let bl = Dim3 { x: 64, y: 2, z: 1 };
    assert_eq!(
        __cudaPushCallConfiguration(g, bl, 128, core::ptr::null_mut()),
        0
    );
    let (mut og, mut ob) = (Dim3 { x: 0, y: 0, z: 0 }, Dim3 { x: 0, y: 0, z: 0 });
    let mut oshm = 0usize;
    let mut ostream: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        __cudaPopCallConfiguration(
            &mut og,
            &mut ob,
            &mut oshm,
            &mut ostream as *mut _ as *mut c_void
        ),
        0
    );
    assert_eq!((og.x, ob.x, ob.y, oshm), (4, 64, 2, 128));
    // empty stack pops to cudaErrorInvalidConfiguration (9)
    assert_eq!(
        __cudaPopCallConfiguration(&mut og, &mut ob, &mut oshm, core::ptr::null_mut()),
        9
    );
}
