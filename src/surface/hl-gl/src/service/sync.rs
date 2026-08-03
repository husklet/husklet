//! Sync objects (GLES3.0 `GLsync`) lowered onto the hl-GPU IR **fence timeline**.
//!
//! Every fence sync this context creates rides one monotonic IR fence (`ctx.fence_ir`, lazily
//! `CreateFence`d): `glFenceSync` submits a command buffer that SIGNALS the fence at the next timeline
//! value and remembers `token → value`; `glClientWaitSync` blocks on that value through the
//! `CommandSink::wait` device→host wait; `glWaitSync` submits a device-side `WaitFence`; `glGetSynciv`
//! reports signaled/unsignaled against the highest value observed reached. This is the GL analogue of a
//! cuda event recorded + waited on the same fence timeline.

use crate::model::context::GlContext;
use crate::model::glconst::*;
use hl_gpu::protocol::model::id::FenceId;
use hl_gpu::{Cmd, CommandBuffer, CommandSink};

impl GlContext {
    /// Ensure the context's backing IR fence exists.
    fn ensure_fence(&mut self, commands: &mut Vec<Cmd>) -> Option<u32> {
        if self.fence_ir == 0 {
            let Ok(fence) = self.alloc_fence_ir() else {
                self.set_gl_error(crate::result::GL_OUT_OF_MEMORY);
                return None;
            };
            self.fence_ir = fence;
            commands.push(Cmd::CreateFence(self.fence_ir));
        }
        Some(self.fence_ir)
    }

    /// Drop a sync object, recording `GL_INVALID_VALUE` for an unknown token.
    pub fn delete_sync(&mut self, sync: usize) {
        if self.syncs.remove(&sync).is_none() {
            self.set_gl_error(GL_INVALID_VALUE);
        } else {
            self.set_pointer_label(sync, None);
        }
    }

    /// Return whether `sync` names a live object.
    pub fn has_sync(&self, sync: usize) -> bool {
        self.syncs.contains_key(&sync)
    }

    pub fn sync_value(&self, sync: usize) -> Option<u64> {
        self.syncs.get(&sync).copied()
    }

    pub fn mark_fence_signaled(&mut self, value: u64) {
        self.fence_signaled_through = self.fence_signaled_through.max(value);
    }
}

/// `glFenceSync(condition, flags)` — insert a fence into the command stream: signal the IR fence at the
/// next timeline value and mint a sync token for it. `condition` must be `GL_SYNC_GPU_COMMANDS_COMPLETE`
/// and `flags` must be `0` (else `GL_INVALID_ENUM`/`GL_INVALID_VALUE`, returning `None` → a null `GLsync`).
pub fn fence_sync(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    condition: u32,
    flags: u32,
) -> Option<usize> {
    if condition != GL_SYNC_GPU_COMMANDS_COMPLETE {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return None;
    }
    if flags != 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return None;
    }
    let mut cmds: Vec<Cmd> = Vec::new();
    let fence = ctx.ensure_fence(&mut cmds)?;
    let value = ctx.fence_next_value;
    ctx.fence_next_value += 1;
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: Vec::new(),
        signal: Some((fence, value)),
    }));
    // A transport error leaves the token uncreated (the caller surfaces it via eglGetError).
    if sink.submit(&cmds).is_err() {
        return None;
    }
    let token = ctx.mint_sync_token();
    ctx.syncs.insert(token, value);
    Some(token)
}

/// `glClientWaitSync(sync, flags, timeout)` — a client (host) wait on the fence value `sync` marks.
/// Returns `GL_ALREADY_SIGNALED` if already reached, else (a flush flag or a non-zero timeout) blocks on
/// the fence and returns `GL_CONDITION_SATISFIED`; a zero timeout with no flush polls to
/// `GL_TIMEOUT_EXPIRED`. A bad flag / unknown sync raises the GL error and returns `GL_WAIT_FAILED`.
pub fn client_wait_sync(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    sync: usize,
    flags: u32,
    timeout: u64,
) -> u32 {
    if flags & !GL_SYNC_FLUSH_COMMANDS_BIT != 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return GL_WAIT_FAILED;
    }
    let Some(&value) = ctx.syncs.get(&sync) else {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return GL_WAIT_FAILED;
    };
    if ctx.fence_signaled_through >= value {
        return GL_ALREADY_SIGNALED;
    }
    if timeout == 0 {
        return GL_TIMEOUT_EXPIRED;
    }
    if flags & GL_SYNC_FLUSH_COMMANDS_BIT != 0 || timeout != 0 {
        return match sink.wait_timeout(FenceId(ctx.fence_ir), value, timeout) {
            Ok(hl_gpu::FenceWait::Complete) => {
                ctx.fence_signaled_through = ctx.fence_signaled_through.max(value);
                GL_CONDITION_SATISFIED
            }
            Ok(hl_gpu::FenceWait::Timeout) => GL_TIMEOUT_EXPIRED,
            Err(_) => GL_WAIT_FAILED,
        };
    }
    GL_TIMEOUT_EXPIRED
}

/// Complete the latest fence submitted by this context. `glFinish` uses this after flushing recorded
/// draws so a subsequent zero-time client wait observes `GL_ALREADY_SIGNALED`.
pub fn finish(ctx: &mut GlContext, sink: &mut dyn CommandSink) -> hl_gpu::Result<u64> {
    let value = ctx.fence_next_value.saturating_sub(1);
    if ctx.fence_ir == 0 || value == 0 || ctx.fence_signaled_through >= value {
        return Ok(value);
    }
    if sink.wait_timeout(FenceId(ctx.fence_ir), value, u64::MAX)? == hl_gpu::FenceWait::Complete {
        ctx.mark_fence_signaled(value);
    }
    Ok(value)
}

/// `glWaitSync(sync, flags, timeout)` — a device-side (queue) wait: the GPU defers subsequent work until
/// the fence value is reached. GL requires `flags == 0` and `timeout == GL_TIMEOUT_IGNORED`; a violation
/// or an unknown sync raises `GL_INVALID_VALUE`. Lowers to a `WaitFence` on the IR fence timeline.
pub fn wait_sync(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    sync: usize,
    flags: u32,
    timeout: u64,
) {
    if flags != 0 || timeout != GL_TIMEOUT_IGNORED {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let Some(&value) = ctx.syncs.get(&sync) else {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    };
    let _ = sink.submit(&[Cmd::WaitFence {
        id: ctx.fence_ir,
        value,
    }]);
}

/// `glGetSynciv(sync, pname, …)` — the single integer value for `pname`: `GL_SYNC_STATUS` →
/// `GL_SIGNALED`/`GL_UNSIGNALED`, `GL_OBJECT_TYPE` → `GL_SYNC_FENCE`, `GL_SYNC_CONDITION` →
/// `GL_SYNC_GPU_COMMANDS_COMPLETE`, `GL_SYNC_FLAGS` → `0`. Returns `None` (and sets the GL error) for an
/// unknown sync (`GL_INVALID_VALUE`) or an unsupported `pname` (`GL_INVALID_ENUM`).
pub fn get_synciv(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    sync: usize,
    pname: u32,
) -> Option<i32> {
    let Some(&value) = ctx.syncs.get(&sync) else {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return None;
    };
    match pname {
        GL_SYNC_STATUS => {
            if ctx.fence_signaled_through < value
                && sink
                    .poll_fence(FenceId(ctx.fence_ir), value)
                    .unwrap_or(false)
            {
                ctx.fence_signaled_through = value;
            }
            Some(if ctx.fence_signaled_through >= value {
                GL_SIGNALED as i32
            } else {
                GL_UNSIGNALED as i32
            })
        }
        GL_OBJECT_TYPE => Some(GL_SYNC_FENCE as i32),
        GL_SYNC_CONDITION => Some(GL_SYNC_GPU_COMMANDS_COMPLETE as i32),
        GL_SYNC_FLAGS => Some(0),
        _ => {
            ctx.set_gl_error(GL_INVALID_ENUM);
            None
        }
    }
}
