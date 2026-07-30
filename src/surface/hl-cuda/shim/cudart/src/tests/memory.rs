//! The memory and synchronization paths against a real in-process executor over the socket transport.

use super::super::runtime::*;
use core::ffi::c_void;

use super::support::*;

#[test]
fn memset_hostile_count_is_bounded_and_legal_memset_is_exact() {
    let _serial = crate::state::serial();

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
        let caps = hl_gpu::Capabilities::permissive_fixture("host");
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
    assert_eq!(
        cudaMemcpy(
            host_buf.as_mut_ptr() as *mut c_void,
            dptr,
            64,
            2 /* DtoH */
        ),
        0
    );
    assert_eq!(
        host_buf,
        vec![0xCDu8; 64],
        "legal full memset must write exactly 64 bytes of 0xCD"
    );

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
        cudaMemcpyAsync(
            dptr,
            src.as_ptr() as *const c_void,
            64,
            1, /* HtoD */
            core::ptr::null_mut()
        ),
        0
    );
    let mut got = vec![0u8; 64];
    assert_eq!(
        cudaMemcpyAsync(
            got.as_mut_ptr() as *mut c_void,
            dptr,
            64,
            2, /* DtoH */
            core::ptr::null_mut()
        ),
        0
    );
    assert_eq!(
        got, src,
        "cudaMemcpyAsync round-trips the exact bytes through device memory"
    );

    // Synchronization barriers really submit + wait on a timeline fence over the live socket.
    assert_eq!(cudaDeviceSynchronize(), 0);
    assert_eq!(cudaThreadSynchronize(), 0);
    let mut stream: *mut c_void = core::ptr::null_mut();
    assert_eq!(cudaStreamCreate(&mut stream), 0);
    assert_eq!(cudaStreamSynchronize(stream), 0);
    assert_eq!(cudaStreamSynchronize(core::ptr::null_mut()), 0); // default stream
    let bogus = 0x9999usize as *mut c_void;
    assert_eq!(
        cudaStreamSynchronize(bogus),
        CUDART_ERR_INVALID_RESOURCE_HANDLE
    );
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
    assert_eq!(
        cudaHostGetDevicePointer(&mut jdev, foreign, 0),
        1 /* invalid */
    );
    assert_eq!(cudaFreeHost(hp), 0);

    // Tear down: freeing + resetting the state drops the connected sink, closing the socket so the
    // server thread hits EOF and returns.
    assert_eq!(cudaFree(dptr), 0);
    std::env::remove_var("HL_GPU_EXEC");
    crate::state::reset();
    server.join().unwrap();
    let _ = std::fs::remove_file(&sock);
}
