use crate::mapping::plan::{Operation, PlannedOperation};
use crate::model::{Backing, MapRequest, MappingRange, MemoryError, Protection, Region, Resolution};
use crate::region_set::RegionSet;
use hl_isa::{AddressRange, GuestAddress};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LedgerState {
    mappings: RegionSet,
    generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryLedgerSnapshot {
    pub generation: u64,
    pub regions: Vec<Region>,
}

#[derive(Debug, Default)]
pub struct MemoryLedger {
    state: RwLock<LedgerState>,
    generation: AtomicU64,
}

/// Generation-qualified evidence about executable mappings that share one
/// backing identity. Consumers retain the coordinator mapping transaction
/// while using this value, so the recorded generation cannot be superseded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutableAliasEvidence {
    pub generation: u64,
    pub present: bool,
}

impl MemoryLedger {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: RwLock::new(LedgerState {
                mappings: RegionSet { regions: Vec::new() },
                generation: 0,
            }),
            generation: AtomicU64::new(0),
        }
    }

    pub fn restore(snapshot: MemoryLedgerSnapshot) -> Result<Self, MemoryError> {
        let mut mappings = RegionSet {
            regions: snapshot.regions,
        };
        mappings.finish()?;
        Ok(Self {
            state: RwLock::new(LedgerState {
                mappings,
                generation: snapshot.generation,
            }),
            generation: AtomicU64::new(snapshot.generation),
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> MemoryLedgerSnapshot {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        MemoryLedgerSnapshot {
            generation: state.generation,
            regions: state.mappings.regions.clone(),
        }
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn regions(&self) -> Vec<Region> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mappings
            .regions
            .clone()
    }

    /// Sharing a backing identity is not aliasing: an image maps its text and its data
    /// from one file at disjoint offsets, so only an overlapping byte interval aliases.
    pub(crate) fn executable_aliases(&self, address: GuestAddress, backing: Backing) -> ExecutableAliasEvidence {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = state.generation;
        let Some(source) = state
            .mappings
            .regions
            .iter()
            .find(|region| region.range().contains(address))
            .map(backing_interval)
        else {
            return ExecutableAliasEvidence {
                generation,
                present: false,
            };
        };
        let present = state.mappings.regions.iter().any(|region| {
            same_backing_identity(region.backing(), backing)
                && region.protection().contains(Protection::EXECUTE)
                && match (source, backing_interval(region)) {
                    (Some(source), Some(alias)) => alias.0 < source.1 && source.0 < alias.1,
                    _ => true,
                }
        });
        ExecutableAliasEvidence { generation, present }
    }

    pub(crate) fn replace(&self, expected: u64, regions: Vec<Region>) -> Result<u64, MemoryError> {
        let mut mappings = RegionSet { regions };
        mappings.finish()?;
        let mut live = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if live.generation != expected {
            return Err(MemoryError::InvariantViolation);
        }
        live.mappings = mappings;
        live.generation = live.generation.wrapping_add(1);
        self.generation.store(live.generation, Ordering::Release);
        Ok(live.generation)
    }

    pub fn map(&self, request: MapRequest) -> Result<GuestAddress, MemoryError> {
        self.map_transaction(request, 0, false, |_, _| Ok(()))
    }

    pub fn map_charged(&self, request: MapRequest, charge: u64) -> Result<GuestAddress, MemoryError> {
        self.map_transaction(request, charge, true, |_, _| Ok(()))
    }

    pub(crate) fn map_transaction(
        &self,
        request: MapRequest,
        charge: u64,
        reserved: bool,
        commit: impl FnOnce(GuestAddress, &[Region]) -> Result<(), MemoryError>,
    ) -> Result<GuestAddress, MemoryError> {
        let mut live = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut staged = live.mappings.clone();
        let address = staged.map(request, charge, reserved)?;
        commit(address, &staged.regions)?;
        live.mappings = staged;
        live.generation = live.generation.wrapping_add(1);
        self.generation.store(live.generation, Ordering::Release);
        Ok(address)
    }

    pub fn unmap(&self, range: AddressRange) -> Result<(), MemoryError> {
        self.unmap_transaction(range, |_| Ok(()))
    }

    pub(crate) fn unmap_transaction(
        &self,
        range: AddressRange,
        commit: impl FnOnce(&[Region]) -> Result<(), MemoryError>,
    ) -> Result<(), MemoryError> {
        MappingRange::validate(range)?;
        self.mutate_with(|staged| staged.unmap(range), commit)
    }

    pub fn protect(&self, range: AddressRange, protection: Protection) -> Result<(), MemoryError> {
        self.protect_transaction(range, protection, |_| Ok(()))
    }

    pub(crate) fn protect_transaction(
        &self,
        range: AddressRange,
        protection: Protection,
        commit: impl FnOnce(&[Region]) -> Result<(), MemoryError>,
    ) -> Result<(), MemoryError> {
        MappingRange::validate(range)?;
        self.mutate_with(|staged| staged.protect(range, protection), commit)
    }

    pub(crate) fn batch_transaction(
        &self,
        operations: &[Operation],
        commit: impl FnOnce(&[PlannedOperation], &[Region]) -> Result<(), MemoryError>,
    ) -> Result<Vec<GuestAddress>, MemoryError> {
        let mut live = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut staged = live.mappings.clone();
        let mut plan = Vec::new();
        let mut addresses = Vec::new();
        for operation in operations {
            match *operation {
                Operation::Map(request) | Operation::Replace(request) => {
                    let address = staged.map(request, 0, false)?;
                    plan.push(PlannedOperation::Map(address, request));
                    addresses.push(address);
                }
                Operation::MapCharged(request, charge) | Operation::ReplaceCharged(request, charge) => {
                    let address = staged.map(request, charge, true)?;
                    plan.push(PlannedOperation::Map(address, request));
                    addresses.push(address);
                }
                Operation::Unmap(range) => {
                    MappingRange::validate(range)?;
                    staged.unmap(range)?;
                    plan.push(PlannedOperation::Unmap(range));
                }
                Operation::Protect(range, protection) => {
                    MappingRange::validate(range)?;
                    staged.protect(range, protection)?;
                    plan.push(PlannedOperation::Protect(range, protection));
                }
                Operation::Charge(range) => staged.charge(range, true)?,
                Operation::Uncharge(range) => staged.charge(range, false)?,
            }
        }
        staged.finish()?;
        commit(&plan, &staged.regions)?;
        live.mappings = staged;
        live.generation = live.generation.wrapping_add(1);
        self.generation.store(live.generation, Ordering::Release);
        Ok(addresses)
    }

    #[must_use]
    pub fn resolve(&self, address: GuestAddress, required: Protection) -> Option<Resolution> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mappings
            .resolve(address, required)
    }

    /// Reports whether every byte in `range` belongs to a logical mapping.
    #[must_use]
    pub fn contains(&self, range: AddressRange) -> bool {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut cursor = range.start();
        for region in &state.mappings.regions {
            if region.range().end() <= cursor {
                continue;
            }
            if region.range().start() > cursor {
                return false;
            }
            cursor = region.range().end().min(range.end());
            if cursor == range.end() {
                return true;
            }
        }
        false
    }

    pub fn futex_identity(
        &self,
        address_space: crate::AddressSpaceId,
        address: GuestAddress,
        private: bool,
        access: crate::FutexAccess,
    ) -> Option<crate::FutexIdentity> {
        if address.get() & 3 != 0 {
            return None;
        }
        let required = match access {
            crate::FutexAccess::KeyOnly => Protection::NONE,
            crate::FutexAccess::Read => Protection::READ,
            crate::FutexAccess::Write => Protection::WRITE,
        };
        let resolution = self.resolve(address, required)?;
        if private {
            return Some(crate::FutexIdentity::Private {
                address_space,
                address: address.get(),
            });
        }
        match resolution.region.backing() {
            Backing::Shared(reference) => Some(crate::FutexIdentity::SharedObject {
                slot: reference.object.slot,
                generation: reference.object.generation,
                offset: resolution.backing_offset,
            }),
            Backing::Anonymous { identity, shared: true } => Some(crate::FutexIdentity::SharedAnonymous {
                object: identity,
                offset: resolution.backing_offset,
            }),
            Backing::File { identity, shared: true } => Some(crate::FutexIdentity::SharedFile {
                device: identity.device,
                object: identity.object,
                offset: resolution.backing_offset,
            }),
            Backing::Anonymous { shared: false, .. } | Backing::File { shared: false, .. } => {
                Some(crate::FutexIdentity::Private {
                    address_space,
                    address: address.get(),
                })
            }
        }
    }

    #[must_use]
    pub fn overlaps(&self, range: AddressRange) -> bool {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mappings
            .overlaps(range)
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mappings
            .validate()
    }

    fn mutate_with(
        &self,
        operation: impl FnOnce(&mut RegionSet) -> Result<(), MemoryError>,
        commit: impl FnOnce(&[Region]) -> Result<(), MemoryError>,
    ) -> Result<(), MemoryError> {
        let mut live = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut staged = live.mappings.clone();
        operation(&mut staged)?;
        staged.finish()?;
        commit(&staged.regions)?;
        live.mappings = staged;
        live.generation = live.generation.wrapping_add(1);
        self.generation.store(live.generation, Ordering::Release);
        Ok(())
    }
}

/// The half-open byte interval a region occupies in its backing object.
fn backing_interval(region: &Region) -> Option<(u64, u64)> {
    let first = match region.backing() {
        Backing::Shared(reference) => reference.offset.checked_add(region.backing_offset())?,
        _ => region.backing_offset(),
    };
    Some((first, first.checked_add(region.range().length())?))
}

fn same_backing_identity(left: Backing, right: Backing) -> bool {
    match (left, right) {
        (Backing::Shared(left), Backing::Shared(right)) => left.object == right.object,
        (Backing::Anonymous { identity: left, .. }, Backing::Anonymous { identity: right, .. }) => left == right,
        (Backing::File { identity: left, .. }, Backing::File { identity: right, .. }) => left == right,
        _ => false,
    }
}
