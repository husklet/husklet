//! The cudart shim's process-global state + guest→host command sink (mirrors the cuda shim's `state`).
//!
//! The runtime API's memory + stream ops lower through the same `hl_cuda` services and the same
//! [`hl_gpu::RemoteCommandSink`] boundary; this module owns only the C-ABI marshalling state: the
//! `cudaStream_t` handle table, the sticky last-error, and the selected device ordinal.

use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use hl_cuda::model::event::Event;
use hl_cuda::model::stream::{Stream, StreamTable};
use hl_cuda::service::register::Registry;
use hl_cuda::{CudaContext, CudaDeviceDesc};

/// `cudaStreamLegacy` — the reserved `cudaStream_t` value `((cudaStream_t)1)`.
pub const CUDA_STREAM_LEGACY: usize = 1;
/// `cudaStreamPerThread` — the reserved `cudaStream_t` value `((cudaStream_t)2)`.
pub const CUDA_STREAM_PER_THREAD: usize = 2;
/// Created `cudaStream_t` tokens start above the reserved values (`NULL`, `cudaStreamLegacy`,
/// `cudaStreamPerThread`) so a created stream can never collide with a special stream.
const STREAM_TOKEN_BASE: usize = CUDA_STREAM_PER_THREAD + 1;
use hl_gpu::transport::DEFAULT_EXEC_SOCK;
use hl_gpu::RemoteCommandSink;

/// A `<<<>>>`-launch configuration pushed by `__cudaPushCallConfiguration` and consumed by the matching
/// `__cudaPopCallConfiguration` inside nvcc's generated device stub. Held by-value (no raw pointers) so
/// [`State`] stays `Send` for the process-global `Mutex`; the `cudaStream_t` is kept as its opaque token.
#[derive(Clone, Copy)]
pub struct CallCfg {
    pub grid: [u32; 3],
    pub block: [u32; 3],
    pub shmem: usize,
    /// The `cudaStream_t` opaque token (`0` = default stream).
    pub stream: usize,
}

pub struct State {
    /// The pid that owns this state. A mismatch means it was inherited across `fork(2)`.
    pid: u32,
    pub ctx: CudaContext,
    pub sink: RemoteCommandSink,
    /// The CUDA Runtime API `__cudaRegister*` registry: fatbin handle → module, host-fn pointer →
    /// resolved kernel. Populated by the `__cudaRegister*` entry points, read by `cudaLaunchKernel`.
    pub registry: Registry,
    /// Sticky last runtime error (`cudaGetLastError` reads + clears; `cudaPeekAtLastError` reads only).
    pub last_error: i32,
    /// Selected device ordinal (`cudaSetDevice`/`cudaGetDevice`). One simulated device → always 0.
    pub device: i32,
    /// `cudaStream_t` token table: a created handle is `index + STREAM_TOKEN_BASE`, storing the `hl_cuda`
    /// [`Stream`] whose liveness `ctx.streams` owns. Append-only — a token is never recycled.
    streams: Vec<Stream>,
    /// `cudaEvent_t` token table: a handle is `index + 1`, storing the `hl_cuda` [`Event`] whose liveness
    /// `ctx.events` owns. Append-only — a token is never recycled.
    events: Vec<Event>,
    /// Parallel to `events`: the `cudaEventRecord` timestamp (`None` until recorded), so
    /// `cudaEventElapsedTime` answers from a real monotonic clock. The model tracks *whether* an event is
    /// recorded; the wall clock is C-ABI state only this shim needs.
    event_times: Vec<Option<Instant>>,
    /// The `__cudaPushCallConfiguration`/`__cudaPopCallConfiguration` stack (nvcc's `<<<>>>` glue).
    call_configs: Vec<CallCfg>,
}

impl State {
    fn new() -> Self {
        let vram = std::env::var("HL_CUDA_VRAM_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(8u64 << 30);
        // The launcher-configured identity (`HL_CUDA_NAME` / `HL_CUDA_CC`), applied so the runtime API
        // reports the SAME device as `libcuda` and `libnvidia-ml`.
        let mut device = CudaDeviceDesc::apple_default(vram);
        device.configure(
            std::env::var("HL_CUDA_NAME").ok().as_deref(),
            std::env::var("HL_CUDA_CC").ok().as_deref(),
        );
        State {
            pid: std::process::id(),
            ctx: CudaContext::new(device),
            sink: RemoteCommandSink::new(
                std::env::var("HL_GPU_EXEC").unwrap_or_else(|_| DEFAULT_EXEC_SOCK.to_owned()),
            ),
            registry: Registry::new(),
            last_error: 0,
            device: 0,
            streams: Vec::new(),
            events: Vec::new(),
            event_times: Vec::new(),
            call_configs: Vec::new(),
        }
    }

    // ---- stream handles ---------------------------------------------------------------------------

    pub fn intern_stream(&mut self, s: Stream) -> *mut c_void {
        self.streams.push(s);
        (self.streams.len() - 1 + STREAM_TOKEN_BASE) as *mut c_void
    }

    /// Resolve a `cudaStream_t` token to its LIVE [`Stream`]. The reserved tokens (`NULL`,
    /// `cudaStreamLegacy`, `cudaStreamPerThread`) all name the always-live default stream, which is not an
    /// ordinary allocation and can never be destroyed. A created token resolves only while
    /// `cudaStreamDestroy` has not retired it; afterwards it is indistinguishable from a token that never
    /// existed, and both are `cudaErrorInvalidResourceHandle`.
    pub fn stream(&self, h: *mut c_void) -> Option<Stream> {
        let token = h as usize;
        if token <= CUDA_STREAM_PER_THREAD {
            return Some(StreamTable::DEFAULT);
        }
        let stream = *self.streams.get(token - STREAM_TOKEN_BASE)?;
        self.ctx.streams.is_valid(stream).then_some(stream)
    }

    /// `cudaStreamDestroy` — retire a created stream. `false` for an unknown/already-destroyed token and
    /// for the reserved default-stream tokens, which CUDA does not allow an application to destroy.
    pub fn destroy_stream(&mut self, h: *mut c_void) -> bool {
        let Some(stream) = self.stream(h) else {
            return false;
        };
        self.ctx.streams.destroy(stream)
    }

    // ---- event handles ----------------------------------------------------------------------------

    /// `cudaEventCreate` — mint an (unrecorded) event; the opaque handle is `index + 1`.
    pub fn create_event(&mut self) -> *mut c_void {
        let event = self.ctx.event_create();
        self.events.push(event);
        self.event_times.push(None);
        self.events.len() as *mut c_void
    }

    /// Resolve a `cudaEvent_t` token to its LIVE [`Event`]. `cudaEventDestroy` retires the model object,
    /// so a destroyed token stops resolving — unlike a cleared timestamp slot, which only means "created
    /// but not yet recorded".
    fn event(&self, h: *mut c_void) -> Option<Event> {
        let token = h as usize;
        if token == 0 {
            return None;
        }
        let event = *self.events.get(token - 1)?;
        self.ctx.events.is_valid(event).then_some(event)
    }

    /// Is `h` a live event handle?
    pub fn event_is_valid(&self, h: *mut c_void) -> bool {
        self.event(h).is_some()
    }

    /// `cudaEventRecord` — timestamp a live event with the monotonic clock. `false` for an unknown or
    /// already-destroyed handle.
    pub fn record_event(&mut self, h: *mut c_void) -> bool {
        let Some(event) = self.event(h) else {
            return false;
        };
        if self.ctx.event_record(event, StreamTable::DEFAULT).is_err() {
            return false;
        }
        self.event_times[h as usize - 1] = Some(Instant::now());
        true
    }

    /// `cudaEventQuery` — has this (live) event been recorded yet? With the synchronous executor a
    /// recorded event is already complete; an unrecorded one is not ready.
    pub fn event_recorded(&self, h: *mut c_void) -> bool {
        self.event(h)
            .is_some_and(|event| self.ctx.events.is_recorded(event))
    }

    /// `cudaEventDestroy` — retire a live event. `false` for an unknown or already-destroyed handle.
    pub fn destroy_event(&mut self, h: *mut c_void) -> bool {
        let Some(event) = self.event(h) else {
            return false;
        };
        self.event_times[h as usize - 1] = None;
        self.ctx.event_destroy(event).is_ok()
    }

    /// `cudaEventElapsedTime` — milliseconds between two recorded events. `None` if either handle is
    /// invalid or unrecorded (→ `cudaErrorNotReady`).
    pub fn event_elapsed_ms(&self, start: *mut c_void, end: *mut c_void) -> Option<f32> {
        let a = self.event_timestamp(start)?;
        let b = self.event_timestamp(end)?;
        Some(b.saturating_duration_since(a).as_secs_f64() as f32 * 1.0e3)
    }

    fn event_timestamp(&self, h: *mut c_void) -> Option<Instant> {
        self.event(h)?;
        self.event_times[h as usize - 1]
    }

    // ---- <<<>>> call-configuration stack ----------------------------------------------------------

    /// `__cudaPushCallConfiguration` — push a launch config; `false` signals the (bounded) stack overflowed.
    pub fn push_call_config(&mut self, cfg: CallCfg) -> bool {
        // A real runtime bounds the push stack; 32 nested configs is far beyond nvcc's single-level use.
        if self.call_configs.len() >= 32 {
            return false;
        }
        self.call_configs.push(cfg);
        true
    }
    /// `__cudaPopCallConfiguration` — pop the most recent launch config (`None` if the stack is empty).
    pub fn pop_call_config(&mut self) -> Option<CallCfg> {
        self.call_configs.pop()
    }

    // ---- memory info ------------------------------------------------------------------------------

    /// `cudaMemGetInfo` — `(free, total)` device bytes: total is the advertised VRAM, free is that minus
    /// the sum of every live allocation (clamped so it never underflows).
    pub fn mem_info(&self) -> (usize, usize) {
        let total = self.ctx.device.total_mem;
        let used = self.ctx.mem.total_bytes().min(total);
        ((total - used) as usize, total as usize)
    }

    /// `cudaDeviceReset()` — destroy the calling process's primary context and everything in it. Every
    /// device allocation, loaded module, `__cudaRegister*` binding, stream and event is released, so a
    /// pointer or handle obtained before the reset is no longer valid — which is the whole point of the
    /// call. Clearing only the sticky error left all of them alive and working.
    ///
    /// The device ordinal and the sticky error return to their initial values; the `$HL_GPU_EXEC` sink is
    /// rebuilt too, so the next call opens a fresh connection (real CUDA likewise re-creates the primary
    /// context lazily).
    pub fn reset_device(&mut self) {
        let pid = self.pid;
        *self = State::new();
        self.pid = pid;
    }

    /// CUDA does not inherit a context across `fork(2)`, and the engine implements a guest `fork()` as a
    /// real host fork. Without this the child would hold a copy of the parent's `$HL_GPU_EXEC` fd and
    /// believe it owns the parent's buffer ids — two processes interleaving frames on one socket. Dropping
    /// the inherited state closes only the CHILD's copy of the fd and empties every handle table, so an
    /// inherited `cudaStream_t`/`cudaEvent_t` is `cudaErrorInvalidResourceHandle` and an inherited device
    /// pointer is `cudaErrorInvalidValue`. The runtime API has no explicit init, so a child that starts
    /// over gets its own context and its own connection — never the parent's.
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

    /// Record a runtime error as the sticky last-error and return it (for the `?`-style early returns).
    pub fn fail(&mut self, code: i32) -> i32 {
        if code != 0 {
            self.last_error = code;
        }
        code
    }
}

pub struct ShimState;

impl ShimState {
    /// Run `f` with exclusive global shim-state access. State inherited across `fork(2)` is disowned first
    /// (see [`State::disown_after_fork`]).
    pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        let state = STATE.get_or_init(|| Mutex::new(State::new()));
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        state.disown_after_fork();
        f(&mut state)
    }
}

/// Serialize the tests that drive the process-global state (a single `OnceLock<Mutex<State>>` shared by
/// the whole test binary) so their `reset()` + `$HL_GPU_EXEC` manipulation never interleave under the
/// default parallel test runner.
#[cfg(test)]
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reset the process-global state to a clean slate (test-only, so a unit test starts deterministic).
#[cfg(test)]
pub fn reset() {
    ShimState::with(|state| *state = State::new());
}
