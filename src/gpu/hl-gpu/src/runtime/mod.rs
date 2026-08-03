//! `runtime` — the per-connection [`Session`] that owns mutable client state and drives an injected
//! [`GpuExecutor`]. It VALIDATES and ACCOUNTS a decoded command batch, then DISPATCHES it to the
//! executor, updating the fence timeline; it does **not** choose which executor exists and contains no
//! socket/GPU/platform code.
//!
//! Layering (v2 doctrine): [`model`] owns the state (session, resources, timeline), [`port`] owns the
//! boundary traits it drives through ([`GpuExecutor`], [`Clock`]), [`service`] owns the workflows
//! (negotiate → validate → account → dispatch), and this facade only re-exports + wires the fixed
//! pipeline. Built ON TOP of [`crate::protocol`]; it does not change it.
//!
//! Reading direction (§2): `… → protocol → transport → runtime → executor → PresentableImage → …`.

pub mod model;
pub mod port;
pub mod service;
pub mod sink;

pub use model::resources::{
    GlobalLedger, Ledger, Native, SessionResources, Totals, KIND_BIND_GROUP, KIND_BUFFER,
    KIND_EXTERNAL, KIND_FENCE, KIND_PIPELINE, KIND_SAMPLER, KIND_SHADER, KIND_SURFACE,
    KIND_TEXTURE,
};
pub use model::session::{Limits, Session};
pub use model::sharing::{ExportId, Exports};
pub use model::sync_sharing::{SharedSync, SyncExportId, SyncExports, TimelineSync, TimelineWait};
pub use model::timeline::{FenceState, FenceTimeline};
pub use port::clock::{Clock, FakeClock, SystemClock};
pub use port::executor::{CommittedCommand, CommittedDelta, Execution, GpuExecutor, Presentation};
pub use sink::InProcessCommandSink;

use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::Result;

/// Run one decoded frame through the fixed runtime pipeline: **validate → account → dispatch**. A failure
/// in an earlier stage short-circuits before any later stage runs (and before the executor is touched),
/// giving failure atomicity: a malformed or over-budget frame rejected at `validate` or `account` never
/// reaches the executor at all, and one the executor NACKs is rolled back.
///
/// The guarantee's exact scope — a rejected batch leaves the ID LIFECYCLE and the RESIDENCY LEDGER as they
/// were, while resource CONTENTS are not transactional — is stated with its reasoning in
/// [`service::dispatch::dispatch`]. Read it before relying on atomicity for anything a batch WROTE.
///
/// `negotiate` is a per-connection prelude (call it once at connect); `frame_bytes` is the encoded
/// size of the frame the batch decoded from (checked against the negotiated frame ceiling).
#[derive(Debug, PartialEq)]
pub struct SubmitOutcome {
    pub committed: CommittedDelta,
    pub refusal: Option<crate::GpuError>,
}

pub fn submit_outcome(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    frame_bytes: usize,
    batch: &[Cmd],
) -> Result<SubmitOutcome> {
    // Unshared sessions cannot race a sharing transition. Keeping them out of the
    // process-wide operation lock preserves parallel submission across ordinary
    // GL/Vulkan connections while shared sessions retain ordered ownership.
    let sharing_exports = (!session.buffer_sharing.is_empty() || !session.texture_sharing.is_empty())
        .then(|| session.exports.clone())
        .flatten();
    let _sharing_operation = sharing_exports.as_ref().map(Exports::operation);
    let account = session.account.clone();
    let account_operation = account.operation();
    service::validate::validate(&session.limits, frame_bytes, batch)?;
    let mut retained = std::collections::HashSet::new();
    let mut releases = Vec::new();
    let mut import_releases = Vec::new();
    let mut texture_releases = Vec::new();
    let mut texture_import_releases = Vec::new();
    if let Some(exports) = session.exports.clone() {
        let mut sharing = session.buffer_sharing.clone();
        let mut texture_sharing = session.texture_sharing.clone();
        for (index, command) in batch.iter().enumerate() {
            if let Cmd::DestroyBuffer(id) = command {
                match sharing.remove(id) {
                    Some(model::session::ResourceSharing::Owner(export)) => {
                        let plan = exports.prepare_owner_release(session.id, export)?;
                        retained.insert(index);
                        releases.push((index, *id, plan));
                    }
                    Some(model::session::ResourceSharing::Importer(export)) => {
                        let plan = exports.prepare_import_release(session.id, export)?;
                        if plan.retains_global_charge() {
                            retained.insert(index);
                        }
                        import_releases.push((index, *id, plan));
                    }
                    None => {}
                }
            }
            if let Cmd::DestroyTexture(id) = command {
                match texture_sharing.remove(id) {
                    Some(model::session::ResourceSharing::Owner(export)) => {
                        let plan = exports.prepare_owner_release(session.id, export)?;
                        retained.insert(index);
                        texture_releases.push((index, *id, plan));
                    }
                    Some(model::session::ResourceSharing::Importer(export)) => {
                        let plan = exports.prepare_import_release(session.id, export)?;
                        if plan.retains_global_charge() { retained.insert(index); }
                        texture_import_releases.push((index, *id, plan));
                    }
                    None => {}
                }
            }
        }
    }
    // Snapshot the residency ledger BEFORE the charge so a later executor NACK can undo it. `account`
    // commits the whole frame's charge up front (both the connection ledger and its slice of the shared
    // global account); `dispatch` then hands the batch to the executor, which can still NACK it. Without
    // this rollback the charge would stick even though nothing rendered, so the connection's residency
    // would climb until every frame trips the cap — exactly the "NACK never recovers" failure.
    let ledger_before = session.account.ledger();
    session.charge_frame_skipping(batch, &retained)?;
    // Dispatch across an unwind boundary so the charge is rolled back on EVERY abnormal exit, not only on
    // a clean NACK. An executor that panics unwinds past the error arm below, and the charge it left
    // committed was the WHOLE frame's — including the commands that never ran, because `charge_frame`
    // commits up front (deliberately: that is what refuses an over-budget frame BEFORE anything is
    // allocated). Worse, the charge carries the ledger's own `live` id map, so a leaked charge refuses
    // every later create of those ids with `DuplicateId` even once the resource tables have been rolled
    // back correctly. That is the leak `tests/panic_atomicity.rs` arm A measures; it is a SEPARATE death
    // from the id-table leak that `dispatch` now guards, and it is the one that is actually observable,
    // because the account rejects the retry before the tables are ever consulted.
    //
    // A panic is then REFUSED rather than re-raised, which makes the outcome of a backend defect
    // deterministic instead of depending on who happens to be above it on the stack. The socket transport
    // has its own unwind boundary and would have NACKed the frame anyway; an in-process caller
    // (`InProcessCommandSink`, the CPU oracle harnesses, every test) had none, so the same defect either
    // NACKed one frame or killed the caller depending only on the deployment. Converting here — at the
    // stage that already owns "the frame either happened or it did not" — gives every caller the same
    // answer. The typed [`GpuError::Panicked`] keeps the CAUSE legible, which the ack byte alone cannot.
    //
    // Scope: this wraps `dispatch`, i.e. EXECUTOR work. Validation and accounting run before it and are
    // deliberately outside, so a panic in the runtime's own logic still surfaces as a panic.
    let prepared = match service::dispatch::prepare(
        &mut session.resources,
        &session.timeline,
        session.clock.as_ref(),
        exec,
        batch,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            session
                .account
                .restore_ledger(ledger_before.clone(), &session.global);
            return Err(error);
        }
    };
    // Once an abnormal abort is an ordinary `Err`, it takes the SAME rollback path as every other
    // rejection — one place that restores the account, reached by both causes.
    {
            // No fallible work may follow this point. The prepared dispatch still owns the open resource
            // transaction; committing it and installing the already-preflighted timeline cannot fail.
            let (committed, timeline, refusal) = prepared.commit();
            let buffer_replacements = committed_buffer_replacements(&committed);
            let texture_replacements = committed_texture_replacements(&committed);
            if refusal.is_some() {
                session.account.restore_ledger(ledger_before.clone(), &session.global);
                let committed_sources: std::collections::HashSet<usize> = committed.sources.iter().copied().collect();
                session
                    .charge_frame_selected(batch, &retained, Some(&committed_sources))
                    .expect("committed delta was validated and precharged as part of the full batch");
            }
            drop(account_operation);
            for (index, id, mut release) in releases {
                if !committed.contains_source(index) { continue; }
                if buffer_replacements.contains(&index) { release = release.preserve_owner_id(); }
                release.commit();
                session.buffer_sharing.remove(&id);
            }
            for (index, id, mut release) in import_releases {
                if !committed.contains_source(index) { continue; }
                if buffer_replacements.contains(&index) { release = release.preserve_importer_id(); }
                release.commit();
                session.buffer_sharing.remove(&id);
            }
            for (index, id, mut release) in texture_releases {
                if !committed.contains_source(index) { continue; }
                if texture_replacements.contains(&index) { release = release.preserve_owner_id(); }
                release.commit();
                session.texture_sharing.remove(&id);
            }
            for (index, id, mut release) in texture_import_releases {
                if !committed.contains_source(index) { continue; }
                if texture_replacements.contains(&index) { release = release.preserve_importer_id(); }
                release.commit();
                session.texture_sharing.remove(&id);
            }
            session.timeline = timeline;
            Ok(SubmitOutcome { committed, refusal })
    }
}

pub fn submit(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    frame_bytes: usize,
    batch: &[Cmd],
) -> Result<Vec<Presentation>> {
    let outcome = submit_outcome(session, exec, frame_bytes, batch)?;
    match outcome.refusal {
        Some(error) => Err(crate::GpuError::Partial(Box::new(error))),
        None => Ok(outcome.committed.presentations),
    }
}

fn committed_buffer_replacements(committed: &CommittedDelta) -> std::collections::HashSet<usize> {
    let mut live = std::collections::HashSet::new();
    let mut replacements = std::collections::HashSet::new();
    for entry in committed.commands.iter().rev() {
        let index = entry.source;
        let command = &entry.command;
        match command {
            Cmd::CreateBuffer(id, _) => {
                live.insert(*id);
            }
            Cmd::DestroyBuffer(id) => {
                if live.contains(id) {
                    replacements.insert(index);
                }
                live.remove(id);
            }
            _ => {}
        }
    }
    replacements
}

fn committed_texture_replacements(committed: &CommittedDelta) -> std::collections::HashSet<usize> {
    let mut live = std::collections::HashSet::new();
    let mut replacements = std::collections::HashSet::new();
    for entry in committed.commands.iter().rev() {
        let index = entry.source;
        let command = &entry.command;
        match command {
            Cmd::CreateTexture(id, _) => { live.insert(*id); }
            Cmd::DestroyTexture(id) => {
                if live.contains(id) { replacements.insert(index); }
                live.remove(id);
            }
            _ => {}
        }
    }
    replacements
}

#[cfg(test)]
mod lifecycle_scan_tests {
    use super::*;
    use crate::protocol::model::descriptor::BufferDesc;

    #[test]
    fn future_replacements_scale_linearly_across_large_repeated_churn() {
        let desc = BufferDesc {
            size: 4,
            usage: 0,
            label: String::new(),
        };
        let mut batch = Vec::with_capacity(20_001);
        batch.push(Cmd::CreateBuffer(7, desc.clone()));
        for _ in 0..10_000 {
            batch.push(Cmd::DestroyBuffer(7));
            batch.push(Cmd::CreateBuffer(7, desc.clone()));
        }
        let batch_len = batch.len();
        let committed = CommittedDelta {
            commands: batch.into_iter().enumerate().map(|(source, command)| CommittedCommand { source, command }).collect(),
            fence_signals: Vec::new(),
            presentations: Vec::new(),
            replayable: true,
            sources: (0..batch_len).collect(),
        };
        let replacements = committed_buffer_replacements(&committed);
        assert_eq!(replacements.len(), 10_000);
        assert!((1..batch_len)
            .step_by(2)
            .all(|index| replacements.contains(&index)));
    }
}
