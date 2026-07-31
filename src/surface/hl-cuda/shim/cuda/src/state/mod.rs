//! The shim's process-global device/context state + the guest→host command sink.
//!
//! The `cu*` entry points are free `extern "C"` functions, so their shared mutable state lives behind a
//! process-global `Mutex`. The heavy lifting — the CUDA→hl-GPU-IR lowering — is delegated to the
//! `hl_cuda` service layer (`allocate`/`transfer`/`load_module`/`launch`/`synchronize`), which mutates a
//! [`CudaContext`] and submits protocol `Cmd`s through a [`hl_gpu::RemoteCommandSink`]. That sink is the
//! single boundary to the host GPU-exec service, connected lazily from `$HL_GPU_EXEC` on first submit.
//!
//! This module owns only the C-ABI marshalling state the driver API needs: the opaque handle tables for
//! `CUmodule` / `CUfunction` / `CUstream` / `CUevent` ([`handle`]) and the `CUcontext` token bookkeeping
//! ([`context`]). The compute semantics are NOT redefined here — they are the shared `hl_cuda` services,
//! and object LIFETIME is the `hl_cuda` model's `StreamTable`/`EventTable`, which the handle tables
//! resolve through so a destroyed handle can never be mistaken for a live one.

use core::ffi::c_void;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use hl_cuda::model::event::Event;
use hl_cuda::model::stream::Stream;
use hl_cuda::result::{CUDA_ERROR_INVALID_CONTEXT, CUDA_ERROR_NOT_INITIALIZED};
use hl_cuda::{CudaContext, CudaDeviceDesc, Function};
use hl_gpu::transport::DEFAULT_EXEC_SOCK;
use hl_gpu::RemoteCommandSink;

mod context;
mod handle;

pub use handle::{CU_STREAM_LEGACY, CU_STREAM_PER_THREAD};

/// Everything the shim tracks between `cu*` calls.
pub struct State {
    /// `cuInit` was called. Every entry point that lowers IR is gated on it ([`State::require_init`]), which
    /// is also how a `fork(2)` child is refused: the child's fresh state has never been initialized.
    pub inited: bool,
    /// The pid that owns this state. A mismatch means the state was inherited across `fork(2)`.
    pid: u32,
    /// The CUDA object model + lowering target (device desc, allocation/module/stream tables).
    pub ctx: CudaContext,
    /// The guest→host boundary: encodes each lowered batch and ships it framed over `$HL_GPU_EXEC`.
    pub sink: RemoteCommandSink,

    /// `CUfunction` table: an opaque handle is `index + 1`. Holds the resolved [`Function`] + entry name.
    functions: Vec<(Function, CString)>,
    /// Parallel to `functions`: the per-function preferred cache config recorded by
    /// `cuFuncSetCacheConfig` (a hint the synchronous executor honors as a no-op, but reports faithfully).
    func_cache_config: Vec<i32>,
    /// `CUmodule` table: an opaque handle is `index + 1`, storing the `hl_cuda` module id.
    modules: Vec<u32>,
    /// `CUstream` token table: a created handle is `index + STREAM_TOKEN_BASE`, storing the `hl_cuda`
    /// [`Stream`] whose liveness `ctx.streams` owns. Append-only — a token is never recycled.
    streams: Vec<Stream>,
    /// Parallel to `streams`: the `(flags, priority)` a stream was created with, for
    /// `cuStreamGetFlags`/`cuStreamGetPriority`.
    stream_meta: Vec<(u32, i32)>,
    /// `CUevent` token table: a handle is `index + 1`, storing the `hl_cuda` [`Event`] whose liveness
    /// `ctx.events` owns. Append-only — a token is never recycled.
    events: Vec<Event>,
    /// Parallel to `events`: the `cuEventRecord` timestamp (`None` until recorded) for
    /// `cuEventElapsedTime`. The model tracks *whether* an event is recorded; the wall clock is C-ABI
    /// state only this shim needs.
    event_times: Vec<Option<Instant>>,

    /// `CUcontext` token allocator (opaque, non-null). One simulated device, so contexts are tokens.
    next_ctx: usize,
    /// The live `CUcontext` tokens. `cuCtxDestroy` removes one; every entry point handed a token checks
    /// membership so a destroyed token is `CUDA_ERROR_INVALID_CONTEXT`, not a silent success. Destroying
    /// the CURRENT context also clears [`State::current_ctx`], which is what [`State::require_context`]
    /// consults on behalf of every entry point that does not take a token.
    contexts: HashSet<usize>,
    /// The current context token (`0` = none).
    current_ctx: usize,
    /// The `cuCtxPushCurrent`/`cuCtxPopCurrent` stack (holds the *previous* current tokens).
    ctx_stack: Vec<usize>,
    /// Per-context creation flags (`cuCtxGetFlags`/`cuCtxSetFlags`), keyed by context token.
    ctx_flags: HashMap<usize, u32>,
    /// Device-0 primary context (`cuDevicePrimaryCtxRetain`/`Release`/`Reset`): token (`0` = none),
    /// reference count, and flags. A single simulated device has exactly one primary context.
    primary_ctx: usize,
    primary_refcount: u32,
    primary_flags: u32,

    /// Context-scoped resource limits (`cuCtxGetLimit`/`cuCtxSetLimit`), indexed by `CUlimit`. The
    /// defaults match a real driver's stack/printf-fifo/malloc-heap/rt-sync-depth/rt-pending/l2-gran.
    limits: [usize; hl_cuda::result::CU_LIMIT_MAX as usize],
    /// Context preferred cache config (`cuCtxGetCacheConfig`/`cuCtxSetCacheConfig`).
    cache_config: i32,
    /// Context shared-memory bank config (`cuCtxGetSharedMemConfig`/`cuCtxSetSharedMemConfig`); defaults
    /// to `CU_SHARED_MEM_CONFIG_DEFAULT_BANK_SIZE` (0).
    shared_config: i32,
}

impl State {
    fn new() -> Self {
        // VRAM the simulated device advertises; override with `$HL_CUDA_VRAM_BYTES`. Default 8 GiB.
        let vram = std::env::var("HL_CUDA_VRAM_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(8u64 << 30);
        // The launcher-configured identity (`HL_CUDA_NAME` / `HL_CUDA_CC`), applied so the driver API
        // reports the SAME device as `libnvidia-ml` and `libcudart`.
        let mut device = CudaDeviceDesc::apple_default(vram);
        device.configure(
            std::env::var("HL_CUDA_NAME").ok().as_deref(),
            std::env::var("HL_CUDA_CC").ok().as_deref(),
        );
        State {
            inited: false,
            pid: std::process::id(),
            ctx: CudaContext::new(device),
            // Connect target from $HL_GPU_EXEC; the connection itself is opened lazily on first submit.
            sink: RemoteCommandSink::new(
                std::env::var("HL_GPU_EXEC").unwrap_or_else(|_| DEFAULT_EXEC_SOCK.to_owned()),
            ),
            functions: Vec::new(),
            func_cache_config: Vec::new(),
            modules: Vec::new(),
            streams: Vec::new(),
            stream_meta: Vec::new(),
            events: Vec::new(),
            event_times: Vec::new(),
            next_ctx: 1,
            contexts: HashSet::new(),
            current_ctx: 0,
            ctx_stack: Vec::new(),
            ctx_flags: HashMap::new(),
            primary_ctx: 0,
            primary_refcount: 0,
            primary_flags: 0,
            // stack / printf-fifo / malloc-heap / rt-sync-depth / rt-pending-launches / l2-fetch-gran / persisting-l2.
            limits: [1024, 1024 * 1024, 8 * 1024 * 1024, 2, 2048, 128, 0],
            cache_config: 0,
            shared_config: 0,
        }
    }

    /// The gate every IR-emitting entry point checks before touching `ctx`/`sink`. `Err` carries
    /// `CUDA_ERROR_NOT_INITIALIZED`: either `cuInit` was never called, or this state was inherited
    /// across `fork(2)` and disowned (see [`State::disown_after_fork`]).
    pub fn require_init(&self) -> Result<(), i32> {
        if self.inited {
            Ok(())
        } else {
            Err(CUDA_ERROR_NOT_INITIALIZED)
        }
    }

    /// The gate every entry point that touches the CUDA OBJECT MODEL checks — allocations, copies,
    /// memsets, modules, functions, launches, streams, events and the context's own properties. All of
    /// those belong to a context, but the model they live in is one process-global [`CudaContext`] that
    /// outlives any `CUcontext` token, so without asking here a destroyed context would keep answering:
    /// `cuMemAlloc` after `cuCtxDestroy` handed back a pointer and reported success.
    ///
    /// Subsumes [`State::require_init`] — an uninitialized driver can have no current context either, and
    /// `CUDA_ERROR_NOT_INITIALIZED` is the more specific answer, so it is reported first.
    ///
    /// Calls that legitimately predate a context are NOT gated on this: `cuInit`, the driver version and
    /// error strings, device enumeration and device properties, context creation, the current-context
    /// stack (`cuCtxGetCurrent`/`SetCurrent`/`Push`/`Pop`/`Destroy`), the primary-context refcount, and
    /// `cuGetProcAddress`.
    pub fn require_context(&self) -> Result<(), i32> {
        self.require_init()?;
        if self.current_ctx == 0 {
            return Err(CUDA_ERROR_INVALID_CONTEXT);
        }
        Ok(())
    }

    /// CUDA does not inherit a context across `fork(2)`, and using the parent's context in the child is
    /// undefined. The engine implements a guest `fork()` as a real host fork, so without this the child
    /// would hold a copy of the parent's `$HL_GPU_EXEC` fd and believe it owns the parent's buffer ids —
    /// two processes interleaving frames on one socket. Dropping the inherited state closes only the
    /// CHILD's copy of the fd and empties every handle table, and the fresh state is uninitialized, so
    /// every entry point reports `CUDA_ERROR_NOT_INITIALIZED` until the child calls `cuInit` for itself,
    /// then `CUDA_ERROR_INVALID_CONTEXT` until it creates its own context, and only then does an
    /// inherited handle reach the empty handle tables and report `CUDA_ERROR_INVALID_HANDLE`.
    ///
    /// The pid comparison runs on every state access rather than from a `pthread_atfork` handler, so it
    /// also covers `clone(2)`/`vfork` and happens before any fd or table is touched. A guest `execve()`
    /// reloads the image, which reinitializes these statics, and std's `UnixStream` fd is `SOCK_CLOEXEC`,
    /// so exec needs no guard.
    fn disown_after_fork(&mut self) {
        let pid = std::process::id();
        if self.pid != pid {
            *self = State::new();
            self.pid = pid;
        }
    }

    // ---- context limits + cache config ------------------------------------------------------------

    /// `cuCtxGetLimit` — the modeled value of `CUlimit` slot `idx` (caller validates the range).
    pub fn ctx_limit(&self, idx: usize) -> usize {
        self.limits[idx]
    }
    /// `cuCtxSetLimit` — record `CUlimit` slot `idx` (caller validates the range).
    pub fn set_ctx_limit(&mut self, idx: usize, value: usize) {
        self.limits[idx] = value;
    }
    /// `cuCtxGetCacheConfig`.
    pub fn ctx_cache_config(&self) -> i32 {
        self.cache_config
    }
    /// `cuCtxSetCacheConfig`.
    pub fn set_ctx_cache_config(&mut self, c: i32) {
        self.cache_config = c;
    }
    /// `cuCtxGetSharedMemConfig` — the current context's shared-memory bank config.
    pub fn ctx_shared_config(&self) -> i32 {
        self.shared_config
    }
    /// `cuCtxSetSharedMemConfig`.
    pub fn set_ctx_shared_config(&mut self, c: i32) {
        self.shared_config = c;
    }

    // ---- memory info ------------------------------------------------------------------------------

    /// `cuMemGetInfo` — `(free, total)` device bytes: total is the advertised VRAM, free is that minus
    /// the sum of every live allocation (clamped so it never underflows).
    pub fn mem_info(&self) -> (usize, usize) {
        let total = self.ctx.device.total_mem;
        let used = self.ctx.mem.total_bytes().min(total);
        ((total - used) as usize, total as usize)
    }
}

pub struct ShimState;

impl ShimState {
    /// Run `f` with exclusive global shim-state access. This operation is non-reentrant. State inherited
    /// across `fork(2)` is disowned first (see [`State::disown_after_fork`]).
    pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        let state = STATE.get_or_init(|| Mutex::new(State::new()));
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        state.disown_after_fork();
        f(&mut state)
    }

    /// [`ShimState::with`] for an entry point that needs a CURRENT CONTEXT: `f` runs only once
    /// [`State::require_context`] passes, otherwise its `CUresult` is returned without `f` running. Every
    /// such entry point says so exactly once, by calling this instead of `with`.
    pub fn with_context(f: impl FnOnce(&mut State) -> i32) -> i32 {
        Self::with(|s| match s.require_context() {
            Ok(()) => f(s),
            Err(code) => code,
        })
    }
}

/// Reset the process-global state to a clean slate (test-only, so a unit test starts deterministic).
#[cfg(test)]
pub fn reset() {
    ShimState::with(|state| *state = State::new());
}
