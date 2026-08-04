use std::sync::atomic::Ordering;

use crate::{Backing, FileIdentity, MemoryError, Region};

use super::host::Coordinator;
use super::port::{Host, MemoryAccessHost};

/// Coalesced mutation observed for one stable external file identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackingChange {
    pub identity: FileIdentity,
    pub old_size: u64,
    pub new_size: u64,
    pub flags: BackingChangeFlags,
}

/// Provider evidence merged while a backing change awaits reconciliation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BackingChangeFlags(u8);

impl BackingChangeFlags {
    pub const SIZE: Self = Self(1);
    pub const DATA: Self = Self(2);
    pub const IDENTITY: Self = Self(4);
    pub const DELETED: Self = Self(8);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn contains(self, requested: Self) -> bool {
        self.0 & requested.0 == requested.0
    }
}

/// Host boundary that repairs concrete mappings before a change is published.
pub trait BackingChangeHost: Host {
    fn stage_backing_change(&self, change: BackingChange, mappings: &[Region]) -> Result<u64, MemoryError>;
}

impl<H: BackingChangeHost + MemoryAccessHost> Coordinator<H> {
    /// Reconciles every alias of an externally changed file as one transaction.
    ///
    /// The provider supplies stable identity and consecutive extents. The
    /// coordinator excludes map/unmap/protect publication, selects every live
    /// alias, and advances the access epoch only after host repair commits.
    pub fn backing_changed(&self, change: BackingChange) -> Result<usize, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self.transaction.lock().unwrap_or_else(|error| error.into_inner());
        let mut transition = self.transition();
        let mappings: Vec<_> = self
            .ledger
            .regions()
            .into_iter()
            .filter(|region| {
                matches!(
                    region.backing(),
                    Backing::File { identity, .. } if identity == change.identity
                )
            })
            .collect();
        if mappings.is_empty() {
            return Ok(0);
        }
        let reservation = self.host.stage_backing_change(change, &mappings)?;
        if let Err(error) = self.host.commit(&[reservation]) {
            self.host.rollback(reservation);
            return Err(error);
        }
        self.host.epoch.fetch_add(1, Ordering::AcqRel);
        self.host.executable.publish(
            mappings
                .iter()
                .filter(|mapping| mapping.protection().contains(crate::Protection::EXECUTE))
                .map(|mapping| mapping.range()),
        );
        for mapping in &mappings {
            self.invalidate_exclusive(mapping.range())?;
        }
        transition.published(self.ledger.generation());
        Ok(mappings.len())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use hl_isa::{AddressRange, GuestAddress};

    use crate::{
        Backing, MapRequest, MappingCoordinator, MappingHost, MemoryAccessHost, MemoryError, Placement, Protection,
    };

    use super::{BackingChange, BackingChangeFlags, BackingChangeHost};

    const FILE: crate::FileIdentity = crate::FileIdentity { device: 7, object: 9 };

    #[derive(Debug, Default)]
    struct State {
        changes: Vec<(BackingChange, Vec<crate::Region>)>,
        commits: usize,
        rollbacks: usize,
        fail_commit: bool,
    }

    #[derive(Debug, Default)]
    struct Host {
        state: Mutex<State>,
    }

    impl MappingHost for Host {
        fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
            Ok(1)
        }
        fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
            Ok(1)
        }
        fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
            Ok(1)
        }
        fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
            let mut state = self.state.lock().unwrap();
            state.commits += 1;
            if state.fail_commit {
                Err(MemoryError::InvariantViolation)
            } else {
                Ok(())
            }
        }
        fn rollback(&self, _: u64) {
            self.state.lock().unwrap().rollbacks += 1;
        }
    }

    impl BackingChangeHost for Host {
        fn stage_backing_change(&self, change: BackingChange, mappings: &[crate::Region]) -> Result<u64, MemoryError> {
            self.state.lock().unwrap().changes.push((change, mappings.to_vec()));
            Ok(41)
        }
    }

    impl MemoryAccessHost for Host {
        type Projection = u64;

        fn read(&self, _: AddressRange, output: &mut [u8]) -> Result<(), MemoryError> {
            output.fill(0);
            Ok(())
        }

        fn prepare_write(&self, _: AddressRange) -> Result<u64, MemoryError> {
            Ok(1)
        }

        fn commit_write(&self, _: u64, _: &[u8]) -> Result<(), MemoryError> {
            Ok(())
        }

        fn rollback_write(&self, _: u64) {}
    }

    fn request(address: u64, identity: crate::FileIdentity) -> MapRequest {
        MapRequest {
            placement: Placement::Fixed(GuestAddress::new(address)),
            length: 4096,
            alignment: 4096,
            protection: Protection::READ,
            backing: Backing::File { identity, shared: true },
            backing_offset: 0,
        }
    }

    #[test]
    fn shrink_aliases() {
        let coordinator = MappingCoordinator::new(Host::default());
        coordinator.map(request(0x1000, FILE)).unwrap();
        coordinator.map(request(0x3000, FILE)).unwrap();
        coordinator
            .map(request(0x5000, crate::FileIdentity { device: 7, object: 10 }))
            .unwrap();
        let epoch = coordinator.observer_epoch();
        let change = BackingChange {
            identity: FILE,
            old_size: 8192,
            new_size: 1024,
            flags: BackingChangeFlags::SIZE.union(BackingChangeFlags::DATA),
        };
        assert_eq!(coordinator.backing_changed(change), Ok(2));
        assert_eq!(coordinator.observer_epoch(), epoch + 1);
        let state = coordinator.host.state.lock().unwrap();
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].0, change);
        assert_eq!(state.changes[0].1.len(), 2);
    }

    #[test]
    fn regrow_rollback() {
        let coordinator = MappingCoordinator::new(Host::default());
        coordinator.map(request(0x1000, FILE)).unwrap();
        let epoch = coordinator.observer_epoch();
        coordinator.host.state.lock().unwrap().fail_commit = true;
        let change = BackingChange {
            identity: FILE,
            old_size: 1024,
            new_size: 8192,
            flags: BackingChangeFlags::SIZE,
        };
        assert_eq!(
            coordinator.backing_changed(change),
            Err(MemoryError::InvariantViolation)
        );
        assert_eq!(coordinator.observer_epoch(), epoch);
        assert_eq!(coordinator.host.state.lock().unwrap().rollbacks, 1);
    }

    #[test]
    fn unrelated_isolation() {
        let coordinator = MappingCoordinator::new(Host::default());
        coordinator.map(request(0x1000, FILE)).unwrap();
        let change = BackingChange {
            identity: crate::FileIdentity { device: 8, object: 9 },
            old_size: 4,
            new_size: 8,
            flags: BackingChangeFlags::SIZE,
        };
        assert_eq!(coordinator.backing_changed(change), Ok(0));
        assert!(coordinator.host.state.lock().unwrap().changes.is_empty());
    }
}
