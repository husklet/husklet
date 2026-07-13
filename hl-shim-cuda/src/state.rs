//! The shim's global device/context state + the accumulated dd-gpu IR frame.
//!
//! The `cu*` entry points are free `extern "C"` functions, so their shared mutable state lives behind
//! a process-global `Mutex` (like dd-shim-gl's state, adapted to the single-simulated-device CUDA
//! model). The heavy lifting — the CUDA→dd-gpu-IR translation — is delegated to
//! [`hl_gpu::cuda::CudaContext`], which is the shared, host-authored mapping (memory alloc/copy → IR,
//! PTX module load, kernel launch → compute pipeline + dispatch). We do NOT redefine that mapping; the
//! shim just owns the device-presence values, the C-ABI handle tables, and the frame accumulator, and
//! forwards work into the `CudaContext`.

use core::ffi::c_void;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{Mutex, OnceLock};

use hl_gpu::backend::GpuBackend;
use hl_gpu::cuda::{CudaContext, CudaDeviceDesc, DevicePtr, Function};
use hl_gpu::replay::replay_stream;
use hl_gpu::software::SoftwareBackend;
use hl_shim::transport::{ExecConn, FrameBuilder, Surface};

/// Allocation kinds tracked in the registry (mirrors `cuda_shim.c`'s `ALLOC_*`), so the metadata
/// queries — `cuMemGetInfo`, `cuMemGetAddressRange`, `cuPointerGetAttribute(s)` — answer truthfully.
pub const ALLOC_DEVICE: u8 = 0;
pub const ALLOC_HOST: u8 = 1;
pub const ALLOC_MANAGED: u8 = 2;
pub const ALLOC_REGISTERED: u8 = 3;

/// One entry in the allocation registry: the range `[base, base+size)` and its kind.
#[derive(Clone, Copy)]
pub struct AllocMeta {
    pub base: u64,
    pub size: u64,
    pub kind: u8,
}

/// Per-event record state: `cuEventRecord` timestamps with the monotonic clock so
/// `cuEventElapsedTime` is truthful (mirrors the C oracle's `clock_gettime`).
#[derive(Clone, Copy)]
struct EventState {
    recorded: bool,
    at: Option<std::time::Instant>,
}

/// Everything the shim tracks between `cu*` calls.
pub struct CudaState {
    /// `cuInit` was called (the driver spec-guards most calls behind this).
    pub inited: bool,
    /// The CUDA→dd-gpu-IR translator + simulated device (from the shared `dd-gpu` crate).
    pub ctx: CudaContext,
    /// The dd-gpu IR accumulated for the current stream; flushed on synchronize.
    pub frame: FrameBuilder,
    /// The embedded host executor: a real CPU backend that runs the accumulated IR — including the
    /// compute `Dispatch` (the PTX kernel interpreter in `hl_gpu::ptx`) — and holds the resulting
    /// device-buffer bytes for `cuMemcpyDtoH` readback. This is the in-process analog of
    /// `hl-gpu/cuda/cuda_shim.c`'s embedded interpreter (the parity oracle): it makes the shim
    /// FUNCTIONAL end-to-end on this host with no GPU. On a real Apple-silicon host the same IR is
    /// shipped over `$DD_GPU_EXEC` to the host Metal executor instead (see docs).
    pub backend: SoftwareBackend,
    /// `CUfunction` handle table: an opaque handle is `index + 1` (never null).
    functions: Vec<Function>,
    /// Parallel to `functions`: the entry name (for `cuFuncGetName`) and per-function dynamic-shared
    /// bytes (for `cuFuncGetAttribute`/`cuFuncSetAttribute` parity).
    func_names: Vec<CString>,
    func_dyn_shared: Vec<i32>,
    /// `CUcontext` token allocator (opaque, non-null). One simulated device, so contexts are tokens.
    pub next_ctx: usize,
    /// The current context token (`0` = none).
    pub current_ctx: usize,
    /// The `cuCtxPushCurrent`/`cuCtxPopCurrent` context stack (holds the *previous* current tokens).
    pub ctx_stack: Vec<usize>,
    /// Per-context flags (`cuCtxGetFlags`/`cuCtxSetFlags`, set at create). Keyed by context token.
    ctx_flags: HashMap<usize, u32>,
    /// Device-0 primary context (`cuDevicePrimaryCtxRetain/Release/Reset`): token (`0` = none),
    /// reference count, and flags.
    pub primary_ctx: usize,
    pub primary_refcount: u32,
    pub primary_flags: u32,
    /// Context-scoped resource limits (`cuCtxGetLimit`/`cuCtxSetLimit`), indexed by `CUlimit`.
    pub limits: [usize; 7],
    /// Context cache config (`cuCtxGetCacheConfig`/`SetCacheConfig`) + shared-memory bank config.
    pub cache_config: i32,
    pub shared_config: i32,
    /// Allocation registry keyed by base pointer (device/host/managed/registered ranges) for the
    /// metadata queries. Mirrors the C oracle's `g_allocs`.
    allocs_meta: HashMap<u64, AllocMeta>,
    /// Running device+managed bytes outstanding (for `cuMemGetInfo`).
    pub bytes_outstanding: u64,
    /// Host allocations (`cuMemAllocHost`/`cuMemHostAlloc`): base ptr → (raw ptr, len) so
    /// `cuMemFreeHost` can reclaim the real host memory it handed out.
    host_allocs: HashMap<usize, (usize, usize)>,
    /// `CUstream` token allocator (opaque, non-null). The executor is synchronous, so a stream is just
    /// a scheduling token; ordering is preserved by the single accumulated frame.
    pub next_stream: usize,
    /// Per-stream (flags, priority), for `cuStreamGetFlags`/`cuStreamGetPriority`.
    streams: HashMap<usize, (u32, i32)>,
    /// `CUevent` token allocator (opaque, non-null). Events record/synchronize by flushing.
    pub next_event: usize,
    /// Per-event record state, for `cuEventQuery`/`cuEventElapsedTime`.
    events: HashMap<usize, EventState>,
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
            func_names: Vec::new(),
            func_dyn_shared: Vec::new(),
            next_ctx: 1,
            current_ctx: 0,
            ctx_stack: Vec::new(),
            ctx_flags: HashMap::new(),
            primary_ctx: 0,
            primary_refcount: 0,
            primary_flags: 0,
            // matches cuda_shim.c g_limits: stack/printf/malloc-heap/rt-sync-depth/rt-pending/l2-gran.
            limits: [1024, 1024 * 1024, 8 * 1024 * 1024, 2, 2048, 128, 0],
            cache_config: 0,
            shared_config: 0,
            allocs_meta: HashMap::new(),
            bytes_outstanding: 0,
            host_allocs: HashMap::new(),
            next_stream: 1,
            streams: HashMap::new(),
            next_event: 1,
            events: HashMap::new(),
            conn: None,
        }
    }

    /// Register a resolved function + its entry name, returning its opaque `CUfunction` handle
    /// (`index + 1`, non-null).
    pub fn intern_function(&mut self, f: Function, name: &str) -> *mut c_void {
        self.functions.push(f);
        self.func_names.push(CString::new(name).unwrap_or_default());
        self.func_dyn_shared.push(0);
        self.functions.len() as *mut c_void // len == index+1
    }

    /// Resolve a `CUfunction` handle back to its [`Function`]. `None` for null / out-of-range.
    pub fn function(&self, h: *mut c_void) -> Option<Function> {
        self.func_index(h).map(|i| self.functions[i])
    }

    /// Index into the parallel function tables for a `CUfunction` handle (`None` for null/out-of-range).
    fn func_index(&self, h: *mut c_void) -> Option<usize> {
        let idx = h as usize;
        if idx == 0 || idx > self.functions.len() {
            return None;
        }
        Some(idx - 1)
    }

    /// The interned entry-name pointer for `cuFuncGetName` (stable for the process lifetime).
    pub fn func_name_ptr(&self, h: *mut c_void) -> Option<*const core::ffi::c_char> {
        self.func_index(h).map(|i| self.func_names[i].as_ptr())
    }

    /// Per-function dynamic-shared bytes (`cuFuncGetAttribute`); `None` for a bad handle.
    pub fn func_dyn_shared(&self, h: *mut c_void) -> Option<i32> {
        self.func_index(h).map(|i| self.func_dyn_shared[i])
    }

    /// Set per-function dynamic-shared bytes (`cuFuncSetAttribute`); `false` for a bad handle.
    pub fn set_func_dyn_shared(&mut self, h: *mut c_void, v: i32) -> bool {
        match self.func_index(h) {
            Some(i) => {
                self.func_dyn_shared[i] = v;
                true
            }
            None => false,
        }
    }

    // ---- context flags -------------------------------------------------------------------------

    /// Record a freshly-created context's flags.
    pub fn set_ctx_flags(&mut self, token: usize, flags: u32) {
        self.ctx_flags.insert(token, flags);
    }

    /// The current context's flags (`0` if none / untracked).
    pub fn current_ctx_flags(&self) -> u32 {
        self.ctx_flags.get(&self.current_ctx).copied().unwrap_or(0)
    }

    /// Set the current context's flags (`cuCtxSetFlags`).
    pub fn set_current_ctx_flags(&mut self, flags: u32) {
        if self.current_ctx != 0 {
            self.ctx_flags.insert(self.current_ctx, flags);
        }
    }

    // ---- allocation registry -------------------------------------------------------------------

    /// Register an allocation range so the metadata queries answer truthfully.
    pub fn register_alloc(&mut self, base: u64, size: u64, kind: u8) {
        if kind == ALLOC_DEVICE || kind == ALLOC_MANAGED {
            self.bytes_outstanding = self.bytes_outstanding.saturating_add(size);
        }
        self.allocs_meta.insert(base, AllocMeta { base, size, kind });
    }

    /// Drop an allocation range from the registry (idempotent).
    pub fn unregister_alloc(&mut self, base: u64) {
        if let Some(a) = self.allocs_meta.remove(&base) {
            if a.kind == ALLOC_DEVICE || a.kind == ALLOC_MANAGED {
                self.bytes_outstanding = self.bytes_outstanding.saturating_sub(a.size);
            }
        }
    }

    /// Find the registry entry whose `[base, base+size)` contains `ptr`.
    pub fn find_alloc(&self, ptr: u64) -> Option<AllocMeta> {
        self.allocs_meta
            .values()
            .find(|a| ptr >= a.base && ptr < a.base + a.size.max(1))
            .copied()
    }

    /// `true` iff `ptr` is exactly the base of a live registry entry (for register/unregister guards).
    pub fn alloc_is_base(&self, ptr: u64) -> bool {
        self.allocs_meta.contains_key(&ptr)
    }

    // ---- host allocations ----------------------------------------------------------------------

    /// Allocate real host memory of `size` bytes, returning its pointer; tracked so `cuMemFreeHost`
    /// reclaims it. Also registers it in the metadata registry with `kind`.
    pub fn host_alloc(&mut self, size: usize, kind: u8) -> *mut c_void {
        let n = size.max(1);
        let mut v = vec![0u8; n];
        let ptr = v.as_mut_ptr() as usize;
        std::mem::forget(v); // ownership moves into `host_allocs`; reclaimed in host_free
        self.host_allocs.insert(ptr, (ptr, n));
        self.register_alloc(ptr as u64, size as u64, kind);
        ptr as *mut c_void
    }

    /// Free a host allocation previously returned by [`host_alloc`](Self::host_alloc).
    pub fn host_free(&mut self, p: *mut c_void) {
        let key = p as usize;
        if let Some((ptr, len)) = self.host_allocs.remove(&key) {
            self.unregister_alloc(ptr as u64);
            // SAFETY: `ptr`/`len` came from a `Vec<u8>` we `forget`; reconstruct it to drop.
            unsafe {
                drop(Vec::from_raw_parts(ptr as *mut u8, len, len));
            }
        }
    }

    // ---- streams / events ----------------------------------------------------------------------

    pub fn register_stream(&mut self, token: usize, flags: u32, priority: i32) {
        self.streams.insert(token, (flags, priority));
    }
    pub fn stream_flags(&self, token: usize) -> u32 {
        self.streams.get(&token).map(|s| s.0).unwrap_or(0)
    }
    pub fn stream_priority(&self, token: usize) -> i32 {
        self.streams.get(&token).map(|s| s.1).unwrap_or(0)
    }
    pub fn unregister_stream(&mut self, token: usize) {
        self.streams.remove(&token);
    }

    pub fn register_event(&mut self, token: usize) {
        self.events.insert(token, EventState { recorded: false, at: None });
    }
    /// Timestamp an event as recorded now (`cuEventRecord`).
    pub fn record_event(&mut self, token: usize) {
        self.events.insert(token, EventState { recorded: true, at: Some(std::time::Instant::now()) });
    }
    /// `true` iff the event has been recorded (`cuEventQuery`).
    pub fn event_recorded(&self, token: usize) -> bool {
        self.events.get(&token).map(|e| e.recorded).unwrap_or(false)
    }
    pub fn unregister_event(&mut self, token: usize) {
        self.events.remove(&token);
    }
    /// Elapsed milliseconds between two recorded events (`cuEventElapsedTime`); `None` if either is
    /// unrecorded.
    pub fn event_elapsed_ms(&self, start: usize, end: usize) -> Option<f32> {
        let a = self.events.get(&start)?.at?;
        let b = self.events.get(&end)?.at?;
        Some(b.saturating_duration_since(a).as_secs_f64() as f32 * 1.0e3)
    }

    // ---- device-memory fills / copies (through the shared IR + embedded backend) ---------------

    /// `cuMemset*`: write `pattern` (already expanded to the full byte run) into the device buffer at
    /// `dst`. Returns `false` for a dangling device pointer.
    pub fn memset(&mut self, dst: DevicePtr, pattern: &[u8]) -> bool {
        match self.ctx.memcpy_htod(dst, pattern) {
            Some(cmd) => {
                self.frame.push(cmd);
                true
            }
            None => false,
        }
    }

    /// `cuMemcpyDtoD` / `cuMemcpy` (device→device): flush so any pending kernel has run, read `n`
    /// bytes from `src`, then write them into `dst`. Returns `false` for a dangling pointer.
    pub fn copy_dtod(&mut self, dst: DevicePtr, src: DevicePtr, n: usize) -> bool {
        let Some((sbuf, soff)) = self.ctx.resolve(src) else { return false };
        if self.ctx.resolve(dst).is_none() {
            return false;
        }
        self.flush();
        let mut tmp = vec![0u8; n];
        if self.backend.read_buffer(sbuf, soff, &mut tmp).is_err() {
            return false;
        }
        match self.ctx.memcpy_htod(dst, &tmp) {
            Some(cmd) => {
                self.frame.push(cmd);
                true
            }
            None => false,
        }
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
            let surf = Surface { id: 0, width: 0, height: 0, stride: 0, fd: -1, generation: 0 };
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
