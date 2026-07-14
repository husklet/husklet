//! PROOF: one REAL app runs a CUDA vecadd end-to-end through the ACTUAL staged `libcuda.so.1` shim.
//!
//! This is NOT the in-process lowering (`hl_wip-cuda/tests/e2e.rs` covers that). Here the "app" `dlopen()`s
//! the real, separately-compiled guest shared object that `hl_wip-cuda`'s build.rs staged at
//! `~/.hl/cuda/aarch64/libcuda.so.1`, resolves each `cu*` entry point with `dlsym`, and calls the CUDA
//! Driver API. Inside that `.so`, a process-global `RemoteCommandSink` (built from `$HL_GPU_EXEC`) frames
//! every lowered batch and ships it over a unix socket. A host thread in THIS process serves that socket
//! with a runtime-backed reference `CpuExecutor`, so the whole product data path runs:
//!
//!   app → dlsym(cu*) → libcuda.so.1 → hl_cuda lowering → RemoteCommandSink → unix socket
//!        → serve_connection_with_handler → runtime (validate/account/dispatch) → CpuExecutor
//!        → cuMemcpyDtoH readback over the socket → assert c == a + b == [11, 22, 33, 44].
//!
//! The only host-side wiring the neutral `hl_gpu` crate cannot supply itself is the PTX front-end (a driver
//! concern): we inject `hl_cuda::adapter::ptx::compile` as the executor's kernel compiler, exactly as the
//! composition root would. Everything the guest touches is the real shipped shim.

use core::ffi::{c_char, c_void};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;

use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    encode_stream, BufferId, Capabilities, Cmd, ConnectionHandler, CpuExecutor, FakeClock,
    GlobalLedger, GpuExecutor, Limits, ReadbackRequest, Session,
};

// ===================================================================================================
// host side: the GPU-executor socket server (runtime + CpuExecutor + injected PTX front-end)
// ===================================================================================================

/// A host that owns a runtime `Session` + a `CpuExecutor` with the PTX kernel compiler injected, and serves
/// BOTH the submit path (through the runtime pipeline) and device→host readback (through the executor's
/// device memory). One `&mut self` drives both halves — the same shape as `hl_wip-gpu/tests/readback.rs`'s
/// `RuntimeHost`, with the kernel compiler added so a `cuLaunchKernel`'s KERNEL payload compiles for real.
struct RuntimeHost {
    session: Session,
    exec: CpuExecutor,
}

impl RuntimeHost {
    fn new() -> Self {
        let mut exec = CpuExecutor::new();
        // Inject the driver's PTX parser so a shim-produced `CreateShader { PtxKernel, .. }` (payload = a
        // `KernelDescriptor` carrying the PTX source + entry + block dims) compiles on the fly.
        exec.set_kernel_compiler(|desc: &KernelDescriptor| {
            hl_cuda::adapter::ptx::compile(&desc.ptx, &desc.entry, desc.block)
        });
        let limits = Limits::from_capabilities(exec.capabilities());
        let session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
        Self { session, exec }
    }
}

impl ConnectionHandler for RuntimeHost {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        let frame_bytes = encode_stream(batch).len();
        match hl_gpu::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch) {
            Ok(_) => Verdict::Ack,
            Err(_) => Verdict::Nack,
        }
    }

    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            &self.exec,
            BufferId(req.id),
            req.offset,
            req.len as usize,
        )
        .ok()
    }
}

/// A unique temp socket path for this test, removed on drop.
struct TempSock(PathBuf);
impl TempSock {
    fn new() -> Self {
        let p = std::env::temp_dir()
            .join(format!("hl-realapp-vecadd-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        TempSock(p)
    }
}
impl Drop for TempSock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ===================================================================================================
// guest side: load the REAL staged libcuda.so.1 and bind the cu* entry points via dlsym
// ===================================================================================================

/// The subset of the CUDA Driver API this app calls, resolved from the staged `.so`. The signatures match
/// the shim's EXPORTED symbol names (the versioned `_v2` entry points are what a real CUDA header remaps
/// the un-suffixed names to at compile time).
struct Cuda {
    _handle: *mut c_void, // keep the dlopen handle alive for the process
    cu_init: unsafe extern "C" fn(u32) -> i32,
    cu_device_get: unsafe extern "C" fn(*mut i32, i32) -> i32,
    cu_ctx_create: unsafe extern "C" fn(*mut *mut c_void, u32, i32) -> i32,
    cu_module_load_data: unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> i32,
    cu_module_get_function:
        unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *const c_char) -> i32,
    cu_mem_alloc: unsafe extern "C" fn(*mut u64, usize) -> i32,
    cu_memcpy_htod: unsafe extern "C" fn(u64, *const c_void, usize) -> i32,
    #[allow(clippy::type_complexity)]
    cu_launch_kernel: unsafe extern "C" fn(
        *mut c_void, // function
        u32, u32, u32, // grid  x/y/z
        u32, u32, u32, // block x/y/z
        u32,           // shared mem bytes
        *mut c_void,   // stream
        *mut *mut c_void, // kernelParams
        *mut *mut c_void, // extra
    ) -> i32,
    cu_memcpy_dtoh: unsafe extern "C" fn(*mut c_void, u64, usize) -> i32,
    cu_mem_free: unsafe extern "C" fn(u64) -> i32,
}

/// `dlsym` `name` out of `handle`, transmuting to the target fn-pointer type. Panics (failing the test) if
/// the symbol is absent — that would mean the staged `.so` does not export the real entry point.
unsafe fn sym<T>(handle: *mut c_void, name: &str) -> T {
    let cname = std::ffi::CString::new(name).unwrap();
    let p = libc::dlsym(handle, cname.as_ptr());
    assert!(!p.is_null(), "dlsym({name}) returned null — staged libcuda.so.1 is missing the symbol");
    // Transmute the resolved address into the fn pointer. `T` is always a `unsafe extern "C" fn(..)`.
    std::mem::transmute_copy::<*mut c_void, T>(&p)
}

impl Cuda {
    /// `dlopen` the staged `.so` and resolve every entry point the app uses.
    unsafe fn load(so_path: &str) -> Self {
        let cpath = std::ffi::CString::new(so_path).unwrap();
        let handle = libc::dlopen(cpath.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if handle.is_null() {
            let err = libc::dlerror();
            let msg = if err.is_null() {
                "unknown".to_string()
            } else {
                std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned()
            };
            panic!("dlopen({so_path}) failed: {msg}");
        }
        Cuda {
            cu_init: sym(handle, "cuInit"),
            cu_device_get: sym(handle, "cuDeviceGet"),
            cu_ctx_create: sym(handle, "cuCtxCreate_v2"),
            cu_module_load_data: sym(handle, "cuModuleLoadData"),
            cu_module_get_function: sym(handle, "cuModuleGetFunction"),
            cu_mem_alloc: sym(handle, "cuMemAlloc_v2"),
            cu_memcpy_htod: sym(handle, "cuMemcpyHtoD_v2"),
            cu_launch_kernel: sym(handle, "cuLaunchKernel"),
            cu_memcpy_dtoh: sym(handle, "cuMemcpyDtoH_v2"),
            cu_mem_free: sym(handle, "cuMemFree_v2"),
            _handle: handle,
        }
    }
}

const CUDA_SUCCESS: i32 = 0;

fn f32s_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

#[test]
fn real_libcuda_shim_runs_vecadd_over_the_socket() {
    // --- locate the staged shim -----------------------------------------------------------------
    let home = std::env::var("HOME").expect("HOME set");
    let so_path = format!("{home}/.hl/cuda/aarch64/libcuda.so.1");
    assert!(
        std::path::Path::new(&so_path).exists(),
        "staged shim missing at {so_path} — build it first: \
         cargo build --manifest-path hl_wip-cuda/shim/cuda/Cargo.toml"
    );

    // --- stand up the host GPU-executor socket BEFORE the shim connects --------------------------
    let sock = TempSock::new();
    let sock_path = sock.0.to_string_lossy().into_owned();
    let listener = UnixListener::bind(&sock.0).expect("bind executor socket");

    // Point the shim's process-global RemoteCommandSink at our socket. The shim reads $HL_GPU_EXEC when its
    // global State is first initialized (first cu* call), so set it before any cu* call / dlopen.
    std::env::set_var("HL_GPU_EXEC", &sock_path);

    // Serve the ONE persistent guest connection on a background thread. We do NOT join it: the shim's sink
    // is a process-global that never drops, so the connection stays open until the test process exits — the
    // thread is torn down at exit. By the time cuMemcpyDtoH returns we already have the computed bytes.
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept guest connection");
        let caps = Capabilities::full("host");
        let mut host = RuntimeHost::new();
        // Serve submit + readback frames until the guest closes the connection.
        let _ = hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host);
    });

    // --- the app: load the real shim and drive the CUDA Driver API -------------------------------
    let cu = unsafe { Cuda::load(&so_path) };

    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [10.0f32, 20.0, 30.0, 40.0];
    let n: u32 = a.len() as u32;
    let bytes = (n as usize) * 4;

    unsafe {
        assert_eq!((cu.cu_init)(0), CUDA_SUCCESS, "cuInit");

        let mut dev: i32 = -1;
        assert_eq!((cu.cu_device_get)(&mut dev, 0), CUDA_SUCCESS, "cuDeviceGet");
        assert_eq!(dev, 0);

        let mut ctx: *mut c_void = std::ptr::null_mut();
        assert_eq!((cu.cu_ctx_create)(&mut ctx, 0, dev), CUDA_SUCCESS, "cuCtxCreate");
        assert!(!ctx.is_null(), "context handle is non-null");

        // cuModuleLoadData wants a nul-terminated PTX image. Use the driver crate's canonical VECADD_PTX so
        // the exact same source the lowering tests use is what the shim parses.
        let ptx = std::ffi::CString::new(hl_cuda::adapter::ptx::VECADD_PTX).unwrap();
        let mut module: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            (cu.cu_module_load_data)(&mut module, ptx.as_ptr() as *const c_void),
            CUDA_SUCCESS,
            "cuModuleLoadData(VECADD_PTX)"
        );

        let entry = std::ffi::CString::new("vecadd").unwrap();
        let mut func: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            (cu.cu_module_get_function)(&mut func, module, entry.as_ptr()),
            CUDA_SUCCESS,
            "cuModuleGetFunction(vecadd)"
        );
        assert!(!func.is_null(), "function handle is non-null");

        // Three device allocations (two inputs + one output).
        let mut da: u64 = 0;
        let mut db: u64 = 0;
        let mut dc: u64 = 0;
        assert_eq!((cu.cu_mem_alloc)(&mut da, bytes), CUDA_SUCCESS, "cuMemAlloc a");
        assert_eq!((cu.cu_mem_alloc)(&mut db, bytes), CUDA_SUCCESS, "cuMemAlloc b");
        assert_eq!((cu.cu_mem_alloc)(&mut dc, bytes), CUDA_SUCCESS, "cuMemAlloc c");

        // Upload the two inputs (HtoD → WriteBuffer over the socket).
        let ha = f32s_to_bytes(&a);
        let hb = f32s_to_bytes(&b);
        assert_eq!(
            (cu.cu_memcpy_htod)(da, ha.as_ptr() as *const c_void, bytes),
            CUDA_SUCCESS,
            "cuMemcpyHtoD a"
        );
        assert_eq!(
            (cu.cu_memcpy_htod)(db, hb.as_ptr() as *const c_void, bytes),
            CUDA_SUCCESS,
            "cuMemcpyHtoD b"
        );

        // cuLaunchKernel: grid = 1 block, block = n threads. kernelParams is a void** whose slots point at
        // each argument value: three device pointers (u64) + the element count (int).
        let n_i: i32 = n as i32;
        let mut params: [*mut c_void; 4] = [
            &da as *const u64 as *mut c_void,
            &db as *const u64 as *mut c_void,
            &dc as *const u64 as *mut c_void,
            &n_i as *const i32 as *mut c_void,
        ];
        assert_eq!(
            (cu.cu_launch_kernel)(
                func,
                1, 1, 1,       // grid
                n, 1, 1,       // block
                0,             // shared mem bytes
                std::ptr::null_mut(), // default stream
                params.as_mut_ptr(),
                std::ptr::null_mut(), // extra
            ),
            CUDA_SUCCESS,
            "cuLaunchKernel(vecadd)"
        );

        // cuMemcpyDtoH: the shim resolves the output pointer and reads the bytes back over the socket.
        let mut out = vec![0u8; bytes];
        assert_eq!(
            (cu.cu_memcpy_dtoh)(out.as_mut_ptr() as *mut c_void, dc, bytes),
            CUDA_SUCCESS,
            "cuMemcpyDtoH c"
        );
        let got: Vec<f32> =
            out.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();

        // The whole product data path actually COMPUTED the elementwise sum through the real .so.
        assert_eq!(got, vec![11.0, 22.0, 33.0, 44.0], "vecadd result read back over the socket");

        // Free the allocations cleanly (each lowers + submits over the socket too).
        assert_eq!((cu.cu_mem_free)(da), CUDA_SUCCESS, "cuMemFree a");
        assert_eq!((cu.cu_mem_free)(db), CUDA_SUCCESS, "cuMemFree b");
        assert_eq!((cu.cu_mem_free)(dc), CUDA_SUCCESS, "cuMemFree c");
    }
}
