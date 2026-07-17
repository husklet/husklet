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

/// Ensure the context's backing IR fence exists, pushing its `CreateFence` into `cmds` on first creation.
/// Returns the fence IR id.
fn ensure_fence(ctx: &mut GlContext, cmds: &mut Vec<Cmd>) -> u32 {
    if ctx.fence_ir == 0 {
        ctx.fence_ir = ctx.alloc_fence_ir();
        cmds.push(Cmd::CreateFence(ctx.fence_ir));
    }
    ctx.fence_ir
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
    let fence = ensure_fence(ctx, &mut cmds);
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
    if flags & GL_SYNC_FLUSH_COMMANDS_BIT != 0 || timeout != 0 {
        if sink.wait(FenceId(ctx.fence_ir), value).is_err() {
            return GL_WAIT_FAILED;
        }
        ctx.fence_signaled_through = ctx.fence_signaled_through.max(value);
        return GL_CONDITION_SATISFIED;
    }
    GL_TIMEOUT_EXPIRED
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

/// `glDeleteSync(sync)` — drop the sync object. An unknown (non-null) sync raises `GL_INVALID_VALUE`.
pub fn delete_sync(ctx: &mut GlContext, sync: usize) {
    if ctx.syncs.remove(&sync).is_none() {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glIsSync(sync)` — true once `sync` names a live sync object.
pub fn is_sync(ctx: &GlContext, sync: usize) -> bool {
    ctx.syncs.contains_key(&sync)
}

/// `glGetSynciv(sync, pname, …)` — the single integer value for `pname`: `GL_SYNC_STATUS` →
/// `GL_SIGNALED`/`GL_UNSIGNALED`, `GL_OBJECT_TYPE` → `GL_SYNC_FENCE`, `GL_SYNC_CONDITION` →
/// `GL_SYNC_GPU_COMMANDS_COMPLETE`, `GL_SYNC_FLAGS` → `0`. Returns `None` (and sets the GL error) for an
/// unknown sync (`GL_INVALID_VALUE`) or an unsupported `pname` (`GL_INVALID_ENUM`).
pub fn get_synciv(ctx: &mut GlContext, sync: usize, pname: u32) -> Option<i32> {
    let Some(&value) = ctx.syncs.get(&sync) else {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return None;
    };
    match pname {
        GL_SYNC_STATUS => Some(if ctx.fence_signaled_through >= value {
            GL_SIGNALED as i32
        } else {
            GL_UNSIGNALED as i32
        }),
        GL_OBJECT_TYPE => Some(GL_SYNC_FENCE as i32),
        GL_SYNC_CONDITION => Some(GL_SYNC_GPU_COMMANDS_COMPLETE as i32),
        GL_SYNC_FLAGS => Some(0),
        _ => {
            ctx.set_gl_error(GL_INVALID_ENUM);
            None
        }
    }
}
