//! `account` — transactional residency charge/release for a validated batch.
//!
//! Second stage of the runtime pipeline (decode → validate → **account** → dispatch). Every operation
//! here is all-or-nothing: a proposed next ledger is computed, checked against the per-connection +
//! compiled-cache + process-global ceilings, and committed atomically — a rejection leaves both the
//! connection [`Ledger`] and the shared [`GlobalLedger`] unchanged, so an over-budget frame never reaches
//! the executor with residency half-charged. Ported from `hl-gpu/src/limits.rs`'s `ExecutorBudget`
//! (`preflight` → [`charge_frame`], the external-alloc + ownership-transfer methods, and `commit`),
//! operating on a [`Session`]'s accounting state.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::{GpuError, Result};
use crate::runtime::model::resources::{ResourceKind, Totals, KIND_EXTERNAL, KIND_PIPELINE};
use crate::runtime::model::session::Session;

/// Charge a whole validated frame's creates (and refund its destroys) transactionally against the
/// connection. A `Create*` over a still-live id (one not destroyed earlier in this same frame) is a
/// `DuplicateId` — exactly what the executor's id table raises for it in `dispatch` — so it is rejected
/// HERE, before any ledger mutation, keeping the account/dispatch seam failure-atomic: account and
/// dispatch agree on rejection and the ledger never drifts on a create the executor will refuse. A legal
/// same-frame destroy-then-recreate is unaffected (the destroy clears the id from `next_live` first).
impl Session {
    pub fn charge_frame(&mut self, cmds: &[Cmd]) -> Result<()> {
        self.charge_frame_skipping(cmds, &HashSet::new())
    }

    pub(crate) fn charge_frame_skipping(
        &mut self,
        cmds: &[Cmd],
        retained: &HashSet<usize>,
    ) -> Result<()> {
        self.charge_frame_selected(cmds, retained, None)
    }

    pub(crate) fn charge_frame_selected(
        &mut self,
        cmds: &[Cmd],
        retained: &HashSet<usize>,
        accepted: Option<&[bool]>,
    ) -> Result<()> {
        let session = self;
        let current = session.account.ledger();
        let mut next_live = current.live.clone();
        let mut next = current.totals;
        for (index, cmd) in cmds.iter().enumerate() {
            if accepted.is_some_and(|mask| !mask[index]) {
                continue;
            }
            if let Some((kind, id, bytes)) = cmd.create_charge()? {
                if next_live.insert((kind, id), bytes).is_some() {
                    // The id is already live (from a prior frame, or an earlier create in this frame with no
                    // intervening destroy) — a duplicate create. The executor would reject it as DuplicateId;
                    // reject it here first so no residency is charged for a create that will never happen.
                    return Err(GpuError::DuplicateId {
                        kind: ResourceKind::from_code(kind).name(),
                        id,
                    });
                }
                next.bytes = next
                    .bytes
                    .checked_add(bytes)
                    .ok_or(GpuError::ResourceLimit("residency overflow"))?;
                next.objects = next
                    .objects
                    .checked_add(1)
                    .ok_or(GpuError::ResourceLimit("object count overflow"))?;
                if kind == KIND_PIPELINE {
                    next.compiled_bytes = next
                        .compiled_bytes
                        .checked_add(bytes)
                        .ok_or(GpuError::ResourceLimit("compiled cache overflow"))?;
                }
            } else if let Some((kind, id)) = cmd.destroy_key() {
                if retained.contains(&index) {
                    // The old shared allocation keeps its charge until the release plan commits, but the
                    // protocol id itself is free immediately. This preserves legal Destroy+Create of the
                    // same id: charge the replacement now, then the plan subtracts/transfers the old bytes.
                    next_live.remove(&(kind, id));
                    continue;
                }
                if let Some(bytes) = next_live.remove(&(kind, id)) {
                    next.bytes -= bytes;
                    next.objects -= 1;
                    if kind == KIND_PIPELINE {
                        next.compiled_bytes -= bytes;
                    }
                }
            }
        }
        commit(session, next_live, next)
    }

    /// Charge an external allocation (a dma-buf / IOSurface imported from the guest, not produced by the IR
    /// stream) against this connection. Transactional; a duplicate external id is a typed error.
    pub fn charge_external(session: &mut Session, id: u32, bytes: u64) -> Result<()> {
        let current = session.account.ledger();
        let mut next_live = current.live.clone();
        let mut next = current.totals;
        if next_live.insert((KIND_EXTERNAL, id), bytes).is_some() {
            return Err(GpuError::Invalid("duplicate external allocation id"));
        }
        next.bytes = next
            .bytes
            .checked_add(bytes)
            .ok_or(GpuError::ResourceLimit("residency overflow"))?;
        next.objects = next
            .objects
            .checked_add(1)
            .ok_or(GpuError::ResourceLimit("object count overflow"))?;
        commit(session, next_live, next)
    }

    /// Release a previously-charged external allocation. Errors if the id was never charged.
    pub fn release_external(&mut self, id: u32) -> Result<()> {
        let session = self;
        let current = session.account.ledger();
        let mut next_live = current.live.clone();
        let mut next = current.totals;
        let bytes = next_live
            .remove(&(KIND_EXTERNAL, id))
            .ok_or(GpuError::UnknownId {
                kind: "external allocation",
                id,
            })?;
        next.bytes -= bytes;
        next.objects -= 1;
        commit(session, next_live, next)
    }
}

/// Accept ownership of an object transferred INTO this connection, charging it under `(kind, id)`. A
/// pipeline is charged to the compiled-pipeline sub-total as well as the aggregate, exactly as
/// [`Session::charge_frame`] charges a created one — the accept must mirror what
/// [`release_ownership`] refunds, or the sub-total drifts on a round trip.
pub fn accept_ownership(session: &mut Session, kind: u8, id: u32, bytes: u64) -> Result<()> {
    let current = session.account.ledger();
    let mut next_live = current.live.clone();
    let mut next = current.totals;
    if next_live.insert((kind, id), bytes).is_some() {
        return Err(GpuError::Invalid("ownership transfer over a live id"));
    }
    next.bytes = next
        .bytes
        .checked_add(bytes)
        .ok_or(GpuError::ResourceLimit("residency overflow"))?;
    next.objects = next
        .objects
        .checked_add(1)
        .ok_or(GpuError::ResourceLimit("object count overflow"))?;
    if kind == KIND_PIPELINE {
        next.compiled_bytes = next
            .compiled_bytes
            .checked_add(bytes)
            .ok_or(GpuError::ResourceLimit("compiled cache overflow"))?;
    }
    commit(session, next_live, next)?;
    Ok(())
}

/// Transfer a live object OUT of this connection, returning the bytes released so the receiving
/// accountant can charge them. Errors if the object is not live here.
pub fn release_ownership(session: &mut Session, kind: u8, id: u32) -> Result<u64> {
    let current = session.account.ledger();
    let mut next_live = current.live.clone();
    let mut next = current.totals;
    let bytes = next_live.remove(&(kind, id)).ok_or(GpuError::UnknownId {
        kind: "owned object",
        id,
    })?;
    next.bytes -= bytes;
    next.objects -= 1;
    if kind == KIND_PIPELINE {
        next.compiled_bytes -= bytes;
    }
    commit(session, next_live, next)?;
    Ok(bytes)
}

/// Validate the proposed connection totals against the per-connection + compiled-cache + process-global
/// ceilings and commit them atomically. On any rejection neither the connection nor the global account
/// moves.
fn commit(session: &mut Session, next_live: HashMap<(u8, u32), u64>, next: Totals) -> Result<()> {
    if next.bytes > session.limits.max_connection_bytes
        || next.objects > session.limits.max_connection_objects
    {
        return Err(GpuError::ResourceLimit("connection residency"));
    }
    if next.compiled_bytes > session.limits.max_compiled_cache_bytes {
        return Err(GpuError::ResourceLimit("compiled cache"));
    }
    let ledger = crate::runtime::model::resources::Ledger {
        live: next_live,
        totals: next,
    };
    session.account.commit_ledger(
        session.account.ledger().totals,
        ledger.clone(),
        session.limits.max_connection_bytes,
        session.limits.max_connection_objects,
        &session.global,
    )?;
    Ok(())
}
