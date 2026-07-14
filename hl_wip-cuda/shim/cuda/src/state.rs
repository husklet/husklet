//! The shim's process-global device/context state + the guest→host command sink.
//!
//! The `cu*` entry points are free `extern "C"` functions, so their shared mutable state lives behind a
//! process-global `Mutex`. The heavy lifting — the CUDA→hl-GPU-IR lowering — is delegated to the
//! `hl_cuda` service layer (`allocate`/`transfer`/`load_module`/`launch`/`synchronize`), which mutates a
//! [`CudaContext`] and submits protocol `Cmd`s through a [`hl_gpu::RemoteCommandSink`]. That sink is the
//! single boundary to the host GPU-exec service, connected lazily from `$HL_GPU_EXEC` on first submit.
//!
//! This module owns only the C-ABI marshalling state the driver API needs: the opaque handle tables for
//! `CUcontext` / `CUmodule` / `CUfunction` / `CUstream` / `CUevent`. The compute semantics are NOT
//! redefined here — they are the shared `hl_cuda` services.

use core::ffi::c_void;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use hl_cuda::model::stream::Stream;
use hl_cuda::{CudaContext, CudaDeviceDesc, Function};
use hl_gpu::RemoteCommandSink;

/// Everything the shim tracks between `cu*` calls.
pub struct State {
    /// `cuInit` was called (the driver spec-guards most calls behind this).
    pub inited: bool,
    /// The CUDA object model + lowering target (device desc, allocation/module/stream tables).
    pub ctx: CudaContext,
    /// The guest→host boundary: encodes each lowered batch and ships it framed over `$HL_GPU_EXEC`.
    pub sink: RemoteCommandSink,

    /// `CUfunction` table: an opaque handle is `index + 1`. Holds the resolved [`Function`] + entry name.
    functions: Vec<(Function, CString)>,
    /// `CUmodule` table: an opaque handle is `index + 1`, storing the `hl_cuda` module id.
    modules: Vec<u32>,
    /// `CUstream` table: an opaque handle is `index + 1`, storing the `hl_cuda` [`Stream`]. The default
    /// stream is the null handle (token `0`).
    streams: Vec<Stream>,
    /// `CUevent` table: an opaque handle is `index + 1`, holding the record timestamp (`None` until
    /// `cuEventRecord`) for `cuEventElapsedTime`/`cuEventQuery`.
    events: Vec<Option<Instant>>,

    /// `CUcontext` token allocator (opaque, non-null). One simulated device, so contexts are tokens.
    next_ctx: usize,
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
}

impl State {
    fn new() -> Self {
        // VRAM the simulated device advertises; override with `$HL_CUDA_VRAM_BYTES`. Default 8 GiB.
        let vram = std::env::var("HL_CUDA_VRAM_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(8u64 << 30);
        State {
            inited: false,
            ctx: CudaContext::new(CudaDeviceDesc::apple_default(vram)),
            // Connect target from $HL_GPU_EXEC; the connection itself is opened lazily on first submit.
            sink: RemoteCommandSink::from_env(),
            functions: Vec::new(),
            modules: Vec::new(),
            streams: Vec::new(),
            events: Vec::new(),
            next_ctx: 1,
            current_ctx: 0,
            ctx_stack: Vec::new(),
            ctx_flags: HashMap::new(),
            primary_ctx: 0,
            primary_refcount: 0,
            primary_flags: 0,
        }
    }

    // ---- context tokens ---------------------------------------------------------------------------

    pub fn create_ctx(&mut self) -> *mut c_void {
        let token = self.next_ctx;
        self.next_ctx += 1;
        self.current_ctx = token;
        token as *mut c_void
    }
    /// Mint a context token with recorded creation flags (`cuCtxCreate(flags)`).
    pub fn create_ctx_with_flags(&mut self, flags: u32) -> *mut c_void {
        let token = self.next_ctx;
        self.next_ctx += 1;
        self.current_ctx = token;
        self.ctx_flags.insert(token, flags);
        token as *mut c_void
    }
    pub fn current_ctx(&self) -> *mut c_void {
        self.current_ctx as *mut c_void
    }
    pub fn set_current_ctx(&mut self, h: *mut c_void) {
        self.current_ctx = h as usize;
    }
    pub fn destroy_ctx(&mut self, h: *mut c_void) {
        let token = h as usize;
        if self.current_ctx == token {
            self.current_ctx = 0;
        }
        self.ctx_flags.remove(&token);
    }

    /// `cuCtxPushCurrent(ctx)` — save the current context and make `ctx` current.
    pub fn push_current_ctx(&mut self, h: *mut c_void) {
        self.ctx_stack.push(self.current_ctx);
        self.current_ctx = h as usize;
    }
    /// `cuCtxPopCurrent()` — pop the saved context back to current, returning the token that *was*
    /// current (which the API hands back to the caller).
    pub fn pop_current_ctx(&mut self) -> *mut c_void {
        let popped = self.current_ctx;
        self.current_ctx = self.ctx_stack.pop().unwrap_or(0);
        popped as *mut c_void
    }
    /// The current context's creation flags (`0` if none / untracked).
    pub fn current_ctx_flags(&self) -> u32 {
        self.ctx_flags.get(&self.current_ctx).copied().unwrap_or(0)
    }
    /// Set the current context's flags (`cuCtxSetFlags`); a no-op if there is no current context.
    pub fn set_current_ctx_flags(&mut self, flags: u32) {
        if self.current_ctx != 0 {
            self.ctx_flags.insert(self.current_ctx, flags);
        }
    }

    // ---- primary context (device 0) ---------------------------------------------------------------

    /// `cuDevicePrimaryCtxRetain` — lazily create the single primary context, bump its refcount, and
    /// return its token.
    pub fn primary_ctx_retain(&mut self) -> *mut c_void {
        if self.primary_ctx == 0 {
            self.primary_ctx = self.next_ctx;
            self.next_ctx += 1;
        }
        self.primary_refcount += 1;
        self.primary_ctx as *mut c_void
    }
    /// `cuDevicePrimaryCtxRelease` — drop one reference; the last release tears the primary context down.
    pub fn primary_ctx_release(&mut self) {
        if self.primary_refcount > 0 {
            self.primary_refcount -= 1;
        }
        if self.primary_refcount == 0 {
            self.primary_ctx_teardown();
        }
    }
    /// `cuDevicePrimaryCtxReset` — force the primary context inactive regardless of refcount.
    pub fn primary_ctx_reset(&mut self) {
        self.primary_refcount = 0;
        self.primary_ctx_teardown();
    }
    fn primary_ctx_teardown(&mut self) {
        if self.primary_ctx != 0 {
            if self.current_ctx == self.primary_ctx {
                self.current_ctx = 0;
            }
            self.primary_ctx = 0;
        }
    }
    /// `cuDevicePrimaryCtxGetState` — `(flags, active)` where `active` is 1 while a reference is held.
    pub fn primary_ctx_state(&self) -> (u32, i32) {
        let flags = if self.primary_ctx != 0 { self.primary_flags } else { 0 };
        (flags, (self.primary_refcount > 0) as i32)
    }
    /// `cuDevicePrimaryCtxSetFlags` — record the primary context's flags (only while it exists).
    pub fn set_primary_ctx_flags(&mut self, flags: u32) {
        if self.primary_ctx != 0 {
            self.primary_flags = flags;
        }
    }

    // ---- module handles ---------------------------------------------------------------------------

    pub fn intern_module(&mut self, module_id: u32) -> *mut c_void {
        self.modules.push(module_id);
        self.modules.len() as *mut c_void // len == index + 1
    }
    pub fn module_id(&self, h: *mut c_void) -> Option<u32> {
        let idx = h as usize;
        (idx != 0 && idx <= self.modules.len()).then(|| self.modules[idx - 1])
    }

    // ---- function handles -------------------------------------------------------------------------

    pub fn intern_function(&mut self, f: Function, name: &str) -> *mut c_void {
        self.functions.push((f, CString::new(name).unwrap_or_default()));
        self.functions.len() as *mut c_void
    }
    pub fn function(&self, h: *mut c_void) -> Option<Function> {
        let idx = h as usize;
        (idx != 0 && idx <= self.functions.len()).then(|| self.functions[idx - 1].0)
    }

    // ---- stream handles ---------------------------------------------------------------------------

    pub fn intern_stream(&mut self, s: Stream) -> *mut c_void {
        self.streams.push(s);
        self.streams.len() as *mut c_void
    }
    /// Resolve a `CUstream` handle to its [`Stream`]. The null handle is the default stream.
    pub fn stream(&self, h: *mut c_void) -> Option<Stream> {
        let idx = h as usize;
        if idx == 0 {
            return Some(hl_cuda::model::stream::StreamTable::DEFAULT);
        }
        (idx <= self.streams.len()).then(|| self.streams[idx - 1])
    }

    // ---- event handles ----------------------------------------------------------------------------

    pub fn create_event(&mut self) -> *mut c_void {
        self.events.push(None);
        self.events.len() as *mut c_void
    }
    pub fn record_event(&mut self, h: *mut c_void) -> bool {
        let idx = h as usize;
        if idx != 0 && idx <= self.events.len() {
            self.events[idx - 1] = Some(Instant::now());
            true
        } else {
            false
        }
    }
    pub fn event_is_valid(&self, h: *mut c_void) -> bool {
        let idx = h as usize;
        idx != 0 && idx <= self.events.len()
    }
    /// `cuEventQuery` — has this (valid) event been recorded yet? With the synchronous executor a
    /// recorded event is already complete; an unrecorded one is not ready.
    pub fn event_recorded(&self, h: *mut c_void) -> bool {
        let idx = h as usize;
        idx != 0 && idx <= self.events.len() && self.events[idx - 1].is_some()
    }
    /// `cuEventElapsedTime` — milliseconds between two recorded events. `None` if either handle is
    /// invalid or unrecorded (→ `CUDA_ERROR_NOT_READY`).
    pub fn event_elapsed_ms(&self, start: *mut c_void, end: *mut c_void) -> Option<f32> {
        let a = self.event_timestamp(start)?;
        let b = self.event_timestamp(end)?;
        Some(b.saturating_duration_since(a).as_secs_f64() as f32 * 1.0e3)
    }
    fn event_timestamp(&self, h: *mut c_void) -> Option<Instant> {
        let idx = h as usize;
        if idx == 0 || idx > self.events.len() {
            return None;
        }
        self.events[idx - 1]
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

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// Run `f` with exclusive access to the global shim state. Non-reentrant — never call [`with`] from
/// inside an `f` (the `Mutex` is not recursive); each entry point does exactly one `with`.
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let m = STATE.get_or_init(|| Mutex::new(State::new()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// Reset the process-global state to a clean slate (test-only, so a unit test starts deterministic).
#[cfg(test)]
pub fn reset() {
    with(|s| *s = State::new());
}
