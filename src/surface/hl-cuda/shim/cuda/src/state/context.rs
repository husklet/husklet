//! `CUcontext` tokens: creation, the current-context stack, per-context flags, and the device-0 primary
//! context.
//!
//! One simulated device means a context carries no per-context resources of its own, so a `CUcontext` is
//! an opaque non-null token. Liveness is still tracked: CUDA lets a driver recycle handle values, so a
//! destroyed token and a token that never existed are both `CUDA_ERROR_INVALID_CONTEXT`, but a live
//! context must never be confused with a dead one — which is what accepting any non-null token did.

use super::*;

impl State {
    /// Is `h` a live `CUcontext` token? A null token is never live.
    pub fn ctx_is_live(&self, h: *mut c_void) -> bool {
        self.contexts.contains(&(h as usize))
    }

    /// The current context token as a raw `usize` (`0` = none) — the id `cuCtxGetId` reports.
    pub fn current_ctx_token(&self) -> usize {
        self.current_ctx
    }

    pub fn create_ctx(&mut self) -> *mut c_void {
        self.create_ctx_with_flags(0)
    }

    /// Mint a context token with recorded creation flags (`cuCtxCreate(flags)`).
    pub fn create_ctx_with_flags(&mut self, flags: u32) -> *mut c_void {
        let token = self.next_ctx;
        self.next_ctx += 1;
        self.contexts.insert(token);
        self.current_ctx = token;
        self.ctx_flags.insert(token, flags);
        token as *mut c_void
    }

    pub fn current_ctx(&self) -> *mut c_void {
        self.current_ctx as *mut c_void
    }

    /// `cuCtxSetCurrent(ctx)` — bind `ctx` to the calling thread. A null token detaches the current
    /// context (which CUDA permits); `false` means the token is not live → `CUDA_ERROR_INVALID_CONTEXT`.
    pub fn set_current_ctx(&mut self, h: *mut c_void) -> bool {
        if h.is_null() {
            self.current_ctx = 0;
            return true;
        }
        if !self.ctx_is_live(h) {
            return false;
        }
        self.current_ctx = h as usize;
        true
    }

    /// `cuCtxDestroy(ctx)` — retire a context token. `false` for an unknown/already-destroyed token. The
    /// token is also dropped from the push/pop stack so a later `cuCtxPopCurrent` cannot resurrect it.
    pub fn destroy_ctx(&mut self, h: *mut c_void) -> bool {
        let token = h as usize;
        if !self.contexts.remove(&token) {
            return false;
        }
        if self.current_ctx == token {
            self.current_ctx = 0;
        }
        self.ctx_stack.retain(|t| *t != token);
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
        self.ctx_stack.push(self.current_ctx);
        self.current_ctx = h as usize;
        true
    }

    /// `cuCtxPopCurrent()` — pop the calling thread's current context, restoring the saved one and
    /// returning the token that *was* current. `None` when there is no current context to pop, which is
    /// `CUDA_ERROR_INVALID_CONTEXT` in real CUDA, not a success handing back a null token.
    pub fn pop_current_ctx(&mut self) -> Option<*mut c_void> {
        if self.current_ctx == 0 {
            return None;
        }
        let popped = self.current_ctx;
        self.current_ctx = self.ctx_stack.pop().unwrap_or(0);
        Some(popped as *mut c_void)
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
