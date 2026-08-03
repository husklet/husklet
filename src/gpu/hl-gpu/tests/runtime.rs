//! Runtime-layer tests: drive a `Session` through negotiate → validate → account → dispatch against a
//! `FakeExecutor` and assert the pipeline's contracts — failure atomicity (a rejected batch never reaches
//! the executor and never mutates residency) and transactional residency accounting (charge on create,
//! refund on destroy, reject over-limit before any mutation).

use std::any::Any;

use hl_gpu::protocol::model::capability::{shader_payload, ALL_COMMANDS};
use hl_gpu::protocol::model::command::CommandBuffer;
use hl_gpu::protocol::model::descriptor::{BufferDesc, SurfaceDesc, TextureDesc};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, TextureDim, TextureFormat};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::runtime::port::executor::{GpuExecutor, Presentation};
use hl_gpu::runtime::service;
use hl_gpu::{
    Capabilities, Cmd, Enc, FakeClock, FeatureRequest, FenceId, GlobalLedger, GpuError, Limits,
    Result, Session, SurfaceId, TextureId,
};

// A recording executor: advertises canned capabilities, records every `execute`/`wait` call, and
// mirrors the batch's resource lifecycle into the runtime-owned `SessionResources` (so a create/destroy
// mismatch would surface as a typed table error). Its native handle is a unit `()` behind each id.
struct FakeExecutor {
    caps: Capabilities,
    executed: Vec<Vec<Cmd>>,
    waits: Vec<(u32, u64)>,
}

impl FakeExecutor {
    fn new(caps: Capabilities) -> Self {
        Self {
            caps,
            executed: Vec::new(),
            waits: Vec::new(),
        }
    }
    fn command_count(&self) -> usize {
        self.executed.iter().map(Vec::len).sum()
    }
}

impl GpuExecutor for FakeExecutor {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn execute(&mut self, res: &mut SessionResources, batch: &[Cmd]) -> Result<hl_gpu::Execution> {
        self.executed.push(batch.to_vec());
        let native = || -> Box<dyn Any> { Box::new(()) };
        let mut presents = Vec::new();
        for cmd in batch {
            match cmd {
                Cmd::CreateBuffer(id, _) => res.buffers.insert(*id, native())?,
                Cmd::DestroyBuffer(id) => {
                    res.buffers.remove(*id)?;
                }
                Cmd::CreateSurface(id, desc) => res.surfaces.insert(*id, Box::new(desc.clone()))?,
                Cmd::DestroySurface(id) => {
                    res.surfaces.remove(*id)?;
                }
                Cmd::CreateFence(id) => res.fences.insert(*id, native())?,
                Cmd::DestroyFence(id) => {
                    res.fences.remove(*id)?;
                }
                Cmd::Present {
                    surface,
                    texture,
                    serial,
                } => {
                    let token = res
                        .surfaces
                        .get(*surface)?
                        .downcast_ref::<SurfaceDesc>()
                        .expect("fake surface stores its descriptor")
                        .token;
                    presents.push(Presentation {
                        surface: SurfaceId(*surface),
                        token,
                        texture: TextureId(*texture),
                        serial: *serial,
                    });
                }
                _ => {}
            }
        }
        Ok(hl_gpu::Execution::accepted(presents))
    }

    fn wait(&mut self, _res: &mut SessionResources, fence: FenceId, value: u64) -> Result<()> {
        self.waits.push((fence.0, value));
        Ok(())
    }
}

fn buffer(id: u32, size: u64) -> Cmd {
    Cmd::CreateBuffer(
        id,
        BufferDesc {
            size,
            usage: buffer_usage::COPY_DST,
            label: String::new(),
        },
    )
}

// A 2D RGBA8 texture create — the shape a Chrome compositor tile / SharedImage backing takes. `dim` px²
// is `dim*dim*4` bytes of residency (single mip), so a small `dim` gives a predictable per-tile charge.
fn texture(id: u32, dim: u32) -> Cmd {
    Cmd::CreateTexture(
        id,
        TextureDesc {
            width: dim,
            height: dim,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::SAMPLED | texture_usage::RENDER_TARGET,
            label: String::new(),
        },
    )
}

// An executor that MIRRORS the batch's buffer/texture lifecycle into the runtime-owned tables (like
// `FakeExecutor`) but NACKs the frame the moment it reaches a `Present` — modelling a real executor that
// applies the frame's creates/destroys and only THEN fails device validation on the swap's submit/present
// (the exact Chrome NACK). It applies the creates/destroys BEFORE the failure, so without the runtime's
// transaction the id tables would be left half-mutated when the frame is rejected.
struct NackOnPresentExecutor {
    caps: Capabilities,
}

impl NackOnPresentExecutor {
    fn new(caps: Capabilities) -> Self {
        Self { caps }
    }
}

impl GpuExecutor for NackOnPresentExecutor {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn execute(&mut self, res: &mut SessionResources, batch: &[Cmd]) -> Result<hl_gpu::Execution> {
        let native = || -> Box<dyn Any> { Box::new(()) };
        for cmd in batch {
            match cmd {
                Cmd::CreateBuffer(id, _) => res.buffers.insert(*id, native())?,
                Cmd::DestroyBuffer(id) => {
                    res.buffers.remove(*id)?;
                }
                Cmd::CreateTexture(id, _) => res.textures.insert(*id, native())?,
                Cmd::DestroyTexture(id) => {
                    res.textures.remove(*id)?;
                }
                // The swap's present: the frame's creates/destroys are already applied above; now NACK,
                // exactly as the wgpu executor does when the swap fails device validation.
                Cmd::Present { .. } => {
                    return Err(GpuError::Invalid("wgpu: pass failed device validation"))
                }
                _ => {}
            }
        }
        Ok(hl_gpu::Execution::accepted(Vec::new()))
    }

    fn wait(&mut self, _res: &mut SessionResources, _fence: FenceId, _value: u64) -> Result<()> {
        Ok(())
    }
}

fn session(limits: Limits, global: GlobalLedger) -> Session {
    Session::new(limits, global, Box::new(FakeClock::new(1_000)))
}

#[path = "runtime/negotiation.rs"]
mod negotiation;
#[path = "runtime/partial.rs"]
mod partial;
#[path = "runtime/recovery.rs"]
mod recovery;
#[path = "runtime/residency.rs"]
mod residency;
#[path = "runtime/validation.rs"]
mod validation;
