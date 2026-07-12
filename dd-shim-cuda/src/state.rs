//! The shim's global device/context state + the accumulated dd-gpu IR frame.
//!
//! The `cu*` entry points are free `extern "C"` functions, so their shared mutable state lives behind
//! a process-global `Mutex` (like dd-shim-gl's state, adapted to the single-simulated-device CUDA
//! model). The heavy lifting — the CUDA→dd-gpu-IR translation — is delegated to
//! [`dd_gpu::cuda::CudaContext`], which is the shared, host-authored mapping (memory alloc/copy → IR,
//! PTX module load, kernel launch → compute pipeline + dispatch). We do NOT redefine that mapping; the
//! shim just owns the device-presence values, the C-ABI handle tables, and the frame accumulator, and
//! forwards work into the `CudaContext`.

use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use dd_gpu::backend::GpuBackend;
use dd_gpu::cuda::{CudaContext, CudaDeviceDesc, DevicePtr, Function};
use dd_gpu::replay::replay_stream;
use dd_gpu::software::SoftwareBackend;
use dd_shim_common::transport::{ExecConn, FrameBuilder, Surface};

/// Everything the shim tracks between `cu*` calls.
pub struct CudaState {
    /// `cuInit` was called (the driver spec-guards most calls behind this).
    pub inited: bool,
    /// The CUDA→dd-gpu-IR translator + simulated device (from the shared `dd-gpu` crate).
    pub ctx: CudaContext,
    /// The dd-gpu IR accumulated for the current stream; flushed on synchronize.
    pub frame: FrameBuilder,
    /// The embedded host executor: a real CPU backend that runs the accumulated IR — including the
    /// compute `Dispatch` (the PTX kernel interpreter in `dd_gpu::ptx`) — and holds the resulting
    /// device-buffer bytes for `cuMemcpyDtoH` readback. This is the in-process analog of
    /// `dd-gpu/cuda/cuda_shim.c`'s embedded interpreter (the parity oracle): it makes the shim
    /// FUNCTIONAL end-to-end on this host with no GPU. On a real Apple-silicon host the same IR is
    /// shipped over `$DD_GPU_EXEC` to the host Metal executor instead (see docs).
    pub backend: SoftwareBackend,
    /// `CUfunction` handle table: an opaque handle is `index + 1` (never null).
    functions: Vec<Function>,
    /// `CUcontext` token allocator (opaque, non-null). One simulated device, so contexts are tokens.
    pub next_ctx: usize,
    /// The current context token (`0` = none).
    pub current_ctx: usize,
    /// `CUstream` token allocator (opaque, non-null). The executor is synchronous, so a stream is just
    /// a scheduling token; ordering is preserved by the single accumulated frame.
    pub next_stream: usize,
    /// `CUevent` token allocator (opaque, non-null). Events record/synchronize by flushing.
    pub next_event: usize,
    /// Lazily-opened transport to the host GPU-exec service (only used when `$DD_GPU_EXEC` is set).
    conn: Option<ExecConn>,
}

impl CudaState {
    fn new() -> Self {
        // VRAM the simulated device advertises (carved from unified memory on the real host); override
        // with `$DD_CUDA_VRAM_BYTES`. Default 8 GiB.
        let vram = std::env::var("DD_CUDA_VRAM_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(8u64 << 30);
        CudaState {
            inited: false,
            ctx: CudaContext::new(CudaDeviceDesc::apple_default(vram)),
            frame: FrameBuilder::new(),
            backend: SoftwareBackend::new(),
            functions: Vec::new(),
            next_ctx: 1,
            current_ctx: 0,
            next_stream: 1,
            next_event: 1,
            conn: None,
        }
    }

    /// Register a resolved function, returning its opaque `CUfunction` handle (`index + 1`, non-null).
    pub fn intern_function(&mut self, f: Function) -> *mut c_void {
        self.functions.push(f);
        self.functions.len() as *mut c_void // len == index+1
    }

    /// Resolve a `CUfunction` handle back to its [`Function`]. `None` for null / out-of-range.
    pub fn function(&self, h: *mut c_void) -> Option<Function> {
        let idx = h as usize;
        if idx == 0 {
            return None;
        }
        self.functions.get(idx - 1).copied()
    }

    /// Flush accumulated IR at a synchronization point: encode it with the shared contract, then
    /// EXECUTE it on the embedded software backend — buffers, uploads, and the compute `Dispatch`
    /// (which runs the PTX kernel on the CPU interpreter) all take effect, mutating the device buffers
    /// held in [`backend`](CudaState::backend). The buffer bytes persist across flushes (they are only
    /// dropped on `cuMemFree`), so a later `cuMemcpyDtoH` reads the real, kernel-produced results.
    ///
    /// When `$DD_GPU_EXEC` is set the SAME bytes are also shipped to the host GPU-exec service (the
    /// real Apple-silicon deployment path; best-effort). Then the frame is cleared.
    pub fn flush(&mut self) {
        if self.frame.is_empty() {
            return;
        }
        let bytes = self.frame.finish();
        let ncmds = self.frame.cmds().len();
        // Execute in-process: replay the frame into the CPU backend. The decoder + executor are the
        // host's own `dd_gpu` code (no second implementation), so what runs here is byte-for-byte what
        // a host executor would run — the anti-drift guarantee, now covering execution, not just shape.
        if let Err(e) = replay_stream(&mut self.backend, &bytes) {
            if std::env::var_os("DD_SHIM_DEBUG").is_some() {
                eprintln!("[dd-shim-cuda] flush: embedded backend replay error ({ncmds} cmds): {e}");
            }
        }
        if std::env::var_os("DD_GPU_EXEC").is_some() {
            let conn = self.conn.get_or_insert_with(ExecConn::from_env);
            // Compute has no present surface; a synthetic zero surface carries the IR-length header.
            let surf = Surface { id: 0, width: 0, height: 0, stride: 0, fd: -1 };
            let _ = conn.submit(&surf, &bytes);
        } else if std::env::var_os("DD_SHIM_DEBUG").is_some() {
            eprintln!(
                "[dd-shim-cuda] flush: executed {} IR bytes ({ncmds} cmds) on the embedded software \
                 backend (dispatches so far: {})",
                bytes.len(),
                self.backend.dispatches
            );
        }
        self.frame.clear();
    }

    /// The `cuMemcpyDtoH` engine: flush pending work so any launched kernel has actually run, then read
    /// `out.len()` bytes of device memory starting at device pointer `src` out of the backend into
    /// `out`. Returns `false` for a dangling device pointer or an out-of-range read (→
    /// `CUDA_ERROR_INVALID_VALUE`). This is the readback the scaffold deferred — now real.
    pub fn read_device(&mut self, src: DevicePtr, out: &mut [u8]) -> bool {
        self.flush();
        let Some((buf, off)) = self.ctx.resolve(src) else {
            return false;
        };
        self.backend.read_buffer(buf, off, out).is_ok()
    }
}

static STATE: OnceLock<Mutex<CudaState>> = OnceLock::new();

/// Run `f` with exclusive access to the global shim state. Non-reentrant — never call [`with`] from
/// inside an `f` (the `Mutex` is not recursive); the entry points sequence their `with` calls instead.
pub fn with<R>(f: impl FnOnce(&mut CudaState) -> R) -> R {
    let m = STATE.get_or_init(|| Mutex::new(CudaState::new()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// Reset the global state (test-only, so the anti-drift test starts from a clean frame).
#[cfg(test)]
pub fn reset() {
    with(|s| *s = CudaState::new());
}
