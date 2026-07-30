//! The [`GpuExecutor`] impl — a thin router from the validated command batch to the native handlers in
//! the sibling modules, storing every native object behind its protocol id in [`SessionResources`] exactly
//! as the CPU reference executor does. All shape/limit validation happened in the runtime before a batch
//! reaches here, so this layer only performs native work and surfaces typed lifecycle errors.

use hl_gpu::protocol::model::capability::Capabilities;
use hl_gpu::protocol::model::command::Cmd;
#[cfg(target_os = "macos")]
use hl_gpu::protocol::model::descriptor::SurfaceDesc;
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
        self.module_journal.clear();
        self.pipeline_journal.clear();
        #[cfg(target_os = "macos")]
        self.presentation_journal.clear();
        let result = self.execute_batch(res, batch);
        // `write_buffer` operations are queue ordered. One batch-boundary submit makes a write-only batch
        // observable and replaces the former empty submit after every aligned write. Flush on failure too:
        // queued writes cannot be cancelled, and retaining them past transactional resource rollback would
        // make their eventual submission timing depend on an unrelated later batch.
        self.flush_writes();
        match result {
            Ok(presents) => {
                self.dedup.commit_batch();
                self.modules.apply(std::mem::take(&mut self.module_journal));
                self.pipelines
                    .apply(std::mem::take(&mut self.pipeline_journal));
                #[cfg(target_os = "macos")]
                self.presentation_journal.clear();
                Ok(presents)
            }
            Err(e) => {
                self.dedup.rollback_batch();
                self.module_journal.clear();
                self.pipeline_journal.clear();
                #[cfg(target_os = "macos")]
                {
                    let mut completions = self
                        .presentation_completions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    for key in self.presentation_journal.drain(..) {
                        completions.remove(&key);
                    }
                }
                Err(e)
            }
        }
    }

    fn wait(&mut self, res: &mut SessionResources, fence_id: FenceId, value: u64) -> Result<()> {
        self.wait_for_completion();
        if fence::Fence::value(res, fence_id.0)? < value {
            return Err(GpuError::Invalid("wait on a fence value never signalled"));
        }
        Ok(())
    }

    fn poll_fence(
        &mut self,
        res: &SessionResources,
        fence_id: FenceId,
        value: u64,
    ) -> Result<bool> {
        self.gpu.device.poll(wgpu::Maintain::Poll);
        Ok(fence::Fence::value(res, fence_id.0)? >= value)
    }

    fn wait_timeout(
        &mut self,
        res: &mut SessionResources,
        fence_id: FenceId,
        value: u64,
        timeout_ns: u64,
    ) -> Result<hl_gpu::FenceWait> {
        if fence::Fence::scheduled(res, fence_id.0)? < value {
            return Err(GpuError::Invalid("wait on a fence value never signalled"));
        }
        if timeout_ns == u64::MAX {
            self.wait_for_completion();
            return Ok(hl_gpu::FenceWait::Complete);
        }
        let started = std::time::Instant::now();
        let timeout = std::time::Duration::from_nanos(timeout_ns);
        loop {
            self.gpu.device.poll(wgpu::Maintain::Poll);
            if fence::Fence::value(res, fence_id.0)? >= value {
                return Ok(hl_gpu::FenceWait::Complete);
            }
            if started.elapsed() >= timeout {
                return Ok(hl_gpu::FenceWait::Timeout);
            }
            std::thread::yield_now();
        }
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
                    self.flush_writes();
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
                Cmd::CreateTextureView(id, desc) => {
                    let view = self.make_texture_view(res, desc)?;
                    res.textures.insert(*id, Box::new(view))?;
                }
                Cmd::DestroyTextureView(id) => {
                    res.textures.remove(*id)?;
                }
                Cmd::CreateSampler(id, d) => self.create_sampler(res, *id, d)?,
                Cmd::DestroySampler(id) => {
                    res.samplers.remove(*id)?;
                }
                Cmd::CreateShader { id, kind, spirv } => {
                    let started = std::time::Instant::now();
                    self.create_shader(res, *id, *kind, spirv)?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.shaders.add(started.elapsed());
                    }
                }
                Cmd::DestroyShader(id) => self.destroy_shader(res, *id)?,
                Cmd::CreateRenderPipeline(id, d) => {
                    let started = std::time::Instant::now();
                    self.create_render_pipeline(res, *id, d, None, Default::default())?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.render_pipelines.add(started.elapsed());
                    }
                }
                Cmd::CreateComputePipeline(id, d) => {
                    let started = std::time::Instant::now();
                    self.create_compute_pipeline(res, *id, d, None)?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.compute_pipelines.add(started.elapsed());
                    }
                }
                Cmd::CreateRenderPipelineLayout(id, d, layout, multisample) => {
                    let started = std::time::Instant::now();
                    self.create_render_pipeline(res, *id, d, Some(layout), *multisample)?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.render_pipelines.add(started.elapsed());
                    }
                }
                Cmd::CreateComputePipelineLayout(id, d, layout) => {
                    let started = std::time::Instant::now();
                    self.create_compute_pipeline(res, *id, d, Some(layout))?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.compute_pipelines.add(started.elapsed());
                    }
                }
                Cmd::DestroyPipeline(id) => self.destroy_pipeline(res, *id)?,
                Cmd::CreateBindGroup(id, d) => {
                    let started = std::time::Instant::now();
                    self.create_bind_group(res, *id, d)?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.bind_groups.add(started.elapsed());
                    }
                }
                Cmd::DestroyBindGroup(id) => {
                    res.bind_groups.remove(*id)?;
                }
                Cmd::CreateSurface(id, d) => res.surfaces.insert(*id, Box::new(d.clone()))?,
                Cmd::DestroySurface(id) => {
                    #[cfg(target_os = "macos")]
                    if let Ok(surface) = res.surfaces.get(*id) {
                        if let Some(desc) = surface.downcast_ref::<SurfaceDesc>() {
                            self.presentation_completions
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .retain(|(token, _), _| *token != desc.token.get());
                        }
                    }
                    res.surfaces.remove(*id)?;
                }
                Cmd::CreateFence(id) => res.fences.insert(*id, Box::new(fence::Fence::state()))?,
                Cmd::DestroyFence(id) => {
                    res.fences.remove(*id)?;
                }
                Cmd::Submit(cb) => {
                    // Preserve the protocol's host-write-before-command-buffer ordering. One flush covers
                    // the entire consecutive write run instead of issuing one empty submission per write.
                    self.flush_writes();
                    let started = std::time::Instant::now();
                    self.submit_cb(res, cb)?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.logical_submissions.add(started.elapsed());
                    }
                }
                Cmd::WaitFence { id, value } => {
                    self.flush_writes();
                    if fence::Fence::scheduled(res, *id)? < *value {
                        return Err(GpuError::Invalid("wait on a fence value never signalled"));
                    }
                }
                Cmd::Present {
                    surface,
                    texture,
                    serial,
                } => {
                    self.flush_writes();
                    presents.push(present::present(self, res, *surface, *texture, *serial)?);
                }
            }
        }
        Ok(presents)
    }
}
