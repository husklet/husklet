//! Drive a [`GpuBackend`] from decoded IR — the host-side replay loop.
//!
//! On the real host this reads frames out of the shared-memory command ring, decodes each [`Cmd`], and
//! calls the backend. Here it is exercised headless against the mock and software backends.

use crate::backend::{GpuBackend, PresentToken};
use crate::id::*;
use crate::ir::*;
use crate::wire::Decoder;
use crate::{GpuError, Result};

/// Apply one decoded command to a backend. Returns an optional present token when the command was a
/// `Present` (so the caller can hand the buffer to DDP).
pub fn apply(be: &mut dyn GpuBackend, cmd: &Cmd) -> Result<Option<PresentToken>> {
    match cmd {
        Cmd::CreateBuffer(id, d) => be.create_buffer(BufferId(*id), d)?,
        Cmd::DestroyBuffer(id) => be.destroy_buffer(BufferId(*id))?,
        Cmd::WriteBuffer { id, offset, data } => be.write_buffer(BufferId(*id), *offset, data)?,
        Cmd::CreateTexture(id, d) => be.create_texture(TextureId(*id), d)?,
        Cmd::DestroyTexture(id) => be.destroy_texture(TextureId(*id))?,
        Cmd::CreateSampler(id, d) => be.create_sampler(SamplerId(*id), d)?,
        Cmd::DestroySampler(id) => be.destroy_sampler(SamplerId(*id))?,
        Cmd::CreateShader { id, kind, spirv } => be.create_shader(ShaderId(*id), *kind, spirv)?,
        Cmd::DestroyShader(id) => be.destroy_shader(ShaderId(*id))?,
        Cmd::CreateRenderPipeline(id, d) => be.create_render_pipeline(PipelineId(*id), d)?,
        Cmd::CreateComputePipeline(id, d) => be.create_compute_pipeline(PipelineId(*id), d)?,
        Cmd::DestroyPipeline(id) => be.destroy_pipeline(PipelineId(*id))?,
        Cmd::CreateBindGroup(id, d) => be.create_bind_group(BindGroupId(*id), d)?,
        Cmd::DestroyBindGroup(id) => be.destroy_bind_group(BindGroupId(*id))?,
        Cmd::CreateSurface(id, d) => be.create_surface(SurfaceId(*id), d)?,
        Cmd::DestroySurface(id) => be.destroy_surface(SurfaceId(*id))?,
        Cmd::CreateFence(id) => be.create_fence(FenceId(*id))?,
        Cmd::DestroyFence(id) => be.destroy_fence(FenceId(*id))?,
        Cmd::Submit(cb) => be.submit(cb)?,
        Cmd::WaitFence { id, value } => be.wait_fence(FenceId(*id), *value)?,
        Cmd::Present { surface, texture } => {
            let tok = be.present(SurfaceId(*surface), TextureId(*texture))?;
            return Ok(Some(tok));
        }
    }
    Ok(None)
}

/// Replay a whole in-memory command list, collecting any present tokens.
pub fn replay(be: &mut dyn GpuBackend, cmds: &[Cmd]) -> Result<Vec<PresentToken>> {
    let mut presents = Vec::new();
    for c in cmds {
        if let Some(tok) = apply(be, c)? {
            presents.push(tok);
        }
    }
    Ok(presents)
}

/// Validate that the ENTIRE wire stream decodes before it is applied — the atomic-frame guarantee. Walks
/// every command with the real decoders (so a non-finite float, unknown tag, truncated body, or bad enum
/// anywhere in the frame is caught) WITHOUT calling the backend, so a malformed command late in a frame
/// can never leave earlier commands' mutations behind. The `WriteBuffer` zero-copy benefit is preserved:
/// its bulk payload is length-checked and skipped here (no `to_vec`), and the apply pass below still hands
/// the backend a borrowed slice.
fn validate_stream(bytes: &[u8]) -> Result<()> {
    let mut d = Decoder::new(bytes);
    let mut idx = 0usize;
    while !d.is_empty() {
        let pos = d.pos();
        let tag = d.peek_u8();
        let step = if d.peek_u8() == Some(crate::ir::tag::WRITE_BUFFER) {
            // Validate the WriteBuffer header + payload length, then SKIP the payload bytes (no copy).
            (|| -> Result<()> {
                let _ = d.u8()?; // tag
                let _id = d.u32()?;
                let _offset = d.u64()?;
                let len = d.u32()? as usize;
                if len > d.remaining() {
                    return Err(GpuError::ShortBuffer);
                }
                let _ = d.raw_bytes(len)?; // borrowed view; discarded — zero allocation
                Ok(())
            })()
        } else {
            Cmd::decode(&mut d).map(|_| ())
        };
        step.map_err(|e| {
            GpuError::Decode(format!(
                "replay validation command {idx} at byte {pos}/{} tag {:?} remaining {}: {e}",
                d.len(),
                tag,
                d.remaining()
            ))
        })?;
        idx += 1;
    }
    Ok(())
}

/// Decode a wire stream (as produced by [`crate::ir::encode_stream`]) and replay it.
pub fn replay_stream(be: &mut dyn GpuBackend, bytes: &[u8]) -> Result<Vec<PresentToken>> {
    // Atomic frame: reject a malformed frame in full before touching the backend (validate-before-execute).
    validate_stream(bytes)?;
    let mut d = Decoder::new(bytes);
    let mut presents = Vec::new();
    let mut idx = 0usize;
    while !d.is_empty() {
        let pos = d.pos();
        let tag = d.peek_u8();
        // L8 fast-path: `WriteBuffer` carries the bulk of a frame's bytes (glmark2's horse re-uploads
        // ~1 MB/frame). Decode its header in place and hand the backend a BORROWED slice of the stream,
        // skipping the `to_vec` copy that `Cmd::decode` would otherwise make. Semantically identical.
        if d.peek_u8() == Some(crate::ir::tag::WRITE_BUFFER) {
            let write = (|| -> Result<(u32, u64, &[u8])> {
                let _ = d.u8()?; // tag
                let id = d.u32()?;
                let offset = d.u64()?;
                let len_pos = d.pos();
                let len = d.u32()? as usize;
                if len > d.remaining() {
                    return Err(GpuError::Decode(format!(
                        "write-buffer id {id} offset {offset} claims {len} bytes at length byte {len_pos}, only {} remain",
                        d.remaining()
                    )));
                }
                let data = d.raw_bytes(len)?; // &[u8] view into the stream — zero copy
                Ok((id, offset, data))
            })()
            .map_err(|e| {
                GpuError::Decode(format!(
                    "replay command {idx} write-buffer fast path at byte {pos}/{} tag {:?} remaining {}: {e}",
                    d.len(),
                    tag,
                    d.remaining()
                ))
            })?;
            be.write_buffer(BufferId(write.0), write.1, write.2)?;
            idx += 1;
            continue;
        }
        let cmd = Cmd::decode(&mut d).map_err(|e| {
            GpuError::Decode(format!(
                "replay command {idx} at byte {pos}/{} tag {:?} remaining {}: {e}",
                d.len(),
                tag,
                d.remaining()
            ))
        })?;
        if let Some(tok) = apply(be, &cmd)? {
            presents.push(tok);
        }
        idx += 1;
    }
    Ok(presents)
}
