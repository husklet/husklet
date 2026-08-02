use super::*;
impl Exports {
    pub(crate) fn abort_export(&self, owner: SessionId, id: ExportId) {
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let removable = registry.entries.get(&id).is_some_and(|entry| {
            entry.key.session == owner && entry.importers.is_empty() && !entry.owner_released
        });
        if removable {
            if let Some(entry) = registry.entries.get_mut(&id) {
                entry.owner_released = true;
                entry.revoke(owner);
                let key = entry.key;
                registry.by_resource.remove(&key);
            }
            registry.collect(id);
        }
        drop(registry);
        self.release_unused_authority(owner);
    }

    /// Drop an importer's reference. Frees the resource if it was the last one and the owner had released.
    fn release_import_inner(&self, importer: SessionId, id: ExportId) -> Result<()> {
        let mut registry = self.inner.lock().unwrap();
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
        // Leaving "unregister while mapped" undefined is how a resource ends up permanently MappedBy a
        // session that has gone. It is defined here as an implicit unmap by the holder, and refused for
        // anyone else.
        if entry.state() == MapState::MappedBy(importer) {
            entry.set_state(MapState::Unmapped);
        }
        let before = entry.importers.len();
        entry.importers.retain(|s| *s != importer);
        entry.accounts.remove(&importer);
        entry.revoke(importer);
        if entry.importers.len() == before {
            return Err(GpuError::Invalid("release: not an importer of this export"));
        }
        registry.collect(id);
        drop(registry);
        self.release_unused_authority(importer);
        Ok(())
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn release_import(&self, importer: SessionId, id: ExportId) -> Result<()> {
        self.release_import_inner(importer, id)
    }

    /// The owner destroyed its id. The destroy SUCCEEDS and the storage is retained while importers
    /// remain; the charge moves to them.
    ///
    /// Refusing the destroy was considered and rejected — deleting a buffer is legal application
    /// behaviour and an application that gets an error there has no recourse. Silent retention was also
    /// rejected: that is an invisible leak.
    fn owner_release_inner(&self, id: ExportId) -> Result<()> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("owner release: no such export"))?;
        if entry.pending.is_some() {
            return Err(GpuError::MappedElsewhere {
                kind: "shared buffer transition",
                id: id.0 as u32,
            });
        }
        if entry.owner_released {
            return Err(GpuError::Invalid("owner release: owner already released"));
        }
        // Owner destruction is also an implicit release of its exclusive claim. Retaining MappedBy(owner)
        // would permanently exclude live importers after the owner's local id is gone.
        if entry.state() == MapState::MappedBy(entry.key.session) {
            entry.set_state(MapState::Unmapped);
        }
        let owner = entry.key.session;
        entry.owner_released = true;
        entry.revoke(owner);
        let key = entry.key;
        registry.by_resource.remove(&key);
        registry.collect(id);
        drop(registry);
        self.release_unused_authority(owner);
        Ok(())
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn owner_release(&self, id: ExportId) -> Result<()> {
        self.owner_release_inner(id)
    }

    /// Claim exclusive use. Refuses if already mapped by anyone, including the caller.
    pub fn map(&self, session: SessionId, id: ExportId) -> Result<()> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("map: no such export"))?;
        if entry.pending.is_some() {
            return Err(GpuError::MappedElsewhere {
                kind: "shared buffer transition",
                id: id.0 as u32,
            });
        }
        if !entry.is_party(session) {
            return Err(GpuError::Invalid("map: not a party to this export"));
        }
        match entry.state() {
            MapState::Unmapped => {
                entry.set_state(MapState::MappedBy(session));
                Ok(())
            }
            MapState::MappedBy(_) => Err(GpuError::Invalid("map: already mapped")),
        }
    }

    /// Release an exclusive claim. Only the holder may.
    pub fn unmap(&self, session: SessionId, id: ExportId) -> Result<()> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("unmap: no such export"))?;
        if entry.pending.is_some() {
            return Err(GpuError::MappedElsewhere {
                kind: "shared buffer transition",
                id: id.0 as u32,
            });
        }
        if !entry.is_party(session) {
            return Err(GpuError::Invalid("unmap: not a party to this export"));
        }
        if entry.state() != MapState::MappedBy(session) {
            return Err(GpuError::Invalid("unmap: not held by this session"));
        }
        entry.set_state(MapState::Unmapped);
        Ok(())
    }

    /// **The guard.** Whether `session` may touch this resource right now.
    ///
    /// This is the predicate the single resource-resolution point calls. While a resource is mapped, use
    /// by any session other than the holder is refused. Slice 1 defines and tests the predicate; wiring
    /// it into `ResourceTable::get`/`get_mut` — the one place every command resolves an id to its native
    /// object — is the next slice, and is the gate the capability must not ship without.
    pub fn check_access(&self, session: SessionId, id: ExportId) -> Result<()> {
        let registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get(&id)
            .ok_or(GpuError::Invalid("access: no such export"))?;
        if !entry.is_party(session) {
            return Err(GpuError::Invalid("access: not a party to this export"));
        }
        match entry.state() {
            MapState::Unmapped => Ok(()),
            MapState::MappedBy(holder) if holder == session => Ok(()),
            MapState::MappedBy(_) => Err(GpuError::Invalid(
                "access: the resource is mapped by another session",
            )),
        }
    }

    /// Retained bytes currently charged to `session` by this registry.
    pub fn bytes_charged_to(&self, session: SessionId) -> u64 {
        let registry = self.inner.lock().unwrap();
        registry
            .entries
            .values()
            .filter(|e| e.charged_to() == Some(session))
            .map(|e| e.bytes)
            .sum()
    }

    pub fn is_live(&self, id: ExportId) -> bool {
        self.inner.lock().unwrap().entries.contains_key(&id)
    }

    /// Drop every reference held by a departing session, in both directions.
    pub fn forget_session(&self, session: SessionId) {
        loop {
            #[derive(Clone, Copy)]
            enum Role {
                Owner(ExportId, bool),
                Importer(ExportId, bool),
                Claim(ExportId),
            }
            let role = {
                let registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
                registry.entries.iter().find_map(|(id, entry)| {
                    if entry.key.session == session && !entry.owner_released {
                        Some(Role::Owner(*id, entry.owner_account.is_some()))
                    } else if entry.importers.contains(&session) {
                        Some(Role::Importer(*id, entry.accounts.contains_key(&session)))
                    } else if entry.state() == MapState::MappedBy(session) {
                        Some(Role::Claim(*id))
                    } else {
                        None
                    }
                })
            };
            let Some(role) = role else {
                self.release_unused_authority(session);
                return;
            };
            let result = match role {
                Role::Owner(id, true) => self
                    .prepare_owner_release(session, id)
                    .map(|plan| plan.commit()),
                Role::Owner(id, false) => self.owner_release_inner(id),
                Role::Importer(id, true) => self
                    .prepare_import_release(session, id)
                    .map(|plan| plan.commit()),
                Role::Importer(id, false) => self.release_import_inner(session, id),
                Role::Claim(id) => self.unmap(session, id),
            };
            if matches!(result, Err(GpuError::MappedElsewhere { .. })) {
                let id = match role {
                    Role::Owner(id, _) | Role::Importer(id, _) | Role::Claim(id) => id,
                };
                self.settle_transition(id, std::time::Duration::from_millis(10));
            } else if result.is_err() {
                return;
            }
        }
    }
}
