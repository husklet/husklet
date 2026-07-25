//! The [`GpuExecutor`] impl — a thin router from the validated command batch to the native handlers in
//! the sibling modules, storing every native object behind its protocol id in [`SessionResources`] exactly
//! as the CPU reference executor does. All shape/limit validation happened in the runtime before a batch
//! reaches here, so this layer only performs native work and surfaces typed lifecycle errors.

use hl_gpu::protocol::model::capability::Capabilities;
use hl_gpu::protocol::model::command::Cmd;
use hl_gpu::protocol::model::id::{BufferId, FenceId};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::runtime::port::executor::{GpuExecutor, Presentation};
use hl_gpu::{GpuError, Result};

use crate::{fence, present, WgpuExecutor};

impl GpuExecutor for WgpuExecutor {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn execute(&mut self, res: &mut SessionResources, batch: &[Cmd]) -> Result<Vec<Presentation>> {
        // The runtime dispatches this batch inside an all-tables transaction (`begin_txn` → execute →
        // `commit_txn`/`rollback_txn`): if we return `Err`, the id tables roll back to the pre-batch state.
        // The dedup caches live outside `SessionResources`, so mirror that lifecycle here — journal every
        // cache mutation during the batch and, on failure, replay the inverses so the caches (and their
        // residency counters) stay in exact lock-step with the rolled-back id tables. On success the journal
        // is dropped and any zero-refcount backing is swept.
        self.dedup.begin_batch();
        match self.execute_batch(res, batch) {
            Ok(presents) => {
                self.dedup.commit_batch();
                Ok(presents)
            }
            Err(e) => {
                self.dedup.rollback_batch();
                Err(e)
            }
        }
    }

    fn wait(&mut self, res: &mut SessionResources, fence_id: FenceId, value: u64) -> Result<()> {
        if fence::Fence::value(res, fence_id.0)? < value {
            return Err(GpuError::Invalid("wait on a fence value never signalled"));
        }
        Ok(())
    }

    fn read_buffer(
        &self,
        res: &SessionResources,
        id: BufferId,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        self.read_bytes(res, id.0, offset, len)
    }
}

impl WgpuExecutor {
    /// Run one validated command batch against `res`. Wrapped by [`GpuExecutor::execute`], which brackets it
    /// with the dedup-cache transaction hooks so a partial-batch failure rolls the caches back too.
    fn execute_batch(
        &mut self,
        res: &mut SessionResources,
        batch: &[Cmd],
    ) -> Result<Vec<Presentation>> {
        let mut presents = Vec::new();
        for cmd in batch {
            match cmd {
                Cmd::CreateBuffer(id, d) => {
                    let b = self.make_buffer(d.size);
                    res.buffers.insert(*id, Box::new(b))?;
                }
                Cmd::DestroyBuffer(id) => {
                    res.buffers.remove(*id)?;
                }
                Cmd::WriteBuffer { id, offset, data } => {
                    self.write_bytes(res, *id, *offset, data)?
                }
                Cmd::CreateTexture(id, d) => {
                    let t = self.make_texture(d)?;
                    res.textures.insert(*id, Box::new(t))?;
                }
                Cmd::DestroyTexture(id) => {
                    res.textures.remove(*id)?;
                }
                Cmd::CreateSampler(id, d) => self.create_sampler(res, *id, d)?,
                Cmd::DestroySampler(id) => {
                    res.samplers.remove(*id)?;
                }
                Cmd::CreateShader { id, kind, spirv } => {
                    self.create_shader(res, *id, *kind, spirv)?
                }
                Cmd::DestroyShader(id) => self.destroy_shader(res, *id)?,
                Cmd::CreateRenderPipeline(id, d) => self.create_render_pipeline(res, *id, d)?,
                Cmd::CreateComputePipeline(id, d) => self.create_compute_pipeline(res, *id, d)?,
                Cmd::DestroyPipeline(id) => self.destroy_pipeline(res, *id)?,
                Cmd::CreateBindGroup(id, d) => self.create_bind_group(res, *id, d)?,
                Cmd::DestroyBindGroup(id) => {
                    res.bind_groups.remove(*id)?;
                }
                Cmd::CreateSurface(id, d) => res.surfaces.insert(*id, Box::new(d.clone()))?,
                Cmd::DestroySurface(id) => {
                    res.surfaces.remove(*id)?;
                }
                Cmd::CreateFence(id) => res.fences.insert(*id, Box::new(0u64))?,
                Cmd::DestroyFence(id) => {
                    res.fences.remove(*id)?;
                }
                Cmd::Submit(cb) => self.submit_cb(res, cb)?,
                Cmd::WaitFence { id, value } => {
                    if fence::Fence::value(res, *id)? < *value {
                        return Err(GpuError::Invalid("wait on a fence value never signalled"));
                    }
                }
                Cmd::Present { surface, texture } => {
                    presents.push(present::present(res, *surface, *texture)?);
                }
            }
        }
        Ok(presents)
    }
}
