//! The opaque `CUmodule` / `CUfunction` / `CUstream` / `CUevent` handle tables.
//!
//! A handle is a token, not a pointer. Tokens are minted from an append-only table and NEVER recycled,
//! but a token alone cannot say whether the object it names is still alive: that is the `hl_cuda` model's
//! `StreamTable`/`EventTable`. Every resolver here therefore does two steps — token → object id, then
//! `is_valid` on the owning table — so `cuStreamDestroy`/`cuEventDestroy` really retire a handle and a
//! later use is `CUDA_ERROR_INVALID_HANDLE`. CUDA allows a driver to recycle handle values, so
//! "destroyed" and "never existed" are legitimately the same error; a LIVE handle being mistaken for a
//! dead one (or the reverse) is not.

use super::*;
use hl_cuda::model::stream::StreamTable;

/// `CU_STREAM_LEGACY` — the reserved `CUstream` value `(CUstream)0x1`.
pub const CU_STREAM_LEGACY: usize = 1;
/// `CU_STREAM_PER_THREAD` — the reserved `CUstream` value `(CUstream)0x2`.
pub const CU_STREAM_PER_THREAD: usize = 2;
/// Created `CUstream` tokens start above the three reserved values (`NULL`, `CU_STREAM_LEGACY`,
/// `CU_STREAM_PER_THREAD`) so a created stream can never collide with a special stream.
const STREAM_TOKEN_BASE: usize = CU_STREAM_PER_THREAD + 1;

impl State {
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
        self.functions
            .push((f, CString::new(name).unwrap_or_default()));
        self.func_cache_config.push(0);
        self.functions.len() as *mut c_void
    }
    pub fn function(&self, h: *mut c_void) -> Option<Function> {
        self.func_index(h).map(|i| self.functions[i].0)
    }
    /// The interned entry-name pointer for `cuFuncGetName` (`None` for a bad handle). The `CString` is
    /// owned by the process-global table for the process lifetime, so the pointer stays valid.
    pub fn func_name_ptr(&self, h: *mut c_void) -> Option<*const core::ffi::c_char> {
        self.func_index(h).map(|i| self.functions[i].1.as_ptr())
    }
    /// Index into the parallel `CUfunction` tables for a handle (`None` for null / out-of-range).
    fn func_index(&self, h: *mut c_void) -> Option<usize> {
        let idx = h as usize;
        (idx != 0 && idx <= self.functions.len()).then_some(idx - 1)
    }
    /// Record a function's preferred cache config (`cuFuncSetCacheConfig`); `false` for a bad handle.
    pub fn set_func_cache_config(&mut self, h: *mut c_void, c: i32) -> bool {
        match self.func_index(h) {
            Some(i) => {
                self.func_cache_config[i] = c;
                true
            }
            None => false,
        }
    }

    // ---- stream handles ---------------------------------------------------------------------------

    pub fn intern_stream(&mut self, s: Stream, flags: u32, priority: i32) -> *mut c_void {
        self.streams.push(s);
        self.stream_meta.push((flags, priority));
        (self.streams.len() - 1 + STREAM_TOKEN_BASE) as *mut c_void
    }

    /// Resolve a `CUstream` token to its LIVE [`Stream`]. The reserved tokens (`NULL`,
    /// `CU_STREAM_LEGACY`, `CU_STREAM_PER_THREAD`) all name the always-live default stream, which is not
    /// an ordinary allocation and can never be destroyed. A created token resolves only while
    /// `cuStreamDestroy` has not retired it; afterwards it is indistinguishable from a token that never
    /// existed, and both are `CUDA_ERROR_INVALID_HANDLE`.
    pub fn stream(&self, h: *mut c_void) -> Option<Stream> {
        let token = h as usize;
        if token <= CU_STREAM_PER_THREAD {
            return Some(StreamTable::DEFAULT);
        }
        let stream = *self.streams.get(token - STREAM_TOKEN_BASE)?;
        self.ctx.streams.is_valid(stream).then_some(stream)
    }

    /// The `(flags, priority)` a live `CUstream` was created with. The reserved default-stream tokens
    /// report `(0, 0)`; a destroyed or unknown token is `None`.
    pub fn stream_meta(&self, h: *mut c_void) -> Option<(u32, i32)> {
        let token = h as usize;
        if token <= CU_STREAM_PER_THREAD {
            return Some((0, 0));
        }
        self.stream(h)?;
        self.stream_meta.get(token - STREAM_TOKEN_BASE).copied()
    }

    /// `cuStreamDestroy` — retire a created stream. `false` for an unknown/already-destroyed token and
    /// for the reserved default-stream tokens, which CUDA does not allow an application to destroy.
    pub fn destroy_stream(&mut self, h: *mut c_void) -> bool {
        let Some(stream) = self.stream(h) else {
            return false;
        };
        self.ctx.streams.destroy(stream)
    }

    // ---- event handles ----------------------------------------------------------------------------

    pub fn create_event(&mut self) -> *mut c_void {
        let event = self.ctx.event_create();
        self.events.push(event);
        self.event_times.push(None);
        self.events.len() as *mut c_void // len == index + 1
    }

    /// Resolve a `CUevent` token to its LIVE [`Event`]. `cuEventDestroy` retires the model object, so a
    /// destroyed token stops resolving — unlike a cleared timestamp slot, which only means "created but
    /// not yet recorded".
    fn event(&self, h: *mut c_void) -> Option<Event> {
        let token = h as usize;
        if token == 0 {
            return None;
        }
        let event = *self.events.get(token - 1)?;
        self.ctx.events.is_valid(event).then_some(event)
    }

    pub fn event_is_valid(&self, h: *mut c_void) -> bool {
        self.event(h).is_some()
    }

    /// `cuEventRecord` — mark a live event recorded and stamp it from the monotonic clock. `false` for an
    /// unknown or already-destroyed handle.
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

    /// `cuEventQuery` — has this (live) event been recorded yet? With the synchronous executor a
    /// recorded event is already complete; an unrecorded one is not ready.
    pub fn event_recorded(&self, h: *mut c_void) -> bool {
        self.event(h)
            .is_some_and(|event| self.ctx.events.is_recorded(event))
    }

    /// `cuEventDestroy` — retire a live event. `false` for an unknown or already-destroyed handle.
    pub fn destroy_event(&mut self, h: *mut c_void) -> bool {
        let Some(event) = self.event(h) else {
            return false;
        };
        self.event_times[h as usize - 1] = None;
        self.ctx.event_destroy(event).is_ok()
    }

    /// `cuEventElapsedTime` — milliseconds between two recorded events. `None` if either handle is
    /// invalid or unrecorded (→ `CUDA_ERROR_NOT_READY`).
    pub fn event_elapsed_ms(&self, start: *mut c_void, end: *mut c_void) -> Option<f32> {
        let a = self.event_timestamp(start)?;
        let b = self.event_timestamp(end)?;
        Some(b.saturating_duration_since(a).as_secs_f64() as f32 * 1.0e3)
    }

    fn event_timestamp(&self, h: *mut c_void) -> Option<Instant> {
        self.event(h)?;
        self.event_times[h as usize - 1]
    }
}
