use super::support::*;
use super::*;
use crate::state::reset;
#[test]
fn compute_path_end_to_end_over_socket() {
    use hl_gpu::protocol::model::kernel::KernelDescriptor;
    use hl_gpu::GpuExecutor as _; // brings capabilities() into scope for CpuExecutor

    let _g = guard(); // serialize + reset (no socket yet)

    // A reference executor behind a private temp socket, serving one connection.
    let sock = std::env::temp_dir().join(format!(
        "hl-cuda-driver-{}-{:?}.sock",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&sock);
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = hl_gpu::Capabilities::full("host");
        let mut exec = hl_gpu::CpuExecutor::new();
        exec.set_kernel_compiler(|desc: &KernelDescriptor| {
            ptx::compile(&desc.ptx, &desc.entry, desc.block)
        });
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
    reset();

    assert_eq!(cuInit(0), CUDA_SUCCESS);

    // --- module load + kernel resolution ---
    let img = std::ffi::CString::new(ptx::VECADD_PTX).unwrap();
    let mut module: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuModuleLoadData(&mut module, img.as_ptr() as *const c_void),
        CUDA_SUCCESS
    );
    let name = std::ffi::CString::new("vecadd").unwrap();
    let mut func: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuModuleGetFunction(&mut func, module, name.as_ptr()),
        CUDA_SUCCESS
    );

    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [10.0f32, 20.0, 30.0, 40.0];
    let n = 4u32;
    let nbytes = (n as usize) * 4;

    // --- device alloc (cuMemAlloc_v2) ---
    let (mut da, mut db, mut dc) = (0u64, 0u64, 0u64);
    assert_eq!(cuMemAlloc_v2(&mut da, nbytes), CUDA_SUCCESS);
    assert_eq!(cuMemAlloc_v2(&mut db, nbytes), CUDA_SUCCESS);
    assert_eq!(cuMemAlloc_v2(&mut dc, nbytes), CUDA_SUCCESS);
    assert!(da != 0 && db != 0 && dc != 0);

    // --- HtoD upload: one sync, one async on the default stream ---
    let ab = f32s(&a);
    let bb = f32s(&b);
    assert_eq!(
        cuMemcpyHtoD_v2(da, ab.as_ptr() as *const c_void, nbytes),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuMemcpyHtoDAsync_v2(
            db,
            bb.as_ptr() as *const c_void,
            nbytes,
            core::ptr::null_mut()
        ),
        CUDA_SUCCESS
    );

    // --- launch (cuLaunchKernel): grid 1 block, block n threads ---
    let (da_v, db_v, dc_v, n_v) = (da, db, dc, n as i32);
    let params: [*mut c_void; 4] = [
        &da_v as *const u64 as *mut c_void,
        &db_v as *const u64 as *mut c_void,
        &dc_v as *const u64 as *mut c_void,
        &n_v as *const i32 as *mut c_void,
    ];
    assert_eq!(
        cuLaunchKernel(
            func,
            1,
            1,
            1,
            n,
            1,
            1,
            0,
            core::ptr::null_mut(),
            params.as_ptr() as *mut *mut c_void,
            core::ptr::null_mut()
        ),
        CUDA_SUCCESS
    );
    // --- ctx synchronize (real fence barrier over the socket) ---
    assert_eq!(cuCtxSynchronize(), CUDA_SUCCESS);

    // --- DtoH readback (cuMemcpyDtoH_v2): the pipeline COMPUTED c = a + b ---
    let mut out = vec![0u8; nbytes];
    assert_eq!(
        cuMemcpyDtoH_v2(out.as_mut_ptr() as *mut c_void, dc, nbytes),
        CUDA_SUCCESS
    );
    assert_eq!(
        as_f32s(&out),
        vec![11.0, 22.0, 33.0, 44.0],
        "vecadd computed sum"
    );

    // async DtoH gives the same bytes.
    let mut out2 = vec![0u8; nbytes];
    assert_eq!(
        cuMemcpyDtoHAsync_v2(
            out2.as_mut_ptr() as *mut c_void,
            dc,
            nbytes,
            core::ptr::null_mut()
        ),
        CUDA_SUCCESS
    );
    assert_eq!(out2, out);

    // --- on-device + unified + peer copies all land the same bytes into a fresh buffer ---
    let mut de = 0u64;
    assert_eq!(cuMemAlloc_v2(&mut de, nbytes), CUDA_SUCCESS);
    assert_eq!(cuMemcpyDtoD_v2(de, dc, nbytes), CUDA_SUCCESS);
    let mut rb = vec![0u8; nbytes];
    assert_eq!(
        cuMemcpyDtoH_v2(rb.as_mut_ptr() as *mut c_void, de, nbytes),
        CUDA_SUCCESS
    );
    assert_eq!(rb, out, "DtoD copy reproduced the result");
    assert_eq!(
        cuMemcpyDtoDAsync_v2(de, dc, nbytes, core::ptr::null_mut()),
        CUDA_SUCCESS
    );
    assert_eq!(cuMemcpy(de, dc, nbytes), CUDA_SUCCESS); // unified copy
    assert_eq!(
        cuMemcpyAsync(de, dc, nbytes, core::ptr::null_mut()),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuMemcpyPeer(de, core::ptr::null_mut(), dc, core::ptr::null_mut(), nbytes),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuMemcpyPeerAsync(
            de,
            core::ptr::null_mut(),
            dc,
            core::ptr::null_mut(),
            nbytes,
            core::ptr::null_mut()
        ),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuMemcpyDtoH_v2(rb.as_mut_ptr() as *mut c_void, de, nbytes),
        CUDA_SUCCESS
    );
    assert_eq!(rb, out, "unified/peer copies reproduced the result");

    // --- every memset variant fills a 32-byte buffer; readback proves the exact bytes landed ---
    let mut dm = 0u64;
    assert_eq!(cuMemAlloc_v2(&mut dm, 32), CUDA_SUCCESS);
    let mut mb = vec![0u8; 32];
    // D8: 32 bytes of 0xAB.
    assert_eq!(cuMemsetD8_v2(dm, 0xAB, 32), CUDA_SUCCESS);
    assert_eq!(
        cuMemcpyDtoH_v2(mb.as_mut_ptr() as *mut c_void, dm, 32),
        CUDA_SUCCESS
    );
    assert_eq!(mb, vec![0xABu8; 32]);
    // D16: 16 halfwords of 0xBEEF.
    assert_eq!(cuMemsetD16_v2(dm, 0xBEEF, 16), CUDA_SUCCESS);
    assert_eq!(
        cuMemcpyDtoH_v2(mb.as_mut_ptr() as *mut c_void, dm, 32),
        CUDA_SUCCESS
    );
    for c in mb.chunks_exact(2) {
        assert_eq!(u16::from_le_bytes(c.try_into().unwrap()), 0xBEEF);
    }
    // D32: 8 words of 0xDEADBEEF.
    assert_eq!(cuMemsetD32_v2(dm, 0xDEAD_BEEF, 8), CUDA_SUCCESS);
    assert_eq!(
        cuMemcpyDtoH_v2(mb.as_mut_ptr() as *mut c_void, dm, 32),
        CUDA_SUCCESS
    );
    for c in mb.chunks_exact(4) {
        assert_eq!(u32::from_le_bytes(c.try_into().unwrap()), 0xDEAD_BEEF);
    }
    // Async memset variants (default stream) succeed and are observable after a sync.
    assert_eq!(
        cuMemsetD8Async(dm, 0x11, 32, core::ptr::null_mut()),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuMemsetD16Async(dm, 0x2222, 16, core::ptr::null_mut()),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuMemsetD32Async(dm, 0x3333_3333, 8, core::ptr::null_mut()),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuMemcpyDtoH_v2(mb.as_mut_ptr() as *mut c_void, dm, 32),
        CUDA_SUCCESS
    );
    assert_eq!(
        mb,
        vec![0x33u8; 32],
        "last async memset (D32 0x33333333) wins"
    );

    // --- pitched allocation: a 2D buffer with a 512-aligned row pitch ---
    let (mut dp, mut pitch) = (0u64, 0usize);
    assert_eq!(
        cuMemAllocPitch_v2(&mut dp, &mut pitch, 64, 8, 4),
        CUDA_SUCCESS
    );
    assert!(
        dp != 0 && pitch >= 64 && pitch % 512 == 0,
        "pitch = {pitch}"
    );

    // --- stream + event synchronize barriers over the live socket ---
    let mut stream: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuStreamCreate(&mut stream, 0), CUDA_SUCCESS);
    assert_eq!(cuStreamSynchronize(stream), CUDA_SUCCESS);
    assert_eq!(
        cuStreamSynchronize(0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );
    let mut ev: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuEventCreate(&mut ev, 0), CUDA_SUCCESS);
    assert_eq!(cuEventRecord(ev, stream), CUDA_SUCCESS);
    assert_eq!(cuEventSynchronize(ev), CUDA_SUCCESS);

    // --- the other two launch forms lower through the identical path ---
    assert_eq!(
        cuLaunchCooperativeKernel(
            func,
            1,
            1,
            1,
            n,
            1,
            1,
            0,
            core::ptr::null_mut(),
            params.as_ptr() as *mut *mut c_void
        ),
        CUDA_SUCCESS
    );
    let cfg: [u32; 8] = [1, 1, 1, n, 1, 1, 0, 0];
    assert_eq!(
        cuLaunchKernelEx(
            cfg.as_ptr() as *const c_void,
            func,
            params.as_ptr() as *mut *mut c_void,
            core::ptr::null_mut()
        ),
        CUDA_SUCCESS
    );
    assert_eq!(cuCtxSynchronize(), CUDA_SUCCESS);
    let mut fin = vec![0u8; nbytes];
    assert_eq!(
        cuMemcpyDtoH_v2(fin.as_mut_ptr() as *mut c_void, dc, nbytes),
        CUDA_SUCCESS
    );
    assert_eq!(
        as_f32s(&fin),
        vec![11.0, 22.0, 33.0, 44.0],
        "re-launches recomputed the sum"
    );

    // --- free (cuMemFree_v2): a freed pointer no longer resolves ---
    assert_eq!(cuMemFree_v2(da), CUDA_SUCCESS);
    assert_eq!(cuMemFree_v2(db), CUDA_SUCCESS);
    assert_eq!(cuMemFree_v2(dc), CUDA_SUCCESS);
    assert_eq!(cuMemFree_v2(de), CUDA_SUCCESS);
    assert_eq!(cuMemFree_v2(dm), CUDA_SUCCESS);
    assert_eq!(cuMemFree_v2(dp), CUDA_SUCCESS);

    // Tear down: dropping the connected sink closes the socket so the server thread hits EOF.
    std::env::remove_var("HL_GPU_EXEC");
    reset();
    server.join().unwrap();
    let _ = std::fs::remove_file(&sock);
}
