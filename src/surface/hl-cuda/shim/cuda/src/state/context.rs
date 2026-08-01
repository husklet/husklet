//! `CUcontext` tokens: creation, the current-context stack, per-context flags, and the device-0 primary
//! context.
//!
//! One simulated device means a context carries no per-context resources of its own, so a `CUcontext` is
//! an opaque non-null token. Liveness is still tracked: CUDA lets a driver recycle handle values, so a
//! destroyed token and a token that never existed are both `CUDA_ERROR_INVALID_CONTEXT`, but a live
//! context must never be confused with a dead one — which is what accepting any non-null token did.
//!
//! # Which state is per-thread
//!
//! CUDA binds the current context, and the `cuCtxPushCurrent`/`cuCtxPopCurrent` stack, **per thread**:
//! `cuCtxSetCurrent` on one thread is invisible to every other, and each thread has its own stack.
//! These were process-global fields on `State`, so the last thread to call `cuCtxSetCurrent` silently
//! redirected all the others — measured against a shipped bundle with two barrier-sequenced threads:
//! thread A set context `0x2`, then read back `0x3`, thread B's. The failure was quiet, which is the
//! worst shape available. Both threads still had *a* context, so every later call succeeded under the
//! wrong one and an application got wrong results with no error anywhere.
//!
//! So the binding lives in [`CURRENT`], a thread-local, while the set of LIVE tokens and their flags
//! stay on `State`: a context is a process-wide object that any thread may name or destroy, and only the
//! binding is thread-private. That split is why `State::require_context` checks liveness rather than
//! merely that a token is set — another thread may have destroyed the context this one still names.
//!
//! Concurrency itself was never the problem: the process-global mutex serialises submits correctly, and
//! 192 concurrent launch-and-readback pairs across eight threads showed zero cross-talk. Only the
//! semantics were wrong.

use std::cell::RefCell;

use super::*;

/// One thread's context binding: the token it currently has bound (`0` = none) and its own
/// push/pop stack of previously-bound tokens.
#[derive(Default)]
struct Binding {
    token: usize,
    stack: Vec<usize>,
}

thread_local! {
    static CURRENT: RefCell<Binding> = RefCell::new(Binding::default());
}

fn with_binding<R>(f: impl FnOnce(&mut Binding) -> R) -> R {
    CURRENT.with(|b| f(&mut b.borrow_mut()))
}

/// The calling thread's bound context token (`0` = none). Free rather than a `State` method because it
/// reads no shared state — the binding belongs to the thread, not to the mutex.
pub(super) fn current_token() -> usize {
    with_binding(|b| b.token)
}

/// Drop this thread's binding and stack. Used after `fork(2)`, where the inherited handle tables are
/// discarded and the token names nothing.
pub(super) fn reset_current() {
    with_binding(|b| {
        b.token = 0;
        b.stack.clear();
    });
}

impl State {
    /// Is `h` a live `CUcontext` token? A null token is never live.
    pub fn ctx_is_live(&self, h: *mut c_void) -> bool {
        self.contexts.contains(&(h as usize))
    }

    /// The calling thread's context token as a raw `usize` (`0` = none) — the id `cuCtxGetId` reports.
    pub fn current_ctx_token(&self) -> usize {
        current_token()
    }

    pub fn create_ctx(&mut self) -> *mut c_void {
        self.create_ctx_with_flags(0)
    }

    /// Mint a context token with recorded creation flags (`cuCtxCreate(flags)`).
    pub fn create_ctx_with_flags(&mut self, flags: u32) -> *mut c_void {
        let token = self.next_ctx;
        self.next_ctx += 1;
        self.contexts.insert(token);
        // `cuCtxCreate` makes the new context current ON THE CALLING THREAD, and pushes it onto that
        // thread's stack. Other threads keep whatever they had bound.
        with_binding(|b| {
            if b.token != 0 {
                b.stack.push(b.token);
            }
            b.token = token;
        });
        self.ctx_flags.insert(token, flags);
        token as *mut c_void
    }

    pub fn current_ctx(&self) -> *mut c_void {
        current_token() as *mut c_void
    }

    /// `cuCtxSetCurrent(ctx)` — bind `ctx` to the calling thread. A null token detaches the current
    /// context (which CUDA permits); `false` means the token is not live → `CUDA_ERROR_INVALID_CONTEXT`.
    pub fn set_current_ctx(&mut self, h: *mut c_void) -> bool {
        if h.is_null() {
            with_binding(|b| b.token = 0);
            return true;
        }
        if !self.ctx_is_live(h) {
            return false;
        }
        with_binding(|b| b.token = h as usize);
        true
    }

    /// `cuCtxDestroy(ctx)` — retire a context token. `false` for an unknown/already-destroyed token. The
    /// token is also dropped from the push/pop stack so a later `cuCtxPopCurrent` cannot resurrect it.
    pub fn destroy_ctx(&mut self, h: *mut c_void) -> bool {
        let token = h as usize;
        if !self.contexts.remove(&token) {
            return false;
        }
        // Clear it from the CALLING thread's binding and stack. A thread that still names the
        // destroyed token keeps it until it next asks, and `require_context`'s liveness check refuses
        // it then — using a context another thread destroyed is undefined in CUDA, and failing closed
        // is the honest reading of undefined.
        with_binding(|b| {
            if b.token == token {
                b.token = 0;
            }
            b.stack.retain(|t| *t != token);
        });
        self.ctx_flags.remove(&token);
        if self.primary_ctx == token {
            self.primary_ctx = 0;
            self.primary_refcount = 0;
        }
        true
    }

    /// `cuCtxPushCurrent(ctx)` — save the current context and make `ctx` current. `false` for a token
    /// that is not live (→ `CUDA_ERROR_INVALID_CONTEXT`).
    pub fn push_current_ctx(&mut self, h: *mut c_void) -> bool {
        if !self.ctx_is_live(h) {
            return false;
        }
        with_binding(|b| {
            b.stack.push(b.token);
            b.token = h as usize;
        });
        true
    }

    /// `cuCtxPopCurrent()` — pop the calling thread's current context, restoring the saved one and
    /// returning the token that *was* current. `None` when there is no current context to pop, which is
    /// `CUDA_ERROR_INVALID_CONTEXT` in real CUDA, not a success handing back a null token.
    pub fn pop_current_ctx(&mut self) -> Option<*mut c_void> {
        with_binding(|b| {
            if b.token == 0 {
                return None;
            }
            let popped = b.token;
            b.token = b.stack.pop().unwrap_or(0);
            Some(popped as *mut c_void)
        })
    }

    /// The current context's creation flags (`0` if none / untracked).
    pub fn current_ctx_flags(&self) -> u32 {
        self.ctx_flags.get(&current_token()).copied().unwrap_or(0)
    }

    /// Set the current context's flags (`cuCtxSetFlags`); a no-op if there is no current context.
    pub fn set_current_ctx_flags(&mut self, flags: u32) {
        let token = current_token();
        if token != 0 {
            self.ctx_flags.insert(token, flags);
        }
    }

    // ---- primary context (device 0) ---------------------------------------------------------------

    /// `cuDevicePrimaryCtxRetain` — lazily create the single primary context, bump its refcount, and
    /// return its token.
    pub fn primary_ctx_retain(&mut self) -> *mut c_void {
        if self.primary_ctx == 0 {
            self.primary_ctx = self.next_ctx;
            self.next_ctx += 1;
            self.contexts.insert(self.primary_ctx);
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
            let token = self.primary_ctx;
            self.primary_ctx = 0;
            self.destroy_ctx(token as *mut c_void);
        }
    }

    /// `cuDevicePrimaryCtxGetState` — `(flags, active)` where `active` is 1 while a reference is held.
    pub fn report_primary_context(&self) -> (u32, i32) {
        let flags = if self.primary_ctx != 0 {
            self.primary_flags
        } else {
            0
        };
        (flags, (self.primary_refcount > 0) as i32)
    }

    /// `cuDevicePrimaryCtxSetFlags` — record the primary context's flags (only while it exists).
    pub fn set_primary_ctx_flags(&mut self, flags: u32) {
        if self.primary_ctx != 0 {
            self.primary_flags = flags;
        }
    }
}
