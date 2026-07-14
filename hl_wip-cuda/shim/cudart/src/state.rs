//! The cudart shim's process-global state + guest→host command sink (mirrors the cuda shim's `state`).
//!
//! The runtime API's memory + stream ops lower through the same `hl_cuda` services and the same
//! [`hl_gpu::RemoteCommandSink`] boundary; this module owns only the C-ABI marshalling state: the
//! `cudaStream_t` handle table, the sticky last-error, and the selected device ordinal.

use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use hl_cuda::model::stream::Stream;
use hl_cuda::service::register::Registry;
use hl_cuda::{CudaContext, CudaDeviceDesc};
use hl_gpu::RemoteCommandSink;

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
        }
    }

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

    /// Record a runtime error as the sticky last-error and return it (for the `?`-style early returns).
    pub fn fail(&mut self, code: i32) -> i32 {
        if code != 0 {
            self.last_error = code;
        }
        code
    }
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let m = STATE.get_or_init(|| Mutex::new(State::new()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}
