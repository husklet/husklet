//! [`GpuExecutor`] — the host-side execution port, one of the three small ports that replace the old
//! god-trait `GpuBackend` (§2 of the v2 overview).
//!
//! The runtime validates + accounts a decoded command batch and then hands it to a `GpuExecutor` it does
//! **not** choose (the host binary injects a CPU reference executor or a wgpu executor). This is the
//! contract those executors implement: it references only protocol types + [`SessionResources`], so it is
//! object-safe (`&mut dyn GpuExecutor`) and free of any wgpu/Metal/CUDA/fd type.
//!
//! Ported from `hl-gpu/src/backend.rs` (the `GpuBackend` trait), collapsing its per-resource lifecycle
//! methods into a single batch `execute` over the runtime-owned [`SessionResources`] (the executor stores
//! natives behind those id entries) while keeping the `capabilities` / fence-`wait` semantics unchanged.

use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::command::Cmd;
use crate::protocol::model::descriptor::{FrameSerial, SurfaceToken};
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{BufferId, FenceId, SurfaceId, TextureId};
use crate::protocol::port::sink::FenceWait;
use crate::runtime::model::resources::Native;
use crate::runtime::model::resources::SessionResources;
use crate::runtime::model::sharing::Shared;

/// The outcome of a `Present` command executed within a batch: which surface presented which texture.
/// (The out-of-band presentable-image handle is delivered to the compositor on a separate channel; the
/// runtime surfaces only the protocol-id pairing here.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Presentation {
    pub surface: SurfaceId,
    pub token: SurfaceToken,
    pub texture: TextureId,
    pub serial: FrameSerial,
}

/// The durable state an executor actually committed from a batch.
///
/// `commands` contains only replay-safe commands. Observations (`WaitFence`) and presentation are not
/// journaled; a fully accepted `Submit` is, while a partially lowered one makes `replayable` false because
/// its successful inner prefix cannot be represented without executing refused operations again. `source`
/// preserves the original command position solely for runtime accounting and sharing release plans.
#[derive(Clone, Debug, PartialEq)]
pub struct CommittedCommand {
    pub source: usize,
    pub command: Cmd,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommittedDelta {
    pub commands: Vec<CommittedCommand>,
    pub fence_signals: Vec<(u32, u64)>,
    pub presentations: Vec<Presentation>,
    pub replayable: bool,
    pub(crate) sources: Vec<usize>,
}

impl CommittedDelta {
    fn from_indices(
        batch: &[Cmd],
        indices: impl IntoIterator<Item = usize>,
        presentations: Vec<Presentation>,
        partially_lowered_submits: &[usize],
        scheduled_signals: Option<Vec<(u32, u64)>>,
    ) -> Self {
        let mut commands = Vec::new();
        let mut fence_signals = Vec::new();
        let mut replayable = true;
        let sources: Vec<usize> = indices.into_iter().collect();
        assert!(sources.windows(2).all(|pair| pair[0] < pair[1]), "committed command indices must be strictly ordered and unique");
        assert!(
            partially_lowered_submits.windows(2).all(|pair| pair[0] < pair[1]),
            "partially lowered submit indices must be strictly ordered and unique"
        );
        assert!(
            partially_lowered_submits.iter().all(|source| {
                sources.contains(source) && matches!(batch.get(*source), Some(Cmd::Submit(_)))
            }),
            "a partially lowered submit must name a committed Submit command"
        );
        for &source in &sources {
            let command = batch.get(source).unwrap_or_else(|| panic!("committed command index is out of range"));
            match command {
                Cmd::Submit(cb) => {
                    if partially_lowered_submits.contains(&source) {
                        // A partially lowered Submit cannot be reconstructed from the original command:
                        // replay would also rerun its refused suffix.
                        replayable = false;
                    } else {
                        commands.push(CommittedCommand { source, command: command.clone() });
                        if let Some(signal) = cb.signal { fence_signals.push(signal); }
                    }
                }
                Cmd::Present { .. } | Cmd::WaitFence { .. } => {}
                command => commands.push(CommittedCommand { source, command: command.clone() }),
            }
        }
        for presentation in &presentations {
            assert!(sources.iter().any(|&source| matches!(batch.get(source), Some(Cmd::Present { surface, texture, serial })
                if *surface == presentation.surface.0 && *texture == presentation.texture.0 && *serial == presentation.serial)),
                "a committed presentation must correspond to a successful Present command");
        }
        if let Some(signals) = scheduled_signals { fence_signals = signals; }
        Self { commands, fence_signals, presentations, replayable, sources }
    }

    pub fn replay_commands(&self) -> impl Iterator<Item = &Cmd> {
        self.commands.iter().map(|entry| &entry.command)
    }

    pub(crate) fn contains_source(&self, source: usize) -> bool {
        self.sources.contains(&source)
    }
}

/// Work the executor completed from one batch. A nonfatal operation refusal is reported separately from
/// fatal execution failure because the successful commands on either side are committed.
#[derive(Debug, PartialEq)]
pub struct Execution {
    committed: CommittedDelta,
    refusal: Option<GpuError>,
    complete: bool,
}

impl std::ops::Deref for Execution {
    type Target = [Presentation];

    fn deref(&self) -> &Self::Target {
        &self.committed.presentations
    }
}

impl Execution {
    pub fn accepted(presentations: Vec<Presentation>) -> Self {
        Self {
            committed: CommittedDelta { commands: Vec::new(), fence_signals: Vec::new(), presentations, replayable: true, sources: Vec::new() },
            refusal: None,
            complete: true,
        }
    }

    pub fn partial(
        presentations: Vec<Presentation>,
        refusal: GpuError,
        batch: &[Cmd],
        committed: Vec<usize>,
        partially_lowered_submits: Vec<usize>,
        scheduled_signals: Vec<(u32, u64)>,
    ) -> Self {
        assert!(
            !refusal.is_fatal(),
            "a partial execution cannot contain a fatal error"
        );
        Self {
            committed: CommittedDelta::from_indices(
                batch,
                committed,
                presentations,
                &partially_lowered_submits,
                Some(scheduled_signals),
            ),
            refusal: Some(refusal),
            complete: false,
        }
    }

    pub fn presentations(&self) -> &[Presentation] {
        &self.committed.presentations
    }

    pub(crate) fn into_parts(mut self, batch: &[Cmd]) -> (CommittedDelta, Option<GpuError>) {
        if self.complete {
            self.committed = CommittedDelta::from_indices(
                batch,
                0..batch.len(),
                self.committed.presentations,
                &[],
                None,
            );
        }
        (self.committed, self.refusal)
    }
}

/// The host executor a runtime `Session` drives. Object-safe so the runtime holds `&mut dyn GpuExecutor`
/// and both a pure CPU reference executor and a wgpu executor implement the same contract.
///
/// The runtime guarantees ordering: a batch reaches [`execute`](GpuExecutor::execute) only *after* it has
/// been fully validated (shape/limits) and accounted (residency charged), so an executor never has to
/// re-validate limits. [`Execution::partial`] commits every successfully executed command while reporting
/// a nonfatal refusal; `Err` is a fatal batch failure and the runtime atomically rolls resources back.
pub trait GpuExecutor {
    /// The capability descriptor this executor advertises; the runtime negotiates a guest's
    /// [`FeatureRequest`](crate::protocol::model::capability::FeatureRequest) against it before any
    /// command flows.
    fn capabilities(&self) -> Capabilities;

    /// Execute a validated, accounted batch against the runtime-owned `resources`. The executor inserts
    /// its native object behind each created id and removes it on destroy (lifecycle errors surface as
    /// typed [`GpuError`](crate::protocol::model::error::GpuError)s from the resource tables), records
    /// encoder work on `Submit`, and returns one [`Presentation`] per `Present` command in order. Return
    /// [`Execution::partial`] after continuing past a nonfatal operation refusal. Reserve `Err` for an
    /// outcome whose resource and accounting state must be rolled back atomically.
    fn execute(
        &mut self,
        resources: &mut SessionResources,
        batch: &[Cmd],
    ) -> Result<Execution>;

    /// Block until timeline fence `fence` reaches `value`. Serves the `CommandSink::wait` path (an
    /// out-of-band wait not carried inside a command batch); `resources` is passed so the executor can
    /// resolve the fence's native primitive.
    fn wait(&mut self, resources: &mut SessionResources, fence: FenceId, value: u64) -> Result<()>;

    /// Poll fence completion without blocking.
    fn poll_fence(
        &mut self,
        resources: &SessionResources,
        fence: FenceId,
        value: u64,
    ) -> Result<bool> {
        let _ = (resources, fence, value);
        Err(GpuError::Unsupported("executor: poll_fence"))
    }

    fn wait_timeout(
        &mut self,
        resources: &mut SessionResources,
        fence: FenceId,
        value: u64,
        timeout_ns: u64,
    ) -> Result<FenceWait> {
        if timeout_ns == 0 {
            return self.poll_fence(resources, fence, value).map(|done| {
                if done {
                    FenceWait::Complete
                } else {
                    FenceWait::Timeout
                }
            });
        }
        self.wait(resources, fence, value)?;
        Ok(FenceWait::Complete)
    }

    /// Read `len` bytes back from buffer `id` at `offset` out of the runtime-owned `resources` — the
    /// host-side half of the device→host readback path (`CommandSink::read_buffer` /`cuMemcpyDtoH`).
    ///
    /// Additive: the default returns [`GpuError::Unsupported`] so an executor that cannot expose device
    /// memory keeps compiling; the CPU reference executor overrides it over its `SessionResources` natives.
    fn read_buffer(
        &self,
        resources: &SessionResources,
        id: BufferId,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let _ = (resources, id, offset, len);
        Err(GpuError::Unsupported("executor: read_buffer"))
    }

    /// Return a shareable native alias and its authoritative logical byte length for `id`.
    ///
    /// Additive and honestly unsupported by default: an executor must not satisfy this with a copy,
    /// because callers rely on writes through either session becoming visible through the other alias.
    fn export_buffer(&self, resources: &SessionResources, id: BufferId) -> Result<(Shared, u64)> {
        let _ = (resources, id);
        Err(GpuError::Unsupported("executor: export_buffer"))
    }

    /// Turn a previously exported native into the owned value an importing session can insert into its
    /// resource table. `bytes` is the registry's authoritative length and must agree with the native.
    fn import_buffer(&self, resource: Shared, bytes: u64) -> Result<Native> {
        let _ = (resource, bytes);
        Err(GpuError::Unsupported("executor: import_buffer"))
    }

    /// Return a zero-copy shareable alias for a live texture and its authoritative residency size.
    fn export_texture(&self, resources: &SessionResources, id: TextureId) -> Result<(Shared, u64)> {
        let _ = (resources, id);
        Err(GpuError::Unsupported("executor: export_texture"))
    }

    /// Turn a texture export into a native texture for an importing session.
    fn import_texture(&self, resource: Shared, bytes: u64) -> Result<Native> {
        let _ = (resource, bytes);
        Err(GpuError::Unsupported("executor: import_texture"))
    }

    /// Flush and complete all device work submitted before this call.
    fn sharing_barrier(&mut self) -> Result<()> {
        Err(GpuError::Unsupported("executor: sharing_barrier"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CpuExecutor;

    #[test]
    fn cpu_executor_honestly_refuses_buffer_aliasing() {
        let executor = CpuExecutor::new();
        let resources = SessionResources::new();
        let error = match executor.export_buffer(&resources, BufferId(1)) {
            Ok(_) => panic!("CPU storage cannot be aliased safely"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            GpuError::Unsupported("executor: export_buffer")
        ));

        let shared: Shared = std::sync::Arc::new(());
        let error = match executor.import_buffer(shared, 4) {
            Ok(_) => panic!("CPU storage cannot import an alias safely"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            GpuError::Unsupported("executor: import_buffer")
        ));
    }
}
