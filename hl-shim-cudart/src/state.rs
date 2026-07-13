//! The shim's global compute state (device / IR frame / embedded executor / handle tables) plus the
//! cudart-owned bits the driver has no analogue for (thread-local last-error, current-device, and the
//! `<<<>>>` call-config stack, and the process-global fatbin / function registries the nvcc glue fills).
//!
//! dd-shim-cudart is a PEER of dd-shim-cuda over the SAME shared `dd-gpu` core: the heavy lifting — the
//! CUDA→dd-gpu-IR translation — is delegated to [`hl_gpu::cuda::CudaContext`] (memory alloc/copy → IR,
//! PTX module load, kernel launch → compute pipeline + dispatch), and the accumulated IR is EXECUTED
//! in-process on an embedded [`hl_gpu::software::SoftwareBackend`] (the CPU PTX interpreter — the same
//! executor the oracle uses), so a runtime vecadd runs end-to-end and reads back correct results with no
//! GPU. We do NOT redefine that mapping; the shim owns device-presence values, C-ABI handle tables, the
//! frame accumulator, and the cudart error/last-error/registration surface, forwarding compute into the
//! `CudaContext`. This mirrors dd-shim-cuda's `state.rs` exactly (the runtime API lowers to the same IR).

use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use hl_gpu::backend::GpuBackend;
use hl_gpu::cuda::{CudaContext, CudaDeviceDesc, DevicePtr, Function};
use hl_gpu::replay::replay_stream;
use hl_gpu::software::SoftwareBackend;
use hl_shim::transport::{ExecConn, FrameBuilder, Surface};

use crate::result::*;

// ==================================================================================================
// cudart-owned thread-local state (last-error / current-device / <<<>>> call-config stack)
// ==================================================================================================

thread_local! {
    static LAST_ERROR: Cell<i32> = const { Cell::new(CUDA_SUCCESS_RT) };
    static DEVICE: Cell<i32> = const { Cell::new(0) };
    static CFG_STACK: RefCell<Vec<CallCfg>> = const { RefCell::new(Vec::new()) };
}

/// One pushed `<<<grid,block,shmem,stream>>>` launch configuration.
#[derive(Clone, Copy)]
pub struct CallCfg {
    pub grid: [u32; 3],
    pub block: [u32; 3],
    pub shmem: usize,
    pub stream: *mut c_void,
}

/// Record + return: a nonzero result becomes the sticky thread-local last error (drained by
/// `cudaGetLastError`). Mirrors the C oracle's `rec()`.
pub fn rec(e: i32) -> i32 {
    if e != CUDA_SUCCESS_RT {
        LAST_ERROR.with(|c| c.set(e));
    }
    e
}
pub fn take_last_error() -> i32 {
    LAST_ERROR.with(|c| {
        let e = c.get();
        c.set(CUDA_SUCCESS_RT);
        e
    })
}
pub fn peek_last_error() -> i32 {
    LAST_ERROR.with(|c| c.get())
}
pub fn current_device() -> i32 {
    DEVICE.with(|c| c.get())
}
pub fn set_current_device(d: i32) {
    DEVICE.with(|c| c.set(d));
}
/// Push a `<<<>>>` config; false if the stack is full (the C oracle returns nonzero → skip the launch).
pub fn push_call_config(cfg: CallCfg) -> bool {
    const CFG_MAX: usize = 32;
    CFG_STACK.with(|s| {
        let mut v = s.borrow_mut();
        if v.len() >= CFG_MAX {
            return false;
        }
        v.push(cfg);
        true
    })
}
/// Pop a `<<<>>>` config (`None` if empty → `cudaErrorInvalidConfiguration`).
pub fn pop_call_config() -> Option<CallCfg> {
    CFG_STACK.with(|s| s.borrow_mut().pop())
}

// ==================================================================================================
// process-global fatbin + function registries (the nvcc registration glue)
// ==================================================================================================

/// One registered fatbin: the `fatCubin` nvcc handed us + the lazily-loaded PTX module.
pub struct FatBin {
    /// Raw host pointer to the `__fatBinC_Wrapper_t` / container (only ever handed to the fatbin
    /// walker). Stored as `usize` so the registry stays `Send`.
    pub fatcubin: usize,
    pub module: u32, // driver module id (0 = not yet loaded)
    pub loaded: bool,
    pub load_res: i32, // the CUresult of the lazy cuModuleLoadData
    pub live: bool,    // cleared by __cudaUnregisterFatBinary
}

/// A registered kernel: host-stub pointer → (fatbin, device kernel name). The device function is
/// resolved from the loaded module at launch time (a cheap name lookup), so no per-func cache is kept
/// here (which also keeps the registry `Send`, i.e. free of raw pointers).
pub struct FuncReg {
    pub host_fun: usize,
    pub fat: usize, // 1-based index into `fatbins` (0 = none / unregistered)
    pub name: String,
}

#[derive(Default)]
pub struct Registry {
    pub fatbins: Vec<FatBin>,
    pub funcs: Vec<FuncReg>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

/// Run `f` with exclusive access to the fatbin/function registry.
pub fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let m = REGISTRY.get_or_init(|| Mutex::new(Registry::default()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

// ==================================================================================================
// the shared compute state (device model / IR frame / embedded executor / handle tables)
// ==================================================================================================

/// Allocation kinds tracked in the registry (mirrors the oracle's `ALLOC_*`).
pub const ALLOC_DEVICE: u8 = 0;
pub const ALLOC_HOST: u8 = 1;
pub const ALLOC_MANAGED: u8 = 2;

/// One entry in the allocation registry: the range `[base, base+size)` and its kind.
#[derive(Clone, Copy)]
pub struct AllocMeta {
    pub base: u64,
    pub size: u64,
    pub kind: u8,
}

/// Per-event record state so `cuEventElapsedTime`/`cudaEventElapsedTime` is truthful.
#[derive(Clone, Copy)]
struct EventState {
    recorded: bool,
    at: Option<std::time::Instant>,
}

/// Everything the shim tracks between calls (the compute core).
pub struct CudartState {
    /// `cuInit`/`ensure_init` has run.
    pub inited: bool,
    /// The CUDA→dd-gpu-IR translator + simulated device (from the shared `dd-gpu` crate).
    pub ctx: CudaContext,
    /// The dd-gpu IR accumulated for the current stream; flushed on synchronize.
    pub frame: FrameBuilder,
    /// The embedded host executor: a real CPU backend that runs the accumulated IR — including the
    /// compute `Dispatch` (the PTX interpreter in `hl_gpu::ptx`) — and holds the resulting
    /// device-buffer bytes for `cudaMemcpy(...DeviceToHost)` readback. On a real Apple-silicon host the
    /// same IR is shipped over `$DD_GPU_EXEC` to the host Metal executor instead.
    pub backend: SoftwareBackend,
    /// `CUfunction` handle table: an opaque handle is `index + 1` (never null).
    functions: Vec<Function>,
    /// Allocation registry keyed by base pointer, for `cudaMemGetInfo` / metadata.
    allocs_meta: HashMap<u64, AllocMeta>,
    /// Running device+managed bytes outstanding (for `cudaMemGetInfo`).
    pub bytes_outstanding: u64,
    /// Host allocations (`cudaMallocHost`/`cudaHostAlloc`): base ptr → (raw ptr, len) so
    /// `cudaFreeHost` can reclaim the real host memory it handed out.
    host_allocs: HashMap<usize, (usize, usize)>,
    /// `CUstream` token allocator (opaque, non-null). The executor is synchronous, so a stream is a
    /// scheduling token; ordering is preserved by the single accumulated frame.
    pub next_stream: usize,
    /// `CUevent` token allocator (opaque, non-null).
    pub next_event: usize,
    /// Per-event record state, for `cudaEventQuery`/`cudaEventElapsedTime`.
    events: HashMap<usize, EventState>,
    /// Lazily-opened transport to the host GPU-exec service (only used when `$DD_GPU_EXEC` is set).
    conn: Option<ExecConn>,
}

impl CudartState {
    fn new() -> Self {
        // VRAM the simulated device advertises; override with `$DD_CUDA_VRAM_BYTES`. Default 8 GiB.
        let vram = std::env::var("DD_CUDA_VRAM_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(8u64 << 30);
        CudartState {
            inited: false,
            ctx: CudaContext::new(CudaDeviceDesc::apple_default(vram)),
            frame: FrameBuilder::new(),
            backend: SoftwareBackend::new(),
            functions: Vec::new(),
            allocs_meta: HashMap::new(),
            bytes_outstanding: 0,
            host_allocs: HashMap::new(),
            next_stream: 1,
            next_event: 1,
            events: HashMap::new(),
            conn: None,
        }
    }

    // ---- function handle table ------------------------------------------------------------------

    /// Register a resolved function, returning its opaque `CUfunction`-style handle (`index+1`).
    pub fn intern_function(&mut self, f: Function) -> *mut c_void {
        self.functions.push(f);
        self.functions.len() as *mut c_void
    }
    /// Resolve a function handle back to its [`Function`]. `None` for null / out-of-range.
    pub fn function(&self, h: *mut c_void) -> Option<Function> {
        let idx = h as usize;
        if idx == 0 || idx > self.functions.len() {
            return None;
        }
        Some(self.functions[idx - 1])
    }

    // ---- allocation registry --------------------------------------------------------------------

    pub fn register_alloc(&mut self, base: u64, size: u64, kind: u8) {
        if kind == ALLOC_DEVICE || kind == ALLOC_MANAGED {
            self.bytes_outstanding = self.bytes_outstanding.saturating_add(size);
        }
        self.allocs_meta.insert(base, AllocMeta { base, size, kind });
    }
    pub fn unregister_alloc(&mut self, base: u64) {
        if let Some(a) = self.allocs_meta.remove(&base) {
            if a.kind == ALLOC_DEVICE || a.kind == ALLOC_MANAGED {
                self.bytes_outstanding = self.bytes_outstanding.saturating_sub(a.size);
            }
        }
    }

    // ---- host allocations -----------------------------------------------------------------------

    pub fn host_alloc(&mut self, size: usize, kind: u8) -> *mut c_void {
        let n = size.max(1);
        let mut v = vec![0u8; n];
        let ptr = v.as_mut_ptr() as usize;
        std::mem::forget(v); // ownership moves into `host_allocs`; reclaimed in host_free
        self.host_allocs.insert(ptr, (ptr, n));
        self.register_alloc(ptr as u64, size as u64, kind);
        ptr as *mut c_void
    }
    pub fn host_free(&mut self, p: *mut c_void) {
        let key = p as usize;
        if let Some((ptr, len)) = self.host_allocs.remove(&key) {
            self.unregister_alloc(ptr as u64);
            // SAFETY: `ptr`/`len` came from a `Vec<u8>` we `forget`; reconstruct it to drop.
            unsafe { drop(Vec::from_raw_parts(ptr as *mut u8, len, len)) };
        }
    }

    // ---- streams / events -----------------------------------------------------------------------

    pub fn register_event(&mut self, token: usize) {
        self.events.insert(token, EventState { recorded: false, at: None });
    }
    pub fn record_event(&mut self, token: usize) {
        self.events.insert(token, EventState { recorded: true, at: Some(std::time::Instant::now()) });
    }
    pub fn event_recorded(&self, token: usize) -> bool {
        self.events.get(&token).map(|e| e.recorded).unwrap_or(false)
    }
    pub fn unregister_event(&mut self, token: usize) {
        self.events.remove(&token);
    }
    pub fn event_elapsed_ms(&self, start: usize, end: usize) -> Option<f32> {
        let a = self.events.get(&start)?.at?;
        let b = self.events.get(&end)?.at?;
        Some(b.saturating_duration_since(a).as_secs_f64() as f32 * 1.0e3)
    }

    // ---- device-memory fills / copies (through the shared IR + embedded backend) ----------------

    /// `cudaMemset`: write `pattern` (already expanded) into the device buffer at `dst`. Returns
    /// `false` for a dangling device pointer.
    pub fn memset(&mut self, dst: DevicePtr, pattern: &[u8]) -> bool {
        match self.ctx.memcpy_htod(dst, pattern) {
            Some(cmd) => {
                self.frame.push(cmd);
                true
            }
            None => false,
        }
    }

    /// `cudaMemcpy(...DeviceToDevice)` / default: flush so any pending kernel has run, read `n` bytes
    /// from `src`, then write them into `dst`. Returns `false` for a dangling pointer.
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
    /// EXECUTE it on the embedded software backend (buffers, uploads, and the compute `Dispatch` all
    /// take effect). The buffer bytes persist across flushes, so a later `cudaMemcpy(...DeviceToHost)`
    /// reads the real, kernel-produced results. When `$DD_GPU_EXEC` is set the SAME bytes ship to the
    /// host GPU-exec service (best-effort). Then the frame is cleared.
    pub fn flush(&mut self) {
        if self.frame.is_empty() {
            return;
        }
        let bytes = self.frame.finish();
        let ncmds = self.frame.cmds().len();
        if let Err(e) = replay_stream(&mut self.backend, &bytes) {
            if std::env::var_os("DD_SHIM_DEBUG").is_some() {
                eprintln!("[dd-shim-cudart] flush: embedded backend replay error ({ncmds} cmds): {e}");
            }
        }
        if std::env::var_os("DD_GPU_EXEC").is_some() {
            let conn = self.conn.get_or_insert_with(ExecConn::from_env);
            let surf = Surface { id: 0, width: 0, height: 0, stride: 0, fd: -1, generation: 0 };
            let _ = conn.submit(&surf, &bytes);
        } else if std::env::var_os("DD_SHIM_DEBUG").is_some() {
            eprintln!(
                "[dd-shim-cudart] flush: executed {} IR bytes ({ncmds} cmds) on the embedded software \
                 backend (dispatches so far: {})",
                bytes.len(),
                self.backend.dispatches
            );
        }
        self.frame.clear();
    }

    /// The `cudaMemcpy(...DeviceToHost)` engine: flush pending work so any launched kernel has run, then
    /// read `out.len()` bytes of device memory at `src` out of the backend into `out`. Returns `false`
    /// for a dangling device pointer or an out-of-range read.
    pub fn read_device(&mut self, src: DevicePtr, out: &mut [u8]) -> bool {
        self.flush();
        let Some((buf, off)) = self.ctx.resolve(src) else {
            return false;
        };
        self.backend.read_buffer(buf, off, out).is_ok()
    }
}

static STATE: OnceLock<Mutex<CudartState>> = OnceLock::new();

/// Run `f` with exclusive access to the global compute state. Non-reentrant.
pub fn with<R>(f: impl FnOnce(&mut CudartState) -> R) -> R {
    let m = STATE.get_or_init(|| Mutex::new(CudartState::new()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// Lazy runtime init: mark the simulated device initialized. (The compute state is created lazily on
/// first `with`; there is no separate driver process to `cuInit`.) Mirrors the oracle's `ensure_init`.
pub fn ensure_init() -> i32 {
    with(|s| s.inited = true);
    CUDA_SUCCESS_RT
}
