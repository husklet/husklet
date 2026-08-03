//! `dispatch` — hand a validated + accounted batch to the executor and update the fence timeline.
//!
//! Final stage of the runtime pipeline (decode → validate → account → **dispatch**). By the time a batch
//! reaches here it has passed [`validate`](super::validate) and [`charge_frame`](super::account::charge_frame),
//! so this stage only drives the executor over the runtime-owned [`SessionResources`] and reflects the
//! batch's fence lifecycle (register/retire) and completion signals into the [`FenceTimeline`], stamped
//! with the session [`Clock`](crate::runtime::port::clock::Clock). Ported from `hl-gpu/src/replay.rs`
//! (the `apply`/`replay` loop that drove a `GpuBackend`), collapsed onto the single batch
//! [`GpuExecutor::execute`] call.

use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::Result;
use crate::protocol::model::id::{BufferId, FenceId, TextureId};
use crate::runtime::model::resources::SessionResources;
use crate::runtime::model::session::{ResourceSharing, Session};
use crate::runtime::model::sharing::{ExportId, ResourceKey};
use crate::runtime::model::timeline::FenceTimeline;
use crate::runtime::port::clock::Clock;
use crate::runtime::port::executor::{CommittedDelta, GpuExecutor};

/// Dispatch a validated, accounted batch. The executor performs the native work (creating/destroying
/// resources behind `session.resources`, recording submits, presenting); afterwards the runtime records
/// each fence's lifecycle and any completion-signal timeline values. Returns one [`Presentation`] per
/// `Present` command, in order.
struct ResourceTransaction<'a> {
    resources: &'a mut SessionResources,
    committed: bool,
}

impl<'a> ResourceTransaction<'a> {
    fn begin(resources: &'a mut SessionResources) -> Self {
        resources.begin_txn();
        Self {
            resources,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.resources.commit_txn();
        self.committed = true;
    }
}

impl Drop for ResourceTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.resources.rollback_txn();
        }
    }
}

pub(crate) struct PreparedDispatch<'a> {
    transaction: ResourceTransaction<'a>,
    timeline: Option<FenceTimeline>,
    committed: Option<CommittedDelta>,
    refusal: Option<crate::GpuError>,
}

impl PreparedDispatch<'_> {
    /// The accepted path's infallible tail: all validation and executor work completed before this call.
    pub(crate) fn commit(mut self) -> (CommittedDelta, FenceTimeline, Option<crate::GpuError>) {
        self.transaction.commit();
        (
            self.committed.take().expect("prepared dispatch owns its committed delta"),
            self.timeline
                .take()
                .expect("prepared dispatch owns its timeline"),
            self.refusal.take(),
        )
    }
}

pub(crate) fn prepare<'a>(
    resources: &'a mut SessionResources,
    timeline: &FenceTimeline,
    clock: &dyn Clock,
    exec: &mut dyn GpuExecutor,
    batch: &[Cmd],
) -> Result<PreparedDispatch<'a>> {
    // Reflect the batch's fence lifecycle + completion signals onto a COPY of the timeline first. A signal
    // that moves a fence backwards is a typed rejection, and raising it after the executor already applied
    // (and committed) the batch would leave resources live on the executor that `runtime::submit` has just
    // un-charged from the ledger — an accounting divergence a guest could repeat to grow past its residency
    // bound. Pre-flighting makes the rejection happen before ANY mutation; the copy is then installed
    // wholesale once the executor has accepted the work, so the timeline moves exactly when the frame does.
    let now = clock.now_nanos();
    let mut next_timeline = timeline.clone();
    for cmd in batch {
        match cmd {
            Cmd::CreateFence(id) => next_timeline.register(*id),
            Cmd::DestroyFence(id) => next_timeline.retire(*id),
            Cmd::Submit(cb) => {
                if let Some((fence, value)) = cb.signal {
                    next_timeline.signal(fence, value, now)?;
                }
            }
            _ => {}
        }
    }

    // SCOPE OF THE TRANSACTION — the contract, stated exactly:
    //
    //   A fatal batch leaves the ID LIFECYCLE and RESIDENCY LEDGER precisely as they were.
    //   A partial execution commits lifecycle, ledger and contents, then reports its refusal.
    //   Resource CONTENTS are never transactional.
    //
    // The table journal reverts inserts and removes, so a NACKed frame's creates disappear and its destroys
    // come back — that is what lets a connection retry. It cannot revert a write made THROUGH a handle that
    // survives the rollback: if a batch clears a texture and a LATER command in the same batch fails, the
    // clear stays. Verified, not assumed: a `[Submit{clear T}, CreateBuffer(live id)]` batch NACKs on the
    // duplicate id and leaves T cleared.
    //
    // This is the honest intersection rather than a gap to close. A copy-on-write snapshot of every mutated
    // resource per batch is the only way to make contents transactional, and it is unaffordable at
    // browser frame rates; more decisively, the wgpu executor cannot roll back GPU memory at all, so a
    // promise of content atomicity would be one the real executor could never keep. Executors narrow the
    // window instead by validating a whole command buffer before mutating anything (see the CPU executor's
    // `EncoderState::validate`), which is why a single `Submit` is content-atomic even though a
    // multi-command batch is not.
    //
    // Execute inside an all-tables transaction so a FATAL executor failure cannot leave lifecycle state
    // half-mutated. Nonfatal per-operation refusal is an explicit `Execution::partial`: the executor has
    // continued through the batch and the runtime commits the transaction and its already-installed
    // ledger. Fatal `Err` rolls tables and ledger back to the exact pre-frame state.
    // The transaction is scoped by an RAII guard rather than by matching on the result, because an
    // executor can leave this stage in a THIRD way that the match could not see: by PANICKING. A panic
    // unwinds straight past a rollback written on the error arm, so every mutation the batch had already
    // applied stayed applied, with the transaction still open and no owner aware of it — the id the
    // aborted batch created remained allocated forever, and every later batch reusing it was refused,
    // which a guest reads as an unrecoverable device. That was measured, not theorised: see
    // `tests/panic_atomicity.rs`, whose arm A is exactly this leak and whose arm B shows the same panic
    // costs nothing when the aborted batch had allocated nothing — which is why the same defect presented
    // as "cost a thread" on one run and "killed the session" on another.
    //
    // `Drop` runs during unwinding, so the guard restores the pre-frame tables on the panic path too.
    let transaction = ResourceTransaction::begin(resources);
    let executed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exec.execute(transaction.resources, batch)
    }));
    let execution = match executed {
        Ok(Ok(execution)) => execution,
        Ok(Err(error)) => {
            return Err(error);
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_owned());
            return Err(crate::GpuError::Panicked(message));
        }
    };
    let (committed, refusal) = execution.into_parts(batch);
    if refusal.is_some() {
        next_timeline = timeline.clone();
        for entry in &committed.commands {
            match &entry.command {
                Cmd::CreateFence(id) => next_timeline.register(*id),
                Cmd::DestroyFence(id) => next_timeline.retire(*id),
                _ => {}
            }
        }
        for &(fence, value) in &committed.fence_signals {
            next_timeline.signal(fence, value, now).expect("committed signal was preflighted");
        }
    }
    Ok(PreparedDispatch {
        transaction,
        timeline: Some(next_timeline),
        committed: Some(committed),
        refusal,
    })
}

pub fn export_buffer(
    session: &mut Session,
    exec: &dyn GpuExecutor,
    id: BufferId,
) -> Result<ExportId> {
    let exports = session
        .exports
        .as_ref()
        .ok_or(crate::GpuError::Unsupported("sharing registry"))?;
    if let Some(sharing) = session.buffer_sharing.get(&id.0) {
        return match sharing {
            ResourceSharing::Owner(export) => Ok(*export),
            ResourceSharing::Importer(_) => Err(crate::GpuError::Invalid(
                "an imported buffer cannot be exported by its importer",
            )),
        };
    }
    let (native, bytes) = exec.export_buffer(&session.resources, id)?;
    let export = exports.export_accounted(
        ResourceKey {
            session: session.id,
            kind: BufferId::KIND,
            id: id.0,
        },
        native,
        bytes,
        session.account.clone(),
        &session.global,
    )?;
    let installed = (|| {
        let guard = exports.access(session.id, export)?;
        session.resources.buffers.set_guard(id.0, guard)
    })();
    if let Err(error) = installed {
        exports.abort_export(session.id, export);
        return Err(error);
    }
    session
        .buffer_sharing
        .insert(id.0, ResourceSharing::Owner(export));
    Ok(export)
}

pub fn import_buffer(
    session: &mut Session,
    exec: &dyn GpuExecutor,
    id: BufferId,
    export: ExportId,
) -> Result<u64> {
    let exports = session
        .exports
        .as_ref()
        .ok_or(crate::GpuError::Unsupported("sharing registry"))?
        .clone();
    if session.resources.buffers.contains(id.0) {
        return Err(crate::GpuError::DuplicateId {
            kind: BufferId::KIND,
            id: id.0,
        });
    }
    let plan =
        exports.prepare_import(session.id, export, session.account.clone(), &session.global)?;
    let shared = plan.resource();
    let bytes = plan.bytes();
    if let Err(error) = session.account.reserve(
        export.0,
        bytes,
        session.limits.max_connection_bytes,
        session.limits.max_connection_objects,
    ) {
        session.account.discard_reservation(export.0);
        return Err(error);
    }
    let constructed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let guard = plan.access();
        let native = exec.import_buffer(shared, bytes)?;
        session
            .resources
            .buffers
            .insert_guarded(id.0, native, guard)?;
        session
            .buffer_sharing
            .insert(id.0, ResourceSharing::Importer(export));
        plan.commit(id.0)?;
        Ok(bytes)
    }));
    if !matches!(&constructed, Ok(Ok(_))) {
        session.buffer_sharing.remove(&id.0);
        session.resources.buffers.discard(id.0);
        session.account.discard_reservation(export.0);
    }
    match constructed {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

pub fn export_texture(
    session: &mut Session,
    exec: &dyn GpuExecutor,
    id: TextureId,
) -> Result<ExportId> {
    let exports = session.exports.as_ref().ok_or(crate::GpuError::Unsupported("sharing registry"))?;
    if let Some(sharing) = session.texture_sharing.get(&id.0) {
        return match sharing {
            ResourceSharing::Owner(export) => Ok(*export),
            ResourceSharing::Importer(_) => Err(crate::GpuError::Invalid("an imported texture cannot be exported by its importer")),
        };
    }
    let (native, bytes) = exec.export_texture(&session.resources, id)?;
    let export = exports.export_accounted(
        ResourceKey { session: session.id, kind: TextureId::KIND, id: id.0 },
        native,
        bytes,
        session.account.clone(),
        &session.global,
    )?;
    let installed = (|| {
        let guard = exports.access(session.id, export)?;
        session.resources.textures.set_guard(id.0, guard)
    })();
    if let Err(error) = installed {
        exports.abort_export(session.id, export);
        return Err(error);
    }
    session.texture_sharing.insert(id.0, ResourceSharing::Owner(export));
    Ok(export)
}

pub fn import_texture(
    session: &mut Session,
    exec: &dyn GpuExecutor,
    id: TextureId,
    export: ExportId,
) -> Result<u64> {
    let exports = session.exports.as_ref().ok_or(crate::GpuError::Unsupported("sharing registry"))?.clone();
    if session.resources.textures.contains(id.0) {
        return Err(crate::GpuError::DuplicateId { kind: TextureId::KIND, id: id.0 });
    }
    let plan = exports.prepare_import(session.id, export, session.account.clone(), &session.global)?;
    let shared = plan.resource();
    let bytes = plan.bytes();
    if let Err(error) = session.account.reserve(export.0, bytes, session.limits.max_connection_bytes, session.limits.max_connection_objects) {
        session.account.discard_reservation(export.0);
        return Err(error);
    }
    let constructed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let native = exec.import_texture(shared, bytes)?;
        session.resources.textures.insert_guarded(id.0, native, plan.access())?;
        session.texture_sharing.insert(id.0, ResourceSharing::Importer(export));
        plan.commit(id.0)?;
        Ok(bytes)
    }));
    if !matches!(&constructed, Ok(Ok(_))) {
        session.texture_sharing.remove(&id.0);
        session.resources.textures.discard(id.0);
        session.account.discard_reservation(export.0);
    }
    match constructed { Ok(result) => result, Err(payload) => std::panic::resume_unwind(payload) }
}

fn buffer_export(session: &Session, id: BufferId) -> Result<ExportId> {
    session
        .buffer_sharing
        .get(&id.0)
        .map(|sharing| match sharing {
            ResourceSharing::Owner(export) | ResourceSharing::Importer(export) => *export,
        })
        .ok_or(crate::GpuError::Invalid("buffer is not shared"))
}

pub fn map_buffer(session: &mut Session, exec: &mut dyn GpuExecutor, id: BufferId) -> Result<()> {
    let exports = session
        .exports
        .clone()
        .ok_or(crate::GpuError::Unsupported("sharing registry"))?;
    let _operation = exports.operation();
    let export = buffer_export(session, id)?;
    exec.sharing_barrier()?;
    exports.map(session.id, export)
}

pub fn unmap_buffer(session: &mut Session, exec: &mut dyn GpuExecutor, id: BufferId) -> Result<()> {
    let exports = session
        .exports
        .clone()
        .ok_or(crate::GpuError::Unsupported("sharing registry"))?;
    let _operation = exports.operation();
    let export = buffer_export(session, id)?;
    exec.sharing_barrier()?;
    exports.unmap(session.id, export)
}

fn texture_export(session: &Session, id: TextureId) -> Result<ExportId> {
    session.texture_sharing.get(&id.0).map(|sharing| match sharing {
        ResourceSharing::Owner(export) | ResourceSharing::Importer(export) => *export,
    }).ok_or(crate::GpuError::Invalid("texture is not shared"))
}

pub fn map_texture(session: &mut Session, exec: &mut dyn GpuExecutor, id: TextureId) -> Result<()> {
    let exports = session.exports.clone().ok_or(crate::GpuError::Unsupported("sharing registry"))?;
    let _operation = exports.operation();
    let export = texture_export(session, id)?;
    exec.sharing_barrier()?;
    exports.map(session.id, export)?;
    Ok(())
}

pub fn unmap_texture(session: &mut Session, exec: &mut dyn GpuExecutor, id: TextureId) -> Result<()> {
    let exports = session.exports.clone().ok_or(crate::GpuError::Unsupported("sharing registry"))?;
    let _operation = exports.operation();
    let export = texture_export(session, id)?;
    exec.sharing_barrier()?;
    exports.unmap(session.id, export)
}

/// Service the `CommandSink::wait` path: block on the executor until fence `fence` reaches `value`. Not
/// part of a command batch — an out-of-band wait the transport layer forwards.
pub fn wait(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    fence: FenceId,
    value: u64,
) -> Result<()> {
    exec.wait(&mut session.resources, fence, value)
}

#[cfg(test)]
mod sharing_atomicity_tests {
    use super::*;
    use crate::protocol::model::capability::Capabilities;
    use crate::runtime::model::sharing::{ExportId, Exports, Shared};
    use crate::{FakeClock, GlobalLedger, Limits};
    use std::sync::Arc;

    struct LyingExporter;
    struct FailingImporter;
    struct PanickingImporter;
    struct FailingBarrier;

    impl GpuExecutor for LyingExporter {
        fn capabilities(&self) -> Capabilities {
            Capabilities::permissive_fixture("lying exporter")
        }
        fn execute(&mut self, _: &mut SessionResources, _: &[Cmd]) -> Result<crate::Execution> {
            Ok(crate::Execution::accepted(Vec::new()))
        }
        fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> {
            Ok(())
        }
        fn export_buffer(&self, _: &SessionResources, _: BufferId) -> Result<(Shared, u64)> {
            Ok((Arc::new(17u32), 4))
        }
    }

    impl GpuExecutor for FailingImporter {
        fn capabilities(&self) -> Capabilities {
            Capabilities::permissive_fixture("failing importer")
        }
        fn execute(&mut self, _: &mut SessionResources, _: &[Cmd]) -> Result<crate::Execution> {
            Ok(crate::Execution::accepted(Vec::new()))
        }
        fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> {
            Ok(())
        }
        fn import_buffer(
            &self,
            _: Shared,
            _: u64,
        ) -> Result<crate::runtime::model::resources::Native> {
            Err(crate::GpuError::Unsupported(
                "injected native import failure",
            ))
        }
    }

    impl GpuExecutor for PanickingImporter {
        fn capabilities(&self) -> Capabilities {
            Capabilities::permissive_fixture("panicking importer")
        }
        fn execute(&mut self, _: &mut SessionResources, _: &[Cmd]) -> Result<crate::Execution> {
            Ok(crate::Execution::accepted(Vec::new()))
        }
        fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> {
            Ok(())
        }
        fn import_buffer(
            &self,
            _: Shared,
            _: u64,
        ) -> Result<crate::runtime::model::resources::Native> {
            panic!("injected native import panic")
        }
    }

    impl GpuExecutor for FailingBarrier {
        fn capabilities(&self) -> Capabilities {
            Capabilities::permissive_fixture("failing barrier")
        }
        fn execute(&mut self, _: &mut SessionResources, _: &[Cmd]) -> Result<crate::Execution> {
            Ok(crate::Execution::accepted(Vec::new()))
        }
        fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> {
            Ok(())
        }
        fn sharing_barrier(&mut self) -> Result<()> {
            Err(crate::GpuError::Unsupported(
                "injected sharing barrier failure",
            ))
        }
    }

    #[test]
    fn first_map_barrier_failure_never_takes_the_exclusive_claim() {
        let exports = Exports::new();
        let global = GlobalLedger::unbounded();
        let mut exec = FailingBarrier;
        let mut session = Session::new(
            Limits::from_capabilities(exec.capabilities()),
            global.clone(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone());
        let export = exports
            .export_accounted(
                crate::runtime::model::sharing::ResourceKey {
                    session: session.id,
                    kind: BufferId::KIND,
                    id: 7,
                },
                Arc::new(17u32),
                4,
                session.account.clone(),
                &global,
            )
            .unwrap();
        session
            .buffer_sharing
            .insert(7, ResourceSharing::Owner(export));

        assert!(map_buffer(&mut session, &mut exec, BufferId(7)).is_err());
        exports
            .map(session.id, export)
            .expect("failed pre-map barrier must leave the resource unmapped");
        exports.unmap(session.id, export).unwrap();
    }

    #[test]
    fn first_texture_map_barrier_failure_never_takes_the_exclusive_claim() {
        let exports = Exports::new();
        let global = GlobalLedger::unbounded();
        let mut exec = FailingBarrier;
        let mut session = Session::new(
            Limits::from_capabilities(exec.capabilities()),
            global.clone(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone());
        let export = exports
            .export_accounted(
                crate::runtime::model::sharing::ResourceKey {
                    session: session.id,
                    kind: TextureId::KIND,
                    id: 9,
                },
                Arc::new(17u32),
                4,
                session.account.clone(),
                &global,
            )
            .unwrap();
        session
            .texture_sharing
            .insert(9, ResourceSharing::Owner(export));

        assert!(map_texture(&mut session, &mut exec, TextureId(9)).is_err());
        exports
            .map(session.id, export)
            .expect("failed pre-map barrier must leave the texture unmapped");
        exports.unmap(session.id, export).unwrap();
    }

    #[test]
    fn failed_guard_install_rolls_back_the_zero_import_export_and_allows_retry() {
        let exec = LyingExporter;
        let exports = Exports::new();
        let mut session = Session::new(
            Limits::from_capabilities(exec.capabilities()),
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone());
        for expected in [ExportId(1), ExportId(2)] {
            assert!(export_buffer(&mut session, &exec, BufferId(9)).is_err());
            assert!(
                !exports.is_live(expected),
                "failed export must not leave a token reachable"
            );
        }
    }

    #[test]
    fn native_import_failure_never_publishes_an_importer_or_payer() {
        let exports = Exports::new();
        let global = GlobalLedger::unbounded();
        let owner_account = crate::runtime::model::resources::Account::new();
        let export = exports
            .export_accounted(
                crate::runtime::model::sharing::ResourceKey {
                    session: crate::runtime::model::sharing::SessionId(40),
                    kind: BufferId::KIND,
                    id: 1,
                },
                Arc::new(17u32),
                4,
                owner_account,
                &global,
            )
            .unwrap();
        let exec = FailingImporter;
        let mut importer = Session::new(
            Limits::from_capabilities(exec.capabilities()),
            global.clone(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone());
        assert!(import_buffer(&mut importer, &exec, BufferId(9), export).is_err());
        assert!(!importer.resources.buffers.contains(9));
        assert_eq!(importer.account.reserved_bytes(), 0);
        let release = exports
            .prepare_owner_release(crate::runtime::model::sharing::SessionId(40), export)
            .unwrap();
        release.commit();
        assert!(
            !exports.is_live(export),
            "failed import was never published as a retained payer"
        );
    }

    #[test]
    fn native_import_panic_unwinds_every_prepublication_effect() {
        let exports = Exports::new();
        let global = GlobalLedger::unbounded();
        let owner = crate::runtime::model::sharing::SessionId(41);
        let export = exports
            .export_accounted(
                crate::runtime::model::sharing::ResourceKey {
                    session: owner,
                    kind: BufferId::KIND,
                    id: 1,
                },
                Arc::new(17u32),
                4,
                crate::runtime::model::resources::Account::new(),
                &global,
            )
            .unwrap();
        let exec = PanickingImporter;
        let mut importer = Session::new(
            Limits::from_capabilities(exec.capabilities()),
            global.clone(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone());

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = import_buffer(&mut importer, &exec, BufferId(9), export);
        }));
        assert!(
            panic.is_err(),
            "the executor panic must remain observable after cleanup"
        );
        assert!(!importer.resources.buffers.contains(9));
        assert!(!importer.buffer_sharing.contains_key(&9));
        assert_eq!(importer.account.reserved_bytes(), 0);

        let release = exports.prepare_owner_release(owner, export).unwrap();
        release.commit();
        assert!(
            !exports.is_live(export),
            "the panicking import never retained a registry reference"
        );
    }
}

pub fn poll_fence(
    session: &Session,
    exec: &mut dyn GpuExecutor,
    fence: FenceId,
    value: u64,
) -> Result<bool> {
    exec.poll_fence(&session.resources, fence, value)
}

pub fn wait_timeout(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    fence: FenceId,
    value: u64,
    timeout_ns: u64,
) -> Result<crate::FenceWait> {
    exec.wait_timeout(&mut session.resources, fence, value, timeout_ns)
}

/// Service the device→host readback path: return `len` bytes of buffer `id` at `offset` from the executor
/// over the runtime-owned resources. Not part of a command batch — an out-of-band query the transport layer
/// forwards to answer a `CommandSink::read_buffer` / `cuMemcpyDtoH`.
pub fn read_buffer(
    session: &Session,
    exec: &dyn GpuExecutor,
    id: BufferId,
    offset: u64,
    len: usize,
) -> Result<Vec<u8>> {
    exec.read_buffer(&session.resources, id, offset, len)
}
