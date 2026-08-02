use super::*;
pub(crate) struct OwnerReleasePlan {
    exports: Exports,
    id: ExportId,
    pub(super) token: u64,
    owner: (SessionId, u32, Account),
    payer: Option<(SessionId, u32, Account)>,
    bytes: u64,
    global: GlobalLedger,
    preserve_owner_id: bool,
    committed: bool,
}

#[cfg(all(debug_assertions, not(doc)))]
pub struct DebugOwnerReleasePlan(OwnerReleasePlan);

#[cfg(all(debug_assertions, not(doc)))]
impl DebugOwnerReleasePlan {
    pub fn commit(self) {
        self.0.commit();
    }
}

pub(crate) struct ImportReleasePlan {
    exports: Exports,
    id: ExportId,
    token: u64,
    importer: SessionId,
    transfer: Option<((SessionId, u32, Account), (SessionId, u32, Account), u64)>,
    final_payer: Option<(u32, Account, u64)>,
    global: GlobalLedger,
    preserve_importer_id: bool,
    committed: bool,
}

#[cfg(all(debug_assertions, not(doc)))]
pub struct DebugImportReleasePlan(ImportReleasePlan);

#[cfg(all(debug_assertions, not(doc)))]
impl DebugImportReleasePlan {
    pub fn commit(self) {
        self.0.commit();
    }
}

pub(crate) struct ImportPlan {
    pub(super) exports: Exports,
    pub(super) id: ExportId,
    pub(super) token: u64,
    pub(super) importer: SessionId,
    pub(super) resource: Shared,
    pub(super) bytes: u64,
    pub(super) state: StateCell,
    pub(super) account: Account,
    pub(super) active: Arc<AtomicBool>,
    pub(super) committed: bool,
}

#[cfg(all(debug_assertions, not(doc)))]
pub struct DebugImportPlan(pub(super) ImportPlan);

#[cfg(all(debug_assertions, not(doc)))]
impl DebugImportPlan {
    pub fn resource(&self) -> Shared {
        self.0.resource()
    }
    pub fn access(&self) -> Access {
        self.0.access()
    }
}

impl ImportPlan {
    pub(crate) fn resource(&self) -> Shared {
        Arc::clone(&self.resource)
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn access(&self) -> Access {
        Access::new_revocable(
            Arc::clone(&self.state),
            Arc::clone(&self.active),
            self.importer.0,
        )
    }

    pub(crate) fn commit(mut self, local_id: u32) -> Result<()> {
        let mut registry = self
            .exports
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = registry
            .entries
            .get_mut(&self.id)
            .ok_or(GpuError::Invalid("import disappeared"))?;
        if !entry.pending.is_some_and(|pending| {
            pending.token == self.token && pending.phase == TransitionPhase::Prepared
        }) {
            return Err(GpuError::Invalid("import lease cancelled"));
        }
        entry.importers.push(self.importer);
        entry
            .party_access
            .insert(self.importer, Arc::clone(&self.active));
        entry
            .accounts
            .insert(self.importer, (local_id, self.account.clone()));
        entry.pending = None;
        drop(registry);
        self.exports.changed.notify_all();
        self.committed = true;
        Ok(())
    }
}

impl Drop for ImportPlan {
    fn drop(&mut self) {
        if !self.committed {
            self.exports.cancel_prepared(self.id, self.token);
            self.exports.release_unused_authority(self.importer);
        }
    }
}

impl ImportReleasePlan {
    pub fn retains_global_charge(&self) -> bool {
        self.transfer.is_some() || self.final_payer.is_some()
    }

    pub(crate) fn preserve_importer_id(mut self) -> Self {
        self.preserve_importer_id = true;
        self
    }

    pub(crate) fn commit(mut self) {
        let Some(mut lease) = self.exports.begin_commit(self.id, self.token) else {
            self.committed = true;
            return;
        };
        let account_transition = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some((from, to, bytes)) = &self.transfer {
                let (first, second, from_first) = if from.0 < to.0 {
                    (&from.2, &to.2, true)
                } else {
                    (&to.2, &from.2, false)
                };
                let _first_operation = first.operation();
                let _second_operation = second.operation();
                let mut a = first.inner.lock().unwrap_or_else(|e| e.into_inner());
                let mut b = second.inner.lock().unwrap_or_else(|e| e.into_inner());
                let (source, target) = if from_first {
                    (&mut a, &mut b)
                } else {
                    (&mut b, &mut a)
                };
                target.ledger.live.reserve(1);
                #[cfg(all(debug_assertions, not(doc)))]
                trip_release_failpoint(ReleaseFailpoint::PayerToNext);
                lease.irreversible();
                if !self.preserve_importer_id {
                    source.ledger.live.remove(&(KIND_BUFFER, from.1));
                }
                source.ledger.totals.bytes = source.ledger.totals.bytes.saturating_sub(*bytes);
                source.ledger.totals.objects = source.ledger.totals.objects.saturating_sub(1);
                target.reservations.remove(&self.id.0);
                target.ledger.live.insert((KIND_BUFFER, to.1), *bytes);
                target.ledger.totals.bytes = target.ledger.totals.bytes.saturating_add(*bytes);
                target.ledger.totals.objects = target.ledger.totals.objects.saturating_add(1);
            } else if let Some((local, account, bytes)) = &self.final_payer {
                let _operation = account.operation();
                let mut state = account
                    .inner
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let mut global = self.global.lock_totals();
                #[cfg(all(debug_assertions, not(doc)))]
                trip_release_failpoint(ReleaseFailpoint::FinalPayerRefund);
                lease.irreversible();
                if !self.preserve_importer_id {
                    state.ledger.live.remove(&(KIND_BUFFER, *local));
                }
                state.ledger.totals.bytes = state.ledger.totals.bytes.saturating_sub(*bytes);
                state.ledger.totals.objects = state.ledger.totals.objects.saturating_sub(1);
                global.bytes = global.bytes.saturating_sub(*bytes);
                global.objects = global.objects.saturating_sub(1);
            } else {
                #[cfg(all(debug_assertions, not(doc)))]
                trip_release_failpoint(ReleaseFailpoint::NonPayerRelease);
                lease.irreversible();
            }
        }));
        if let Err(payload) = account_transition {
            std::panic::resume_unwind(payload);
        }
        let mut registry = self.exports.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut collect = false;
        if let Some(entry) = registry.entries.get_mut(&self.id) {
            if self.transfer.is_none() {
                if let Some((_, account)) = entry.accounts.get(&self.importer) {
                    if entry.payer != Some(self.importer) {
                        account.discard_reservation(self.id.0);
                    }
                }
            }
            entry.accounts.remove(&self.importer);
            entry.importers.retain(|session| *session != self.importer);
            entry.revoke(self.importer);
            entry.payer = self.transfer.as_ref().map(|(_, to, _)| to.0);
            collect = entry.is_dead();
        }
        if collect {
            registry.collect(self.id);
        }
        if let Some(entry) = registry.entries.get_mut(&self.id) {
            if entry
                .pending
                .is_some_and(|pending| pending.token == self.token)
            {
                entry.pending = None;
            }
        }
        drop(registry);
        self.exports.release_unused_authority(self.importer);
        self.exports.changed.notify_all();
        lease.complete();
        self.committed = true;
    }
}

impl Drop for ImportReleasePlan {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.exports.cancel_prepared(self.id, self.token);
    }
}

impl OwnerReleasePlan {
    pub(crate) fn preserve_owner_id(mut self) -> Self {
        self.preserve_owner_id = true;
        self
    }
    /// Commit cannot fail: preparation reserved the registry entry and the importer already reserved the
    /// full charge. Account locks are acquired in SessionId order and no registry lock is held while waiting.
    pub(crate) fn commit(mut self) {
        let Some(mut lease) = self.exports.begin_commit(self.id, self.token) else {
            self.committed = true;
            return;
        };
        let account_transition = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(payer_info) = &self.payer {
                let (first, second, owner_first) = if self.owner.0 < payer_info.0 {
                    (&self.owner.2, &payer_info.2, true)
                } else {
                    (&payer_info.2, &self.owner.2, false)
                };
                let _first_operation = first.operation();
                let _second_operation = second.operation();
                let mut a = first.inner.lock().unwrap_or_else(|e| e.into_inner());
                let mut b = second.inner.lock().unwrap_or_else(|e| e.into_inner());
                let (owner, payer) = if owner_first {
                    (&mut a, &mut b)
                } else {
                    (&mut b, &mut a)
                };
                payer.ledger.live.reserve(1);
                #[cfg(all(debug_assertions, not(doc)))]
                trip_release_failpoint(ReleaseFailpoint::OwnerToPayer);
                lease.irreversible();
                if !self.preserve_owner_id {
                    owner.ledger.live.remove(&(KIND_BUFFER, self.owner.1));
                }
                owner.ledger.totals.bytes = owner.ledger.totals.bytes.saturating_sub(self.bytes);
                owner.ledger.totals.objects = owner.ledger.totals.objects.saturating_sub(1);
                payer.reservations.remove(&self.id.0);
                payer
                    .ledger
                    .live
                    .insert((KIND_BUFFER, payer_info.1), self.bytes);
                payer.ledger.totals.bytes = payer.ledger.totals.bytes.saturating_add(self.bytes);
                payer.ledger.totals.objects = payer.ledger.totals.objects.saturating_add(1);
            } else {
                let _operation = self.owner.2.operation();
                let mut owner = self
                    .owner
                    .2
                    .inner
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let mut global = self.global.lock_totals();
                #[cfg(all(debug_assertions, not(doc)))]
                trip_release_failpoint(ReleaseFailpoint::OwnerFinalRefund);
                lease.irreversible();
                if !self.preserve_owner_id {
                    owner.ledger.live.remove(&(KIND_BUFFER, self.owner.1));
                }
                owner.ledger.totals.bytes = owner.ledger.totals.bytes.saturating_sub(self.bytes);
                owner.ledger.totals.objects = owner.ledger.totals.objects.saturating_sub(1);
                global.bytes = global.bytes.saturating_sub(self.bytes);
                global.objects = global.objects.saturating_sub(1);
            }
        }));
        if let Err(payload) = account_transition {
            std::panic::resume_unwind(payload);
        }
        let mut registry = self
            .exports
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = registry.entries.get_mut(&self.id) {
            if entry.state() == MapState::MappedBy(entry.key.session) {
                entry.set_state(MapState::Unmapped);
            }
            entry.owner_released = true;
            entry.revoke(self.owner.0);
            entry.payer = self.payer.as_ref().map(|payer| payer.0);
            if entry
                .pending
                .is_some_and(|pending| pending.token == self.token)
            {
                entry.pending = None;
            }
            let key = entry.key;
            registry.by_resource.remove(&key);
        }
        registry.collect(self.id);
        drop(registry);
        self.exports.release_unused_authority(self.owner.0);
        self.exports.changed.notify_all();
        lease.complete();
        self.committed = true;
    }
}

impl Drop for OwnerReleasePlan {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.exports.cancel_prepared(self.id, self.token);
    }
}

impl Exports {
    pub(crate) fn prepare_owner_release(
        &self,
        owner: SessionId,
        id: ExportId,
    ) -> Result<OwnerReleasePlan> {
        let mut registry = self.inner.lock().unwrap();
        let global = registry.global.clone().ok_or(GpuError::Invalid(
            "sharing registry has no global authority",
        ))?;
        registry.next_pending = registry.next_pending.wrapping_add(1).max(1);
        let token = registry.next_pending;
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("release: no such export"))?;
        if entry.key.session != owner || entry.owner_released {
            return Err(GpuError::Invalid("release: not export owner"));
        }
        if entry.pending.is_some() {
            return Err(GpuError::MappedElsewhere {
                kind: "shared buffer transition",
                id: id.0 as u32,
            });
        }
        let owner_account = entry
            .owner_account
            .clone()
            .ok_or(GpuError::Invalid("release: missing owner account"))?;
        let payer = entry.accounts.keys().copied().min().and_then(|payer_id| {
            entry
                .accounts
                .get(&payer_id)
                .cloned()
                .map(|(local_id, account)| (payer_id, local_id, account))
        });
        entry.pending = Some(Pending {
            token,
            phase: TransitionPhase::Prepared,
            authority: None,
        });
        Ok(OwnerReleasePlan {
            exports: self.clone(),
            id,
            token,
            owner: (owner, entry.key.id, owner_account),
            payer,
            bytes: entry.bytes,
            global,
            preserve_owner_id: false,
            committed: false,
        })
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_prepare_owner_release(
        &self,
        owner: SessionId,
        id: ExportId,
    ) -> Result<DebugOwnerReleasePlan> {
        self.prepare_owner_release(owner, id)
            .map(DebugOwnerReleasePlan)
    }

    pub(crate) fn prepare_import_release(
        &self,
        importer: SessionId,
        id: ExportId,
    ) -> Result<ImportReleasePlan> {
        let mut registry = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let global = registry.global.clone().ok_or(GpuError::Invalid(
            "sharing registry has no global authority",
        ))?;
        registry.next_pending = registry.next_pending.wrapping_add(1).max(1);
        let token = registry.next_pending;
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("release: no such export"))?;
        if entry.pending.is_some() {
            return Err(GpuError::MappedElsewhere {
                kind: "shared buffer transition",
                id: id.0 as u32,
            });
        }
        let (local, account) = entry
            .accounts
            .get(&importer)
            .cloned()
            .ok_or(GpuError::Invalid("release: not importer"))?;
        let transfer = if entry.payer == Some(importer) {
            entry
                .accounts
                .iter()
                .filter(|(session, _)| **session != importer)
                .min_by_key(|(session, _)| **session)
                .map(|(session, (next_local, next_account))| {
                    (
                        (importer, local, account.clone()),
                        (*session, *next_local, next_account.clone()),
                        entry.bytes,
                    )
                })
        } else {
            None
        };
        let final_payer = if entry.payer == Some(importer) && transfer.is_none() {
            Some((local, account.clone(), entry.bytes))
        } else {
            None
        };
        entry.pending = Some(Pending {
            token,
            phase: TransitionPhase::Prepared,
            authority: None,
        });
        Ok(ImportReleasePlan {
            exports: self.clone(),
            id,
            token,
            importer,
            transfer,
            final_payer,
            global,
            preserve_importer_id: false,
            committed: false,
        })
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn prepare_import_release_for_test(
        &self,
        importer: SessionId,
        id: ExportId,
    ) -> Result<DebugImportReleasePlan> {
        self.prepare_import_release(importer, id)
            .map(DebugImportReleasePlan)
    }
}
