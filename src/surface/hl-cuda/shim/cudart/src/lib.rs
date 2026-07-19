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
    /// Serializes the tests that drive the process-global [`crate::state`] (which is a single
    /// `OnceLock<Mutex<State>>` shared across the whole test binary) so their `reset()` + `$HL_GPU_EXEC`
    /// manipulation never interleave under the default parallel test runner.
    static STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn runtime_entry_points_roundtrip() {
        let _serial = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

        // cudaGetDeviceProperties_v2 fills the same struct as the legacy alias: name @0, major/minor set.
        let mut p2 = vec![0u8; 4096];
        assert_eq!(cudaGetDeviceProperties_v2(p2.as_mut_ptr() as *mut c_void, 0), 0);
        let n2 = unsafe { std::ffi::CStr::from_ptr(p2.as_ptr() as *const c_char) }
            .to_string_lossy()
            .into_owned();
        assert!(n2.contains("CUDA-sim"), "v2 name: {n2}");
        assert_eq!(cudaGetDeviceProperties_v2(core::ptr::null_mut(), 0), CUDART_ERR_INVALID_DEVICE);

        // cudaStreamCreateWithFlags mints a usable stream (shares cudaStreamCreate's body).
        let mut sf: *mut c_void = core::ptr::null_mut();
        assert_eq!(cudaStreamCreateWithFlags(&mut sf, 0x1 /* cudaStreamNonBlocking */), 0);
        assert!(!sf.is_null());
        assert_eq!(cudaStreamQuery(sf), 0);
        assert_eq!(cudaStreamDestroy(sf), 0);

        // cudaHostAlloc hands back real writable pinned host memory (shares cudaMallocHost's body).
        let mut ha: *mut c_void = core::ptr::null_mut();
        assert_eq!(cudaHostAlloc(&mut ha, 128, 0x2 /* cudaHostAllocMapped */), 0);
        assert!(!ha.is_null());
        unsafe { *(ha as *mut u8).add(64) = 0x5A };
        assert_eq!(unsafe { *(ha as *mut u8).add(64) }, 0x5A);
        assert_eq!(cudaFreeHost(ha), 0);

        // cudaPeekAtLastError reads the sticky error WITHOUT clearing it; cudaGetLastError clears it;
        // cudaDeviceReset clears it back to success. A failing call sets the sticky error truthfully.
        assert_eq!(cudaSetDevice(7), CUDART_ERR_INVALID_DEVICE); // sets last_error = 101
        assert_eq!(cudaPeekAtLastError(), CUDART_ERR_INVALID_DEVICE);
        assert_eq!(cudaPeekAtLastError(), CUDART_ERR_INVALID_DEVICE, "peek does not clear");
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

    /// A runtime-backed host that serves BOTH the submit path (through a runtime `Session` + reference
    /// `CpuExecutor`) and the device→host readback path over the real socket transport — so the cudart
    /// shim's process-global `RemoteCommandSink` has a live executor for `cudaMalloc`/`cudaMemset`/
    /// `cudaMemcpy` to actually land against (mirrors `hl-gpu/tests/readback.rs::RuntimeHost`).
    struct RuntimeHost {
        session: hl_gpu::Session,
        exec: hl_gpu::CpuExecutor,
    }
    impl hl_gpu::ConnectionHandler for RuntimeHost {
        fn submit(&mut self, _h: &hl_gpu::transport::SubmitHeader, batch: &[hl_gpu::Cmd]) -> hl_gpu::transport::Verdict {
            let frame_bytes = hl_gpu::Encoder::stream(batch).len();
            match hl_gpu::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch) {
                Ok(_) => hl_gpu::transport::Verdict::Ack,
                Err(_) => hl_gpu::transport::Verdict::Nack,
            }
        }
        fn read_buffer(&mut self, req: &hl_gpu::ReadbackRequest) -> Option<Vec<u8>> {
            hl_gpu::runtime::service::dispatch::read_buffer(
                &self.session,
                &self.exec,
                hl_gpu::BufferId(req.id),
                req.offset,
                req.len as usize,
            )
            .ok()
        }
    }

    // A `cudaMemset`/`cudaMemsetAsync` whose `count` far exceeds the destination allocation must return the
    // truthful `cudaErrorInvalidValue` WITHOUT building the `vec![value; count]` fill buffer first (a hostile
    // `count` near `usize::MAX` would otherwise be an unbounded multi-GiB host alloc → OOM-abort). A legal
    // memset must still write EXACTLY the right bytes — asserted via a real device→host readback.
    #[test]
    fn memset_hostile_count_is_bounded_and_legal_memset_is_exact() {
        let _serial = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // A runtime-backed host on a private temp socket, serving one connection (submit + readback).
        let sock = std::env::temp_dir().join(format!(
            "hl-cudart-memset-{}-{:?}.sock",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            use hl_gpu::GpuExecutor as _; // brings `capabilities()` into scope for `CpuExecutor`
            let (stream, _) = listener.accept().unwrap();
            let caps = hl_gpu::Capabilities::full("host");
            let exec = hl_gpu::CpuExecutor::new();
            let limits = hl_gpu::Limits::from_capabilities(exec.capabilities());
            let session = hl_gpu::Session::new(
                limits,
                hl_gpu::GlobalLedger::unbounded(),
                Box::new(hl_gpu::FakeClock::new(0)),
            );
            let mut host = RuntimeHost { session, exec };
            let _ = hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host);
        });

        // Point the process-global sink at our socket and rebuild the state so it reconnects there.
        std::env::set_var("HL_GPU_EXEC", sock.to_string_lossy().into_owned());
        crate::state::reset();

        // A small 64-byte device allocation.
        let mut dptr: *mut c_void = core::ptr::null_mut();
        assert_eq!(cudaMalloc(&mut dptr, 64), 0);
        assert!(!dptr.is_null());

        // HOSTILE: a count far larger than the 64-byte allocation → truthful cudaErrorInvalidValue, and
        // (the point of the fix) NO multi-GiB fill allocation / OOM-abort / panic — the bound is applied
        // BEFORE the fill buffer is built.
        assert_eq!(cudaMemset(dptr, 0xAB, usize::MAX), CUDART_ERR_INVALID_VALUE);
        assert_eq!(cudaMemset(dptr, 0xAB, 65), CUDART_ERR_INVALID_VALUE); // one byte past the end
        assert_eq!(
            cudaMemsetAsync(dptr, 0xAB, usize::MAX, core::ptr::null_mut()),
            CUDART_ERR_INVALID_VALUE
        );
        // The sticky last-error reflects the failure — not a false success.
        assert_eq!(cudaGetLastError(), CUDART_ERR_INVALID_VALUE);

        // LEGAL full fill: exactly 64 bytes of 0xCD, verified by an exact device→host readback.
        assert_eq!(cudaMemset(dptr, 0xCD, 64), 0);
        let mut host_buf = vec![0u8; 64];
        assert_eq!(cudaMemcpy(host_buf.as_mut_ptr() as *mut c_void, dptr, 64, 2 /* DtoH */), 0);
        assert_eq!(host_buf, vec![0xCDu8; 64], "legal full memset must write exactly 64 bytes of 0xCD");

        // LEGAL interior fill: clear, then fill 16 bytes at offset 8 with 0xEE — exactly that window
        // changes, the rest stays zero (proves the bounded path still writes the correct range/offset).
        assert_eq!(cudaMemset(dptr, 0x00, 64), 0);
        let mid = unsafe { (dptr as *mut u8).add(8) } as *mut c_void;
        assert_eq!(cudaMemset(mid, 0xEE, 16), 0);
        let mut rb = vec![0u8; 64];
        assert_eq!(cudaMemcpy(rb.as_mut_ptr() as *mut c_void, dptr, 64, 2), 0);
        for (i, b) in rb.iter().enumerate() {
            let want = if (8..24).contains(&i) { 0xEEu8 } else { 0x00 };
            assert_eq!(*b, want, "byte {i} after interior memset");
        }

        // cudaMemcpyAsync (HtoD then DtoH) against the live executor: the async copy lowers identically to
        // the synchronous one, so a round-trip through device memory returns the exact source bytes.
        let src: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(5)).collect();
        assert_eq!(
            cudaMemcpyAsync(dptr, src.as_ptr() as *const c_void, 64, 1 /* HtoD */, core::ptr::null_mut()),
            0
        );
        let mut got = vec![0u8; 64];
        assert_eq!(
            cudaMemcpyAsync(got.as_mut_ptr() as *mut c_void, dptr, 64, 2 /* DtoH */, core::ptr::null_mut()),
            0
        );
        assert_eq!(got, src, "cudaMemcpyAsync round-trips the exact bytes through device memory");

        // Synchronization barriers really submit + wait on a timeline fence over the live socket.
        assert_eq!(cudaDeviceSynchronize(), 0);
        assert_eq!(cudaThreadSynchronize(), 0);
        let mut stream: *mut c_void = core::ptr::null_mut();
        assert_eq!(cudaStreamCreate(&mut stream), 0);
        assert_eq!(cudaStreamSynchronize(stream), 0);
        assert_eq!(cudaStreamSynchronize(core::ptr::null_mut()), 0); // default stream
        let bogus = 0x9999usize as *mut c_void;
        assert_eq!(cudaStreamSynchronize(bogus), 1 /* cudaErrorInvalidValue */);
        assert_eq!(cudaStreamDestroy(stream), 0);

        // cudaHostGetDevicePointer maps a pinned host allocation to a live device pointer (lazily backing
        // it with a real device buffer over the sink); an unregistered host pointer is invalid.
        let mut hp: *mut c_void = core::ptr::null_mut();
        assert_eq!(cudaHostAlloc(&mut hp, 256, 0), 0);
        let mut hdev: *mut c_void = core::ptr::null_mut();
        assert_eq!(cudaHostGetDevicePointer(&mut hdev, hp, 0), 0);
        assert!(!hdev.is_null());
        let mut junk = [0u8; 8];
        let foreign = junk.as_mut_ptr() as *mut c_void;
        let mut jdev: *mut c_void = core::ptr::null_mut();
        assert_eq!(cudaHostGetDevicePointer(&mut jdev, foreign, 0), 1 /* invalid */);
        assert_eq!(cudaFreeHost(hp), 0);

        // Tear down: freeing + resetting the state drops the connected sink, closing the socket so the
        // server thread hits EOF and returns.
        assert_eq!(cudaFree(dptr), 0);
        std::env::remove_var("HL_GPU_EXEC");
        crate::state::reset();
        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }
}
