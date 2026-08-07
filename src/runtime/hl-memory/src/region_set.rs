//! Sorted, coalescing set of guest address ranges backing one ledger view.
use crate::model::{Backing, MapRequest, MappingRange, MemoryError, Placement, Protection, Region, Resolution};
use hl_isa::{AddressRange, GuestAddress};
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RegionSet {
    pub(crate) regions: Vec<Region>,
}

impl RegionSet {
    pub(crate) fn map(
        &mut self,
        request: MapRequest,
        charge: u64,
        reserved: bool,
    ) -> Result<GuestAddress, MemoryError> {
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

    pub(crate) fn unmap(&mut self, removed: AddressRange) -> Result<(), MemoryError> {
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

    // Receiver kept so the retain/protect helpers read as operations on the set.
    #[allow(clippy::unused_self)]
    fn retain_left(&self, region: Region, removed: AddressRange, output: &mut Vec<Region>) -> Result<(), MemoryError> {
        if region.range().start() < removed.start() {
            output.push(region.slice(region.range().start(), region.range().end().min(removed.start()))?);
        }
        Ok(())
    }

    #[allow(clippy::unused_self)]
    fn retain_right(&self, region: Region, removed: AddressRange, output: &mut Vec<Region>) -> Result<(), MemoryError> {
        if region.range().end() > removed.end() {
            output.push(region.slice(region.range().start().max(removed.end()), region.range().end())?);
        }
        Ok(())
    }

    pub(crate) fn protect(&mut self, changed: AddressRange, protection: Protection) -> Result<(), MemoryError> {
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

    pub(crate) fn charge(&mut self, changed: AddressRange, charged: bool) -> Result<(), MemoryError> {
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

    #[allow(clippy::unused_self)]
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

    pub(crate) fn finish(&mut self) -> Result<(), MemoryError> {
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

    pub(crate) fn validate(&self) -> Result<(), MemoryError> {
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

    pub(crate) fn overlaps(&self, range: AddressRange) -> bool {
        self.regions.iter().any(|region| region.overlaps(range))
    }

    pub(crate) fn resolve(&self, address: GuestAddress, required: Protection) -> Option<Resolution> {
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
