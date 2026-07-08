//! Drive a [`GpuBackend`] from decoded IR — the host-side replay loop.
//!
//! On the real host this reads frames out of the shared-memory command ring, decodes each [`Cmd`], and
//! calls the backend. Here it is exercised headless against the mock and software backends.

use crate::backend::{GpuBackend, PresentToken};
use crate::id::*;
use crate::ir::*;
use crate::wire::Decoder;
use crate::Result;

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
        Cmd::CreateShader { id, spirv } => be.create_shader(ShaderId(*id), spirv)?,
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

/// Decode a wire stream (as produced by [`crate::ir::encode_stream`]) and replay it.
pub fn replay_stream(be: &mut dyn GpuBackend, bytes: &[u8]) -> Result<Vec<PresentToken>> {
    let mut d = Decoder::new(bytes);
    let mut presents = Vec::new();
    while !d.is_empty() {
        // L8 fast-path: `WriteBuffer` carries the bulk of a frame's bytes (glmark2's horse re-uploads
        // ~1 MB/frame). Decode its header in place and hand the backend a BORROWED slice of the stream,
        // skipping the `to_vec` copy that `Cmd::decode` would otherwise make. Semantically identical.
        if d.peek_u8() == Some(crate::ir::tag::WRITE_BUFFER) {
            let _ = d.u8()?; // tag
            let id = d.u32()?;
            let offset = d.u64()?;
            let data = d.bytes()?; // &[u8] view into the stream — zero copy
            be.write_buffer(BufferId(id), offset, data)?;
            continue;
        }
        let cmd = Cmd::decode(&mut d)?;
        if let Some(tok) = apply(be, &cmd)? {
            presents.push(tok);
        }
    }
    Ok(presents)
}
