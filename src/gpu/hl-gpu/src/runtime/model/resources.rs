//! The per-connection resource ownership + residency-cost data the runtime owns.
//!
//! Two responsibilities live here, both pure data + arithmetic (no workflow — that is `service/`):
//!
//! * [`SessionResources`] — the singular owner of protocol id → executor-native handle. One
//!   [`ResourceTable`] per resource kind; a [`GpuExecutor`](crate::runtime::port::executor::GpuExecutor)
//!   stores its own native object (a `Box<dyn Any>`) behind each entry, so lifecycle checking (duplicate
//!   create / use-after-free / double-free) is enforced once, uniformly, by the protocol table while the
//!   native type stays private to whichever executor (CPU / wgpu) is injected.
//! * the residency-cost model — [`texture_bytes`], [`surface_bytes`], [`create_charge`], [`destroy_key`]
//!   and the per-connection [`Ledger`] / shared [`GlobalLedger`] accounting *state*. The transactional
//!   charge/commit *workflow* over this state is `service/account.rs`.
//!
//! Ported from `hl-gpu/src/limits.rs` (the `texture_bytes`/`surface_bytes`/`create_charge`/`destroy_key`
//! math + the `Totals`/`GlobalBudget` accounting shape), split so the state lives in `model/` and the
//! transaction lives in `service/`.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::protocol::model::command::Cmd;
use crate::protocol::model::descriptor::{BindResource, SurfaceDesc, TextureDesc};
use crate::protocol::model::enums::TextureDim;
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{
    BindGroupId, BufferId, FenceId, PipelineId, ResourceTable, SamplerId, ShaderId, SurfaceId,
    TextureId,
};

// ---------------------------------------------------------------------------------------------------
// SessionResources — protocol ids → executor-native handles
// ---------------------------------------------------------------------------------------------------

/// An executor's native object for one resource, stored type-erased behind its protocol id. The CPU
/// reference executor puts its own buffer/texture struct here; a wgpu executor puts a `wgpu::Buffer`;
/// the runtime neither knows nor names the concrete type — it only enforces the id lifecycle.
pub type Native = Box<dyn Any>;

/// The singular per-connection owner of `protocol id → executor-native handle`, one table per kind.
///
/// The injected [`GpuExecutor`](crate::runtime::port::executor::GpuExecutor) mutates this in `execute`:
/// on a create it inserts its native object (an id already live is a typed `DuplicateId`), on a destroy
/// it removes it (an unknown id is a typed `UnknownId`). Because the table is the *only* id map, a
/// use-after-free becomes a clean `GpuError` instead of undefined behaviour on a real driver.
pub struct SessionResources {
    pub buffers: ResourceTable<Native>,
    pub textures: ResourceTable<Native>,
    pub samplers: ResourceTable<Native>,
    pub shaders: ResourceTable<Native>,
    pub pipelines: ResourceTable<Native>,
    pub bind_groups: ResourceTable<Native>,
    pub surfaces: ResourceTable<Native>,
    pub fences: ResourceTable<Native>,
}

impl SessionResources {
    pub fn new() -> Self {
        Self {
            buffers: ResourceTable::new(BufferId::KIND),
            textures: ResourceTable::new(TextureId::KIND),
            samplers: ResourceTable::new(SamplerId::KIND),
            shaders: ResourceTable::new(ShaderId::KIND),
            pipelines: ResourceTable::new(PipelineId::KIND),
            bind_groups: ResourceTable::new(BindGroupId::KIND),
            surfaces: ResourceTable::new(SurfaceId::KIND),
            fences: ResourceTable::new(FenceId::KIND),
        }
    }

    /// Begin an all-tables transaction so a whole batch's resource-lifecycle mutations (the executor's
    /// creates/destroys during `execute`) can be rolled back as a unit if the frame NACKs. Paired with
    /// [`commit_txn`](Self::commit_txn) on success or [`rollback_txn`](Self::rollback_txn) on failure.
    pub fn begin_txn(&mut self) {
        self.buffers.begin();
        self.textures.begin();
        self.samplers.begin();
        self.shaders.begin();
        self.pipelines.begin();
        self.bind_groups.begin();
        self.surfaces.begin();
        self.fences.begin();
    }

    /// Commit the in-flight all-tables transaction: every destroyed native parked during the batch is
    /// freed for real. Call after the executor accepted the whole batch.
    pub fn commit_txn(&mut self) {
        self.buffers.commit();
        self.textures.commit();
        self.samplers.commit();
        self.shaders.commit();
        self.pipelines.commit();
        self.bind_groups.commit();
        self.surfaces.commit();
        self.fences.commit();
    }

    /// Roll back the in-flight all-tables transaction: every id created during the batch is dropped and
    /// every id destroyed during the batch is restored, leaving the tables EXACTLY as they were before the
    /// frame. Call when the executor NACKed (failed mid-batch) so a retry / subsequent destroy is consistent.
    pub fn rollback_txn(&mut self) {
        self.buffers.rollback();
        self.textures.rollback();
        self.samplers.rollback();
        self.shaders.rollback();
        self.pipelines.rollback();
        self.bind_groups.rollback();
        self.surfaces.rollback();
        self.fences.rollback();
    }

    /// Total live objects across every kind — a cheap invariant check for tests/diagnostics.
    pub fn live_count(&self) -> usize {
        self.buffers.len()
            + self.textures.len()
            + self.samplers.len()
            + self.shaders.len()
            + self.pipelines.len()
            + self.bind_groups.len()
            + self.surfaces.len()
            + self.fences.len()
    }
}

impl Default for SessionResources {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------------
// residency accounting state (transaction workflow lives in service/account.rs)
// ---------------------------------------------------------------------------------------------------

/// Residency counters for one connection. `bytes`/`objects` are the aggregate charge; `compiled_bytes`
/// is the subset attributable to the compiled-pipeline (PSO/AIR) cache, bounded by its own ceiling.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Totals {
    pub bytes: u64,
    pub objects: u64,
    pub compiled_bytes: u64,
}

/// The per-connection accounting ledger: the live `(kind, id) → bytes` charges and their running
/// [`Totals`]. Pure state — `service/account.rs` computes a proposed next ledger and commits it.
#[derive(Clone, Default)]
pub struct Ledger {
    pub live: HashMap<(u8, u32), u64>,
    pub totals: Totals,
}

impl Ledger {
    /// Cumulative bytes resident on this connection.
    pub fn residency_bytes(&self) -> u64 {
        self.totals.bytes
    }
    /// Cumulative live object count charged to this connection.
    pub fn object_count(&self) -> u64 {
        self.totals.objects
    }
    /// Bytes of this connection's residency attributable to the compiled-pipeline cache.
    pub fn compiled_cache_bytes(&self) -> u64 {
        self.totals.compiled_bytes
    }
}

/// The process-global residency ceiling shared across every connection (clone shares the same account).
/// Enforces a fair global budget so one connection cannot starve the host of all GPU memory.
#[derive(Clone)]
pub struct GlobalLedger {
    inner: Arc<Mutex<Totals>>,
    max_bytes: u64,
    max_objects: u64,
}

impl GlobalLedger {
    pub fn new(max_bytes: u64, max_objects: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Totals::default())),
            max_bytes,
            max_objects,
        }
    }

    /// An effectively-unbounded global account (per-connection ceilings still apply).
    pub fn unbounded() -> Self {
        Self::new(u64::MAX, u64::MAX)
    }

    /// Atomically swap this connection's global contribution from `old` to `next`, rejecting if the
    /// resulting process-wide total would exceed a ceiling. On rejection the global account is unchanged.
    /// `compiled_bytes` is a per-connection concern and is not tracked process-globally.
    pub fn commit(&self, old: Totals, next: Totals) -> Result<()> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let without = Totals {
            bytes: g.bytes.saturating_sub(old.bytes),
            objects: g.objects.saturating_sub(old.objects),
            compiled_bytes: 0,
        };
        let proposed = Totals {
            bytes: without
                .bytes
                .checked_add(next.bytes)
                .ok_or(GpuError::ResourceLimit("global residency overflow"))?,
            objects: without
                .objects
                .checked_add(next.objects)
                .ok_or(GpuError::ResourceLimit("global object overflow"))?,
            compiled_bytes: 0,
        };
        if proposed.bytes > self.max_bytes || proposed.objects > self.max_objects {
            return Err(GpuError::ResourceLimit("global residency"));
        }
        *g = proposed;
        Ok(())
    }

    /// Release this connection's whole contribution back to the global account (on disconnect).
    pub fn refund(&self, totals: Totals) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.bytes = g.bytes.saturating_sub(totals.bytes);
        g.objects = g.objects.saturating_sub(totals.objects);
    }

    /// A snapshot of the process-wide residency currently charged across every connection sharing this
    /// account. A leak check: it must return to its baseline once all sharing connections tear down.
    pub fn snapshot(&self) -> Totals {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Process-wide bytes currently resident across every connection on this shared account.
    pub fn residency_bytes(&self) -> u64 {
        self.snapshot().bytes
    }

    /// Process-wide live object count across every connection on this shared account.
    pub fn object_count(&self) -> u64 {
        self.snapshot().objects
    }
}

// ---------------------------------------------------------------------------------------------------
// residency-cost model (charge kinds + byte-footprint math)
// ---------------------------------------------------------------------------------------------------

// Residency accounting kinds (the first tuple element of the `(kind, id)` live-object key). Distinct per
// resource class so a buffer id and a surface id sharing a numeric value are charged separately.
pub const KIND_BUFFER: u8 = 1;
pub const KIND_TEXTURE: u8 = 2;
pub const KIND_SAMPLER: u8 = 3;
pub const KIND_SHADER: u8 = 4;
pub const KIND_PIPELINE: u8 = 5;
pub const KIND_BIND_GROUP: u8 = 6;
pub const KIND_SURFACE: u8 = 7;
pub const KIND_FENCE: u8 = 8;
pub const KIND_EXTERNAL: u8 = 9;

/// Fixed residency charged for a timeline fence's host-side signal/wait state.
pub const FENCE_BYTES: u64 = 128;

/// The human-readable resource-kind tag for an accounting `kind` byte — the SAME string the executor's
/// [`ResourceTable`] carries for that kind, so a `DuplicateId`/`UnknownId` the account raises is
/// byte-identical to the one the executor would raise for the same id. `KIND_EXTERNAL` has no protocol
/// resource table (it is an imported allocation, not an IR-created object), so it names itself.
pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        KIND_BUFFER => BufferId::KIND,
        KIND_TEXTURE => TextureId::KIND,
        KIND_SAMPLER => SamplerId::KIND,
        KIND_SHADER => ShaderId::KIND,
        KIND_PIPELINE => PipelineId::KIND,
        KIND_BIND_GROUP => BindGroupId::KIND,
        KIND_SURFACE => SurfaceId::KIND,
        KIND_FENCE => FenceId::KIND,
        KIND_EXTERNAL => "external allocation",
        _ => "resource",
    }
}

/// Backing residency (bytes) an executor keeps resident for a presentable surface: one full-frame render
/// target at the surface's format footprint.
pub fn surface_bytes(d: &SurfaceDesc) -> u64 {
    let texel = d.format.bytes_per_texel().unwrap_or(4) as u64;
    (d.width as u64)
        .saturating_mul(d.height as u64)
        .saturating_mul(texel)
}

/// Full mip-chain byte footprint of a texture, also validating its shape (zero/oversized dims, mip count
/// vs. extent, dimensionality-specific constraints) — a malformed descriptor is a typed `ResourceLimit`
/// rejection before any charge or executor allocation.
pub fn texture_bytes(d: &TextureDesc) -> Result<u64> {
    if d.width == 0 || d.height == 0 || d.depth == 0 {
        return Err(GpuError::ResourceLimit("zero texture dimension"));
    }
    if d.mip_levels == 0 {
        return Err(GpuError::ResourceLimit("zero texture mip levels"));
    }
    if d.sample_count == 0 {
        return Err(GpuError::ResourceLimit("zero texture sample count"));
    }
    match d.dim {
        TextureDim::D1 if d.height != 1 || d.sample_count != 1 => {
            return Err(GpuError::ResourceLimit("invalid 1D texture shape"));
        }
        TextureDim::D2 => {}
        TextureDim::D3 if d.sample_count != 1 => {
            return Err(GpuError::ResourceLimit("invalid 3D texture sample count"));
        }
        TextureDim::Cube if d.width != d.height || d.depth % 6 != 0 || d.sample_count != 1 => {
            return Err(GpuError::ResourceLimit("invalid cube texture shape"));
        }
        _ => {}
    }
    let max_mip_dimension = match d.dim {
        TextureDim::D1 => d.width,
        TextureDim::D2 | TextureDim::Cube => d.width.max(d.height),
        TextureDim::D3 => d.width.max(d.height).max(d.depth),
    };
    let max_mips = u32::BITS - max_mip_dimension.leading_zeros();
    if d.mip_levels > max_mips {
        return Err(GpuError::ResourceLimit(
            "texture mip levels exceed dimensions",
        ));
    }
    // Depth/stencil formats report `bytes_per_texel() == None` (the software backend can't clear-fill
    // them) but for footprint accounting they occupy a real 4-byte-per-texel target; charge that instead
    // of rejecting a valid depth render target.
    let texel = d.format.bytes_per_texel().unwrap_or(4) as u64;
    let mut total = 0u64;
    let mut w = d.width as u64;
    let mut h = d.height as u64;
    let mut depth = d.depth as u64;
    for _ in 0..d.mip_levels {
        let level = w
            .max(1)
            .checked_mul(h.max(1))
            .and_then(|v| v.checked_mul(depth.max(1)))
            .and_then(|v| v.checked_mul(d.sample_count as u64))
            .and_then(|v| v.checked_mul(texel))
            .ok_or(GpuError::ResourceLimit("texture footprint overflow"))?;
        total = total
            .checked_add(level)
            .ok_or(GpuError::ResourceLimit("texture footprint overflow"))?;
        w >>= 1;
        h >>= 1;
        if d.dim == TextureDim::D3 {
            depth >>= 1;
        }
    }
    Ok(total)
}

/// The residency charge a `Create*` command incurs: `(kind, id, bytes)`, or `None` for a non-charging
/// command. Errors only when a texture descriptor is malformed (via [`texture_bytes`]).
pub fn create_charge(cmd: &Cmd) -> Result<Option<(u8, u32, u64)>> {
    Ok(match cmd {
        Cmd::CreateBuffer(id, d) => Some((KIND_BUFFER, *id, d.size)),
        Cmd::CreateTexture(id, d) => Some((KIND_TEXTURE, *id, texture_bytes(d)?)),
        Cmd::CreateSampler(id, _) => Some((KIND_SAMPLER, *id, 64)),
        Cmd::CreateShader { id, spirv, .. } => {
            Some((KIND_SHADER, *id, (spirv.len() as u64).saturating_mul(4)))
        }
        // A pipeline's charge is its compiled-cache (PSO/AIR) footprint; the account service also meters
        // it against the negotiated per-connection compiled-cache ceiling.
        Cmd::CreateRenderPipeline(id, _) | Cmd::CreateComputePipeline(id, _) => {
            Some((KIND_PIPELINE, *id, 4096))
        }
        Cmd::CreateBindGroup(id, d) => {
            let referenced = d
                .entries
                .iter()
                .map(|e| match e.resource {
                    BindResource::Buffer { size, .. } => size,
                    _ => 64,
                })
                .sum::<u64>();
            Some((
                KIND_BIND_GROUP,
                *id,
                256u64.saturating_add(referenced.min(4096)),
            ))
        }
        // A presentable surface pins one full-frame render target resident on the executor.
        Cmd::CreateSurface(id, d) => Some((KIND_SURFACE, *id, surface_bytes(d))),
        // A timeline fence pins a small amount of host-side signal state.
        Cmd::CreateFence(id) => Some((KIND_FENCE, *id, FENCE_BYTES)),
        _ => None,
    })
}

/// The `(kind, id)` a `Destroy*` command refunds, or `None` for a non-refunding command.
pub fn destroy_key(cmd: &Cmd) -> Option<(u8, u32)> {
    match cmd {
        Cmd::DestroyBuffer(id) => Some((KIND_BUFFER, *id)),
        Cmd::DestroyTexture(id) => Some((KIND_TEXTURE, *id)),
        Cmd::DestroySampler(id) => Some((KIND_SAMPLER, *id)),
        Cmd::DestroyShader(id) => Some((KIND_SHADER, *id)),
        Cmd::DestroyPipeline(id) => Some((KIND_PIPELINE, *id)),
        Cmd::DestroyBindGroup(id) => Some((KIND_BIND_GROUP, *id)),
        Cmd::DestroySurface(id) => Some((KIND_SURFACE, *id)),
        Cmd::DestroyFence(id) => Some((KIND_FENCE, *id)),
        _ => None,
    }
}
