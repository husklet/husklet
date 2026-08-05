use crate::mapping::plan::{Operation, PlannedOperation};
use crate::model::{Backing, MapRequest, MappingRange, MemoryError, Placement, Protection, Region, Resolution};
use hl_isa::{AddressRange, GuestAddress};
use std::sync::RwLock;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RegionSet {
    regions: Vec<Region>,
}

impl RegionSet {
    fn map(&mut self, request: MapRequest, charge: u64, reserved: bool) -> Result<GuestAddress, MemoryError> {
        request.validate()?;
        let start = request.choose(&self.regions)?;
        let range = MappingRange::create(start, request.length)?;
        self.apply_placement(request.placement, range)?;
        if charge > request.length {
            return Err(MemoryError::InvariantViolation);
        }
        self.regions.push(Region {
            range,
            protection: request.protection,
            backing: request.backing,
            backing_offset: request.backing_offset,
            charge: AddressRange::nonempty(start, charge).ok(),
            reserved,
        });
        self.finish()?;
        Ok(start)
    }

    fn apply_placement(&mut self, placement: Placement, range: AddressRange) -> Result<(), MemoryError> {
        if matches!(placement, Placement::Fixed(_)) {
            return self.unmap(range);
        }
        if self.overlaps(range) {
            return Err(MemoryError::AlreadyMapped);
        }
        Ok(())
    }

    fn unmap(&mut self, removed: AddressRange) -> Result<(), MemoryError> {
        let mut survivors = Vec::with_capacity(self.regions.len().saturating_add(1));
        for region in self.regions.iter().copied() {
            self.retain_unmapped(region, removed, &mut survivors)?;
        }
        self.regions = survivors;
        Ok(())
    }

    fn retain_unmapped(
        &self,
        region: Region,
        removed: AddressRange,
        output: &mut Vec<Region>,
    ) -> Result<(), MemoryError> {
        if !region.overlaps(removed) {
            output.push(region);
            return Ok(());
        }
        self.retain_left(region, removed, output)?;
        self.retain_right(region, removed, output)
    }

    fn retain_left(&self, region: Region, removed: AddressRange, output: &mut Vec<Region>) -> Result<(), MemoryError> {
        if region.range().start() < removed.start() {
            output.push(region.slice(region.range().start(), region.range().end().min(removed.start()))?);
        }
        Ok(())
    }

    fn retain_right(&self, region: Region, removed: AddressRange, output: &mut Vec<Region>) -> Result<(), MemoryError> {
        if region.range().end() > removed.end() {
            output.push(region.slice(region.range().start().max(removed.end()), region.range().end())?);
        }
        Ok(())
    }

    fn protect(&mut self, changed: AddressRange, protection: Protection) -> Result<(), MemoryError> {
        if !self.covers(changed) {
            return Err(MemoryError::Unmapped);
        }
        let mut protected = Vec::with_capacity(self.regions.len().saturating_add(2));
        for region in self.regions.iter().copied() {
            self.protect_region(region, changed, protection, &mut protected)?;
        }
        self.regions = protected;
        self.finish()
    }

    fn charge(&mut self, changed: AddressRange, charged: bool) -> Result<(), MemoryError> {
        if !self.covers(changed) {
            return Err(MemoryError::Unmapped);
        }
        for region in &mut self.regions {
            if !region.overlaps(changed) {
                continue;
            }
            if !region.reserved() || !matches!(region.backing(), Backing::Anonymous { .. }) {
                return Err(MemoryError::InvariantViolation);
            }
            let first = region.range().start().max(changed.start());
            let last = region.range().end().min(changed.end());
            let middle = AddressRange::nonempty(first, last.get() - first.get())?;
            region.charge = if charged {
                match region.charge {
                    Some(current) if middle.start() <= current.end() && current.start() <= middle.end() => {
                        Some(AddressRange::nonempty(
                            current.start().min(middle.start()),
                            current.end().max(middle.end()).get() - current.start().min(middle.start()).get(),
                        )?)
                    }
                    Some(_) => return Err(MemoryError::InvariantViolation),
                    None => Some(middle),
                }
            } else {
                match region.charge {
                    None => None,
                    Some(current) if middle.start() <= current.start() && middle.end() >= current.end() => None,
                    Some(current) if middle.start() <= current.start() => Some(AddressRange::nonempty(
                        middle.end(),
                        current.end().get().saturating_sub(middle.end().get()),
                    )?),
                    Some(current) if middle.end() >= current.end() => Some(AddressRange::nonempty(
                        current.start(),
                        middle.start().get() - current.start().get(),
                    )?),
                    Some(_) => return Err(MemoryError::InvariantViolation),
                }
            };
        }
        self.finish()
    }

    fn covers(&self, range: AddressRange) -> bool {
        let mut cursor = range.start();
        for region in &self.regions {
            if region.range().end() <= cursor {
                continue;
            }
            if region.range().start() > cursor {
                return false;
            }
            cursor = region.range().end();
            if cursor >= range.end() {
                return true;
            }
        }
        false
    }

    fn protect_region(
        &self,
        region: Region,
        changed: AddressRange,
        protection: Protection,
        output: &mut Vec<Region>,
    ) -> Result<(), MemoryError> {
        if !region.overlaps(changed) {
            output.push(region);
            return Ok(());
        }
        let first = region.range().start().max(changed.start());
        let last = region.range().end().min(changed.end());
        if region.range().start() < first {
            output.push(region.slice(region.range().start(), first)?);
        }
        let mut middle = region.slice(first, last)?;
        middle.protection = protection;
        output.push(middle);
        if last < region.range().end() {
            output.push(region.slice(last, region.range().end())?);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), MemoryError> {
        self.canonicalize()?;
        self.validate()
    }

    fn canonicalize(&mut self) -> Result<(), MemoryError> {
        self.regions.sort_unstable_by_key(|region| region.range().start());
        let mut merged: Vec<Region> = Vec::with_capacity(self.regions.len());
        for region in self.regions.iter().copied() {
            Self::append_canonical(&mut merged, region)?;
        }
        self.regions = merged;
        Ok(())
    }

    fn append_canonical(output: &mut Vec<Region>, region: Region) -> Result<(), MemoryError> {
        let Some(previous) = output.last_mut() else {
            output.push(region);
            return Ok(());
        };
        if previous.can_merge(region)? {
            return previous.merge(region);
        }
        output.push(region);
        Ok(())
    }

    fn validate(&self) -> Result<(), MemoryError> {
        for region in &self.regions {
            region.validate()?;
        }
        for pair in self.regions.windows(2) {
            Self::validate_pair(pair[0], pair[1])?;
        }
        Ok(())
    }

    fn validate_pair(left: Region, right: Region) -> Result<(), MemoryError> {
        if left.range().end() > right.range().start() || left.can_merge(right)? {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(())
    }

    fn overlaps(&self, range: AddressRange) -> bool {
        self.regions.iter().any(|region| region.overlaps(range))
    }

    fn resolve(&self, address: GuestAddress, required: Protection) -> Option<Resolution> {
        let region = self.region_at(address)?;
        if !region.protection().contains(required) {
            return None;
        }
        let offset = address.get() - region.range().start().get();
        Some(Resolution {
            region,
            backing_offset: region.backing_offset().checked_add(offset)?,
            contiguous: region.range().end().get() - address.get(),
        })
    }

    fn region_at(&self, address: GuestAddress) -> Option<Region> {
        let index = self
            .regions
            .binary_search_by(|region| {
                if address < region.range().start() {
                    std::cmp::Ordering::Greater
                } else if address >= region.range().end() {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .ok()?;
        Some(self.regions[index])
    }
}

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
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> MemoryLedgerSnapshot {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        MemoryLedgerSnapshot {
            generation: state.generation,
            regions: state.mappings.regions.clone(),
        }
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.state.read().unwrap_or_else(|error| error.into_inner()).generation
    }

    #[must_use]
    pub fn regions(&self) -> Vec<Region> {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .mappings
            .regions
            .clone()
    }

    pub(crate) fn executable_aliases(&self, backing: Backing) -> ExecutableAliasEvidence {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        ExecutableAliasEvidence {
            generation: state.generation,
            present: state.mappings.regions.iter().any(|region| {
                same_backing_identity(region.backing(), backing) && region.protection().contains(Protection::EXECUTE)
            }),
        }
    }

    pub(crate) fn replace(&self, expected: u64, regions: Vec<Region>) -> Result<u64, MemoryError> {
        let mut mappings = RegionSet { regions };
        mappings.finish()?;
        let mut live = self.state.write().unwrap_or_else(|error| error.into_inner());
        if live.generation != expected {
            return Err(MemoryError::InvariantViolation);
        }
        live.mappings = mappings;
        live.generation = live.generation.wrapping_add(1);
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
        let mut live = self.state.write().unwrap_or_else(|error| error.into_inner());
        let mut staged = live.mappings.clone();
        let address = staged.map(request, charge, reserved)?;
        commit(address, &staged.regions)?;
        live.mappings = staged;
        live.generation = live.generation.wrapping_add(1);
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
        let mut live = self.state.write().unwrap_or_else(|error| error.into_inner());
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
        Ok(addresses)
    }

    #[must_use]
    pub fn resolve(&self, address: GuestAddress, required: Protection) -> Option<Resolution> {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .mappings
            .resolve(address, required)
    }

    /// Reports whether every byte in `range` belongs to a logical mapping.
    #[must_use]
    pub fn contains(&self, range: AddressRange) -> bool {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
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
            .unwrap_or_else(|error| error.into_inner())
            .mappings
            .overlaps(range)
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .mappings
            .validate()
    }

    fn mutate_with(
        &self,
        operation: impl FnOnce(&mut RegionSet) -> Result<(), MemoryError>,
        commit: impl FnOnce(&[Region]) -> Result<(), MemoryError>,
    ) -> Result<(), MemoryError> {
        let mut live = self.state.write().unwrap_or_else(|error| error.into_inner());
        let mut staged = live.mappings.clone();
        operation(&mut staged)?;
        staged.finish()?;
        commit(&staged.regions)?;
        live.mappings = staged;
        live.generation = live.generation.wrapping_add(1);
        Ok(())
    }
}

fn same_backing_identity(left: Backing, right: Backing) -> bool {
    match (left, right) {
        (Backing::Shared(left), Backing::Shared(right)) => left.object == right.object,
        (Backing::Anonymous { identity: left, .. }, Backing::Anonymous { identity: right, .. }) => left == right,
        (Backing::File { identity: left, .. }, Backing::File { identity: right, .. }) => left == right,
        _ => false,
    }
}
