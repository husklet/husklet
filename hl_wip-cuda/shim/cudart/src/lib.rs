//! Guest cdylib deployed as `libcudart.so.1` — the CUDA Runtime API drop-in.
//!
//! The exported `cuda*`/`__cuda*` surface is code-generated from `registry/cudart.manifest` (`build.rs`)
//! so it can never drift from the golden 49-entry set. The memory + device + stream basics have real
//! hand-written bodies in [`runtime`] that call the `hl_cuda` lowering services through a process-global
//! [`hl_gpu::RemoteCommandSink`] ([`state`]); the fatbin-registration launch tail are benign default
//! stubs ([`stub`]). The soname `libcudart.so.1` is baked by `build.rs`.

#![allow(non_snake_case)]

pub mod runtime;
pub mod state;
pub mod stub;

/// The CUDA `dim3` launch-geometry type (by-value `{x, y, z}`), referenced by the generated stubs for
/// `cudaLaunchKernel` / `__cudaPushCallConfiguration`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Dim3 {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

// The generated C-ABI export surface: every entry point not hand-written in `runtime`.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported CUDA Runtime API entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = CUDART_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::runtime::*;
    use super::*;
    use core::ffi::{c_char, c_void};

    #[test]
    fn surface_is_complete_and_matches_the_census() {
        assert_eq!(CUDART_ENTRYPOINTS, 49, "CUDA runtime surface drifted from the golden 49");
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
        // The whole surface has real hand-written bodies — no generated default stubs remain.
        assert_eq!(GENERATED_STUBS, 0, "cudart still has default stubs");
    }

    // One serial test drives the sink-free entry points (device/props/events/streams/errors/config), so
    // the process-global state is never raced across parallel tests.
    #[test]
    fn runtime_entry_points_roundtrip() {
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

        // versions
        let mut ver = 0i32;
        assert_eq!(cudaDriverGetVersion(&mut ver), 0);
        assert_eq!(ver, 12020);
        assert_eq!(cudaRuntimeGetVersion(&mut ver), 0);
        assert_eq!(ver, 12020);

        // device properties: name at offset 0, major/minor readable at their fixed offsets
        let mut buf = vec![0u8; 4096];
        assert_eq!(cudaGetDeviceProperties(buf.as_mut_ptr() as *mut c_void, 0), 0);
        let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char) }
            .to_string_lossy()
            .into_owned();
        assert!(name.contains("CUDA-sim"), "unexpected device name: {name}");
        assert_eq!(cudaGetDeviceProperties(core::ptr::null_mut(), 0), CUDART_ERR_INVALID_DEVICE);

        // PCI bus id
        let mut pci = [0 as c_char; 32];
        assert_eq!(cudaDeviceGetPCIBusId(pci.as_mut_ptr(), 32, 0), 0);
        let pci_s = unsafe { std::ffi::CStr::from_ptr(pci.as_ptr()) }.to_string_lossy().into_owned();
        assert_eq!(pci_s, "0000:00:00.0");

        // func attributes: an unregistered/null func falls back to the modeled defaults (success).
        let mut fattr = vec![0u8; 256];
        assert_eq!(cudaFuncGetAttributes(fattr.as_mut_ptr() as *mut c_void, core::ptr::null()), 0);
        assert_eq!(cudaFuncGetAttributes(core::ptr::null_mut(), core::ptr::null()), CUDART_ERR_INVALID_VALUE);

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
            assert_eq!(cudaFuncGetAttributes(ra.as_mut_ptr() as *mut c_void, host_fn), 0);
            // CudaFuncAttributes #[repr(C)]: shared_size_bytes @0 (usize), num_regs @28 (i32).
            let shared = usize::from_le_bytes(ra[0..8].try_into().unwrap());
            let num_regs = i32::from_le_bytes(ra[28..32].try_into().unwrap());
            assert!(num_regs > 0, "vecadd uses registers, got {num_regs}");
            assert_eq!(shared, 0, "vecadd declares no static shared memory");
        }

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
        assert_eq!(cudaEventRecord(bogus, core::ptr::null_mut()), CUDART_ERR_INVALID_RESOURCE_HANDLE);
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
        assert_eq!(__cudaPushCallConfiguration(g, bl, 128, core::ptr::null_mut()), 0);
        let (mut og, mut ob) = (Dim3 { x: 0, y: 0, z: 0 }, Dim3 { x: 0, y: 0, z: 0 });
        let mut oshm = 0usize;
        let mut ostream: *mut c_void = core::ptr::null_mut();
        assert_eq!(
            __cudaPopCallConfiguration(&mut og, &mut ob, &mut oshm, &mut ostream as *mut _ as *mut c_void),
            0
        );
        assert_eq!((og.x, ob.x, ob.y, oshm), (4, 64, 2, 128));
        // empty stack pops to cudaErrorInvalidConfiguration (9)
        assert_eq!(
            __cudaPopCallConfiguration(&mut og, &mut ob, &mut oshm, core::ptr::null_mut()),
            9
        );
    }

    // Local copies of the result codes the assertions above reference (kept crate-private).
    const CUDART_ERR_INVALID_VALUE: i32 = 1;
    const CUDART_ERR_INVALID_DEVICE: i32 = 101;
    const CUDART_ERR_INVALID_RESOURCE_HANDLE: i32 = 400;
    const CUDART_ERR_NOT_READY: i32 = 600;

    /// The default-stream handle (null token).
    fn stream_default() -> *mut c_void {
        core::ptr::null_mut()
    }

    /// Wrap PTX text in a minimal nvcc-style fatbin container (one uncompressed PTX entry) — the exact
    /// shape `__cudaRegisterFatBinary`'s `container_bytes` walks (bare container, magic 0xba55ed50).
    fn make_fatbin(ptx: &str) -> Vec<u8> {
        let payload = ptx.as_bytes();
        let payload_len = payload.len() as u64;
        let fat_size = 64u64 + payload_len; // one 64-byte entry header + the payload
        let mut c = Vec::new();
        c.extend_from_slice(&0xba55_ed50u32.to_le_bytes()); // magic
        c.extend_from_slice(&1u16.to_le_bytes()); // version
        c.extend_from_slice(&16u16.to_le_bytes()); // header_size
        c.extend_from_slice(&fat_size.to_le_bytes()); // fat_size
        let mut e = [0u8; 64];
        e[0..2].copy_from_slice(&1u16.to_le_bytes()); // kind = PTX
        e[4..8].copy_from_slice(&64u32.to_le_bytes()); // entry header_size
        e[8..16].copy_from_slice(&payload_len.to_le_bytes()); // payload_size (flags @40 stay 0 → uncompressed)
        c.extend_from_slice(&e);
        c.extend_from_slice(payload);
        c
    }
}
