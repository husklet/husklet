use super::*;
impl Exports {
    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_fail_next_release(&self, point: ReleaseFailpoint) {
        let _ = self;
        RELEASE_FAILPOINT.with(|armed| armed.set(point as u8));
    }

    fn bind_global(&self, global: &GlobalLedger) -> Result<()> {
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = &registry.global {
            if !existing.same_authority(global) {
                return Err(GpuError::Invalid(
                    "sharing registry reused with different global authority",
                ));
            }
        } else {
            registry.global = Some(global.clone());
        }
        Ok(())
    }
    pub(super) fn release_unused_authority(&self, session: SessionId) {
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if !registry.has_authority_claim(session) {
            registry.authorities.remove(&session);
        }
    }

    fn bind_authority(&self, session: SessionId, account: &Account) -> Result<()> {
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = registry.authorities.get(&session) {
            if !existing.same_authority(account) {
                return Err(GpuError::Invalid(
                    "session id reused with different account authority",
                ));
            }
        } else {
            account.bind_session(session)?;
            registry.authorities.insert(session, account.clone());
        }
        Ok(())
    }
    pub(super) fn begin_commit(&self, id: ExportId, token: u64) -> Option<CommitLease> {
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let Some(pending) = registry
            .entries
            .get_mut(&id)
            .and_then(|entry| entry.pending.as_mut())
        else {
            return None;
        };
        if pending.token != token || pending.phase != TransitionPhase::Prepared {
            return None;
        }
        pending.phase = TransitionPhase::Committing;
        Some(CommitLease {
            exports: self.clone(),
            id,
            token,
            irreversible: false,
            complete: false,
        })
    }

    pub(super) fn cancel_prepared(&self, id: ExportId, token: u64) -> bool {
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let cancelled = registry.entries.get_mut(&id).is_some_and(|entry| {
            if entry.pending.is_some_and(|pending| {
                pending.token == token && pending.phase == TransitionPhase::Prepared
            }) {
                entry.pending = None;
                true
            } else {
                false
            }
        });
        drop(registry);
        if cancelled {
            self.changed.notify_all();
        }
        cancelled
    }

    /// Wait at most `timeout` for a prepared transition. At the deadline a still-prepared lease is
    /// cancelled atomically; a committing lease has crossed the point of no return and is waited out.
    pub fn settle_transition(&self, id: ExportId, timeout: std::time::Duration) {
        let deadline = std::time::Instant::now() + timeout;
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            let pending = registry.entries.get(&id).and_then(|entry| entry.pending);
            let Some(pending) = pending else {
                return;
            };
            let now = std::time::Instant::now();
            if pending.phase == TransitionPhase::Prepared && now >= deadline {
                if let Some(entry) = registry.entries.get_mut(&id) {
                    if entry.pending.is_some_and(|current| {
                        current.token == pending.token && current.phase == TransitionPhase::Prepared
                    }) {
                        entry.pending = None;
                    }
                }
                drop(registry);
                self.changed.notify_all();
                return;
            }
            let wait = if now < deadline {
                deadline - now
            } else {
                std::time::Duration::from_millis(1)
            };
            let (next, _) = self
                .changed
                .wait_timeout(registry, wait)
                .unwrap_or_else(|error| error.into_inner());
            registry = next;
        }
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a resource for import. Idempotent: re-exporting an already-exported resource returns the
    /// existing [`ExportId`] rather than minting a second entry for it.
    fn export_inner(&self, key: ResourceKey, resource: Shared, bytes: u64) -> Result<ExportId> {
        // The sibling creation paths refuse a zero or absurd size; the export path must not become the
        // one that does not. That exact asymmetry was found and fixed in `hl-vulkan`.
        if bytes == 0 {
            return Err(GpuError::Invalid("export: a zero-length resource"));
        }
        let mut registry = self.inner.lock().unwrap();
        if let Some(existing) = registry.by_resource.get(&key) {
            return Ok(*existing);
        }
        registry.next += 1;
        let id = ExportId(registry.next);
        registry.entries.insert(
            id,
            Entry {
                key,
                resource,
                bytes,
                importers: Vec::new(),
                party_access: HashMap::from([(key.session, Arc::new(AtomicBool::new(true)))]),
                accounts: HashMap::new(),
                owner_account: None,
                state: StateCell::default(),
                owner_released: false,
                pending: None,
                payer: None,
            },
        );
        registry.by_resource.insert(key, id);
        Ok(id)
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn export(&self, key: ResourceKey, resource: Shared, bytes: u64) -> Result<ExportId> {
        self.export_inner(key, resource, bytes)
    }

    pub(crate) fn export_accounted(
        &self,
        key: ResourceKey,
        resource: Shared,
        bytes: u64,
        account: Account,
        global: &GlobalLedger,
    ) -> Result<ExportId> {
        self.bind_global(global)?;
        self.bind_authority(key.session, &account)?;
        let id = match self.export_inner(key, resource, bytes) {
            Ok(id) => id,
            Err(error) => {
                self.release_unused_authority(key.session);
                return Err(error);
            }
        };
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("export disappeared"))?;
        if entry.owner_account.is_none() {
            entry.owner_account = Some(account);
        }
        Ok(id)
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_export_accounted(
        &self,
        key: ResourceKey,
        resource: Shared,
        bytes: u64,
        account: Account,
    ) -> Result<ExportId> {
        let global = self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .global
            .clone()
            .unwrap_or_else(GlobalLedger::unbounded);
        self.export_accounted(key, resource, bytes, account, &global)
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_export_accounted_with_global(
        &self,
        key: ResourceKey,
        resource: Shared,
        bytes: u64,
        account: Account,
        global: &GlobalLedger,
    ) -> Result<ExportId> {
        self.export_accounted(key, resource, bytes, account, global)
    }

    /// The exclusive-use guard on this export as seen from `session`'s own resource table.
    ///
    /// This is the bridge between the registry and [`ResourceTable::set_guard`]: the returned [`Access`]
    /// watches the SAME cell [`map`](Self::map)/[`unmap`](Self::unmap) write, so a claim taken here is
    /// visible to the resolution path with no further plumbing and no second copy of the state to drift.
    /// Only a party to the export gets one — a guard for a session that can never touch the resource
    /// would be a guard nothing consults.
    ///
    /// [`ResourceTable::set_guard`]: crate::protocol::model::id::ResourceTable::set_guard
    pub fn access(&self, session: SessionId, id: ExportId) -> Result<Access> {
        let registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get(&id)
            .ok_or(GpuError::Invalid("access: no such export"))?;
        if !entry.is_party(session) {
            return Err(GpuError::Invalid("access: not a party to this export"));
        }
        let active = entry
            .party_access
            .get(&session)
            .cloned()
            .ok_or(GpuError::Invalid("access: missing party authorization"))?;
        Ok(Access::new_revocable(
            Arc::clone(&entry.state),
            active,
            session.0,
        ))
    }

    /// Take a reference to an exported resource. Returns the native object and its authoritative length.
    ///
    /// Refuses a stale or unknown id with a typed error rather than a default — "could not reach the
    /// subject" must never be indistinguishable from "here is your buffer".
    #[cfg(all(debug_assertions, not(doc)))]
    pub fn import(&self, importer: SessionId, id: ExportId) -> Result<(Shared, u64)> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry.entries.get_mut(&id).ok_or(GpuError::Invalid(
            "import: no such export, or it is no longer live",
        ))?;
        if entry.pending.is_some() {
            return Err(GpuError::MappedElsewhere {
                kind: "shared buffer transition",
                id: id.0 as u32,
            });
        }
        if entry.key.session == importer {
            return Err(GpuError::Invalid(
                "import: a session cannot import its own export",
            ));
        }
        if entry.importers.contains(&importer) {
            return Err(GpuError::Invalid(
                "import: already imported by this session",
            ));
        }
        entry.importers.push(importer);
        entry
            .party_access
            .insert(importer, Arc::new(AtomicBool::new(true)));
        Ok((Arc::clone(&entry.resource), entry.bytes))
    }

    /// Reserve an import without publishing an importer reference. Owner release and other transitions
    /// are blocked by the lease until [`ImportPlan::commit`] atomically publishes the fully constructed
    /// alias, or plan Drop cancels it without ever creating a payer candidate.
    pub(crate) fn prepare_import(
        &self,
        importer: SessionId,
        id: ExportId,
        account: Account,
        global: &GlobalLedger,
    ) -> Result<ImportPlan> {
        self.bind_global(global)?;
        self.bind_authority(importer, &account)?;
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        registry.next_pending = registry.next_pending.wrapping_add(1).max(1);
        let token = registry.next_pending;
        let Some(entry) = registry.entries.get_mut(&id) else {
            drop(registry);
            self.release_unused_authority(importer);
            return Err(GpuError::Invalid(
                "import: no such export, or it is no longer live",
            ));
        };
        if entry.pending.is_some() {
            drop(registry);
            self.release_unused_authority(importer);
            return Err(GpuError::MappedElsewhere {
                kind: "shared buffer transition",
                id: id.0 as u32,
            });
        }
        if entry.key.session == importer {
            drop(registry);
            self.release_unused_authority(importer);
            return Err(GpuError::Invalid(
                "import: a session cannot import its own export",
            ));
        }
        if entry.importers.contains(&importer) {
            drop(registry);
            self.release_unused_authority(importer);
            return Err(GpuError::Invalid(
                "import: already imported by this session",
            ));
        }
        entry.pending = Some(Pending {
            token,
            phase: TransitionPhase::Prepared,
            authority: Some(importer),
        });
        let active = Arc::new(AtomicBool::new(true));
        Ok(ImportPlan {
            exports: self.clone(),
            id,
            token,
            importer,
            resource: Arc::clone(&entry.resource),
            bytes: entry.bytes,
            state: Arc::clone(&entry.state),
            committed: false,
            account,
            active,
        })
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_prepare_import(
        &self,
        importer: SessionId,
        id: ExportId,
        account: Account,
    ) -> Result<DebugImportPlan> {
        let global = self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .global
            .clone()
            .ok_or(GpuError::Invalid(
                "sharing registry has no global authority",
            ))?;
        self.prepare_import(importer, id, account, &global)
            .map(DebugImportPlan)
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_prepare_import_with_global(
        &self,
        importer: SessionId,
        id: ExportId,
        account: Account,
        global: &GlobalLedger,
    ) -> Result<DebugImportPlan> {
        self.prepare_import(importer, id, account, global)
            .map(DebugImportPlan)
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn attach_import_account(
        &self,
        importer: SessionId,
        local_id: u32,
        id: ExportId,
        account: Account,
    ) -> Result<()> {
        self.bind_authority(importer, &account)?;
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("import disappeared"))?;
        if entry
            .owner_account
            .as_ref()
            .is_some_and(|owner| owner.same_authority(&account))
            || entry
                .accounts
                .values()
                .any(|(_, existing)| existing.same_authority(&account))
        {
            return Err(GpuError::Invalid(
                "account authority reused across sharing sessions",
            ));
        }
        if !entry.importers.contains(&importer) {
            return Err(GpuError::Invalid("account: not an importer"));
        }
        entry.accounts.insert(importer, (local_id, account));
        Ok(())
    }
}
