//! Host port reservation: the published/prepared entry, its rollback guard, and bind commit.
use crate::catalog::{CatalogSocket, NetworkCatalog};
use crate::{NetworkCatalogError, PortCheckpoint, SocketSnapshot};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Generation-qualified rollback capability for one atomic host bind update.
#[derive(Clone)]
pub(crate) enum PortEntry {
    Published(PortCheckpoint),
    Prepared {
        checkpoint: PortCheckpoint,
        generation: u64,
    },
}

impl PortEntry {
    pub(crate) fn checkpoint(&self) -> &PortCheckpoint {
        match self {
            Self::Published(checkpoint) | Self::Prepared { checkpoint, .. } => checkpoint,
        }
    }
}

#[must_use = "dropping an uncommitted bind rolls its reservation back"]
pub struct PreparedBind<'a> {
    catalog: &'a NetworkCatalog,
    _admission: crate::checkpoint_activity::Admission<'a>,
    previous: Arc<CatalogSocket>,
    installed: SocketSnapshot,
    port: PortCheckpoint,
    generation: u64,
    finalized: bool,
}

impl PreparedBind<'_> {
    pub fn commit(mut self) -> Result<(), NetworkCatalogError> {
        self.catalog.commit_prepared(&mut self)
    }

    pub fn rollback(mut self) -> Result<(), NetworkCatalogError> {
        self.catalog.rollback_prepared(&mut self)
    }
}

impl Drop for PreparedBind<'_> {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = self.catalog.rollback_prepared(self);
        }
    }
}
impl NetworkCatalog {
    pub fn claim_port(&self, checkpoint: PortCheckpoint) -> Result<(), NetworkCatalogError> {
        let _admission = self.activity.admit();
        let slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = match Self::slot(&slots, checkpoint.owner)?.socket.as_deref() {
            Some(CatalogSocket::Host { snapshot, .. }) => snapshot,
            Some(CatalogSocket::UnixPair { endpoints, .. }) => endpoints
                .iter()
                .find(|snapshot| snapshot.id == checkpoint.owner)
                .ok_or(NetworkCatalogError::Stale)?,
            Some(CatalogSocket::Unix { snapshot, .. }) => snapshot,
            None => return Err(NetworkCatalogError::Stale),
        };
        if !Self::owns_port(owner, &checkpoint) {
            return Err(NetworkCatalogError::Invalid);
        }
        let mut ports = self.ports.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if ports.iter().any(|port| {
            let port = port.checkpoint();
            port.family == checkpoint.family && port.port == checkpoint.port
        }) {
            return Err(NetworkCatalogError::Invalid);
        }
        ports.push(PortEntry::Published(checkpoint));
        Ok(())
    }

    /// Atomically publishes a bound host snapshot and reserves its port.
    ///
    /// Locks are acquired in `slots` then `ports` order and released before
    /// returning, so callers may perform host work before commit or rollback.
    pub fn prepare_host_bind(
        &self,
        installed: SocketSnapshot,
        port: PortCheckpoint,
    ) -> Result<PreparedBind<'_>, NetworkCatalogError> {
        let admission = self.activity.admit();
        if installed.id != port.owner || !crate::SocketNamespace::valid_checkpoint_snapshot(&installed) {
            return Err(NetworkCatalogError::Invalid);
        }
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = Self::slot_mut(&mut slots, port.owner)?;
        let Some(previous) = slot
            .socket
            .as_ref()
            .filter(|socket| matches!(socket.as_ref(), CatalogSocket::Host { .. }))
        else {
            return Err(NetworkCatalogError::Invalid);
        };
        if !Self::owns_port(&installed, &port) {
            return Err(NetworkCatalogError::Invalid);
        }
        let previous = previous.clone();
        let mut ports = self.ports.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if ports.iter().any(|reserved| {
            let reserved = reserved.checkpoint();
            reserved.family == port.family && reserved.port == port.port
        }) {
            return Err(NetworkCatalogError::Invalid);
        }
        let generation = self.port_generation.fetch_add(1, Ordering::Relaxed);
        if generation == 0 {
            return Err(NetworkCatalogError::Capacity);
        }
        ports.push(PortEntry::Prepared {
            checkpoint: port.clone(),
            generation,
        });
        Ok(PreparedBind {
            catalog: self,
            _admission: admission,
            previous,
            installed,
            port,
            generation,
            finalized: false,
        })
    }

    pub(crate) fn commit_prepared(&self, prepared: &mut PreparedBind<'_>) -> Result<(), NetworkCatalogError> {
        if prepared.finalized {
            return Ok(());
        }
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = Self::slot_mut(&mut slots, prepared.port.owner)?;
        let Some(current) = slot.socket.as_ref() else {
            return Err(NetworkCatalogError::Stale);
        };
        if !Arc::ptr_eq(current, &prepared.previous) {
            return Err(NetworkCatalogError::Stale);
        }
        let CatalogSocket::Host {
            resource,
            binding,
            accepted,
            ..
        } = current.as_ref()
        else {
            return Err(NetworkCatalogError::Stale);
        };
        let replacement = Arc::new(CatalogSocket::Host {
            snapshot: prepared.installed.clone(),
            resource: *resource,
            binding: binding.clone(),
            accepted: accepted.clone(),
        });
        let mut ports = self.ports.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = ports.iter_mut().find(|entry| {
            matches!(
                entry,
                PortEntry::Prepared { checkpoint, generation }
                    if checkpoint == &prepared.port && *generation == prepared.generation
            )
        }) else {
            return Err(NetworkCatalogError::Stale);
        };
        *entry = PortEntry::Published(prepared.port.clone());
        slot.socket = Some(replacement);
        self.advance_generation();
        prepared.finalized = true;
        Ok(())
    }

    pub(crate) fn rollback_prepared(&self, prepared: &mut PreparedBind<'_>) -> Result<(), NetworkCatalogError> {
        if prepared.finalized {
            return Ok(());
        }
        let slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ports = self.ports.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = ports.iter().position(|entry| {
            matches!(
                entry,
                PortEntry::Prepared { checkpoint, generation }
                    if checkpoint == &prepared.port && *generation == prepared.generation
            )
        }) else {
            return Err(NetworkCatalogError::Stale);
        };
        ports.remove(index);
        drop(slots);
        prepared.finalized = true;
        Ok(())
    }

    pub(crate) fn owns_port(snapshot: &SocketSnapshot, checkpoint: &PortCheckpoint) -> bool {
        matches!(
            (&snapshot.local, checkpoint.family),
            (
                Some(crate::SocketAddress::Inet4 { port, .. }),
                crate::AddressFamily::Inet4
            ) | (
                Some(crate::SocketAddress::Inet6 { port, .. }),
                crate::AddressFamily::Inet6
            ) if *port == checkpoint.port && checkpoint.port != 0
        )
    }
}
