//! The cudart shim's process-global state + guest→host command sink (mirrors the cuda shim's `state`).
//!
//! The runtime API's memory + stream ops lower through the same `hl_cuda` services and the same
//! [`hl_gpu::RemoteCommandSink`] boundary; this module owns only the C-ABI marshalling state: the
//! `cudaStream_t` handle table, the sticky last-error, and the selected device ordinal.

use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use hl_cuda::model::stream::Stream;
use hl_cuda::service::register::Registry;
use hl_cuda::{CudaContext, CudaDeviceDesc};
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
    pub ctx: CudaContext,
    pub sink: RemoteCommandSink,
    /// The CUDA Runtime API `__cudaRegister*` registry: fatbin handle → module, host-fn pointer →
    /// resolved kernel. Populated by the `__cudaRegister*` entry points, read by `cudaLaunchKernel`.
    pub registry: Registry,
    /// Sticky last runtime error (`cudaGetLastError` reads + clears; `cudaPeekAtLastError` reads only).
    pub last_error: i32,
    /// Selected device ordinal (`cudaSetDevice`/`cudaGetDevice`). One simulated device → always 0.
    pub device: i32,
    /// `cudaStream_t` table: an opaque handle is `index + 1`. The null handle is the default stream.
    streams: Vec<Stream>,
    /// `cudaEvent_t` table: an opaque handle is `index + 1`, holding the record timestamp (`None` until
    /// `cudaEventRecord`) so `cudaEventElapsedTime`/`cudaEventQuery` answer from a real monotonic clock.
    events: Vec<Option<Instant>>,
    /// The `__cudaPushCallConfiguration`/`__cudaPopCallConfiguration` stack (nvcc's `<<<>>>` glue).
    call_configs: Vec<CallCfg>,
}

impl State {
    fn new() -> Self {
        let vram = std::env::var("HL_CUDA_VRAM_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(8u64 << 30);
        State {
            ctx: CudaContext::new(CudaDeviceDesc::apple_default(vram)),
            sink: RemoteCommandSink::from_env(),
            registry: Registry::new(),
            last_error: 0,
            device: 0,
            streams: Vec::new(),
            events: Vec::new(),
            call_configs: Vec::new(),
        }
    }

    // ---- stream handles ---------------------------------------------------------------------------

    pub fn intern_stream(&mut self, s: Stream) -> *mut c_void {
        self.streams.push(s);
        self.streams.len() as *mut c_void
    }
    pub fn stream(&self, h: *mut c_void) -> Option<Stream> {
        let idx = h as usize;
        if idx == 0 {
            return Some(hl_cuda::model::stream::StreamTable::DEFAULT);
        }
        (idx <= self.streams.len()).then(|| self.streams[idx - 1])
    }

    // ---- event handles ----------------------------------------------------------------------------

    /// `cudaEventCreate` — mint an (unrecorded) event; the opaque handle is `index + 1`.
    pub fn create_event(&mut self) -> *mut c_void {
        self.events.push(None);
        self.events.len() as *mut c_void
    }
    /// Is `h` a live event handle?
    pub fn event_is_valid(&self, h: *mut c_void) -> bool {
        let idx = h as usize;
        idx != 0 && idx <= self.events.len()
    }
    /// `cudaEventRecord` — timestamp a valid event with the monotonic clock. `false` for a bad handle.
    pub fn record_event(&mut self, h: *mut c_void) -> bool {
        let idx = h as usize;
        if idx != 0 && idx <= self.events.len() {
            self.events[idx - 1] = Some(Instant::now());
            true
        } else {
            false
        }
    }
    /// `cudaEventQuery` — has this (valid) event been recorded yet? With the synchronous executor a
    /// recorded event is already complete; an unrecorded one is not ready.
    pub fn event_recorded(&self, h: *mut c_void) -> bool {
        let idx = h as usize;
        idx != 0 && idx <= self.events.len() && self.events[idx - 1].is_some()
    }
    /// `cudaEventDestroy` — retire a valid event (its slot is cleared, never reused). `false` for a bad
    /// handle.
    pub fn destroy_event(&mut self, h: *mut c_void) -> bool {
        let idx = h as usize;
        if idx != 0 && idx <= self.events.len() {
            self.events[idx - 1] = None;
            true
        } else {
            false
        }
    }
    /// `cudaEventElapsedTime` — milliseconds between two recorded events. `None` if either handle is
    /// invalid or unrecorded (→ `cudaErrorNotReady`).
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
    pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        let state = STATE.get_or_init(|| Mutex::new(State::new()));
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        f(&mut state)
    }
}

/// Reset the process-global state to a clean slate (test-only, so a unit test starts deterministic).
#[cfg(test)]
pub fn reset() {
    ShimState::with(|state| *state = State::new());
}
