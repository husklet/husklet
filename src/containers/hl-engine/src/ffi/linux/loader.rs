use std::collections::BTreeMap;
use std::sync::Arc;

use hl_isa::GuestAddress;
use hl_loader::{
    AddressSpaceError, ImageProtectionRegistry, MappingKind, MappingPlacement, Protection as ImageProtection,
    ReservedMapping, TransactionalAddressSpace,
};
use hl_memory::{Backing, MapRequest, MappingCoordinator, Placement, Protection};

use super::VirtualMemory;
use super::mapping::MappingHostAdapter;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Reservation(u64);

#[derive(Clone)]
struct Entry {
    address: u64,
    size: u64,
    writes: Vec<(u64, Vec<u8>)>,
    protections: Vec<(u64, u64, ImageProtection)>,
    executable: Vec<(u64, u64)>,
    access: Vec<(u64, u64, bool)>,
}

pub struct AddressSpaceAdapter {
    memory: Arc<MappingCoordinator<MappingHostAdapter>>,
    length: usize,
    next: u64,
    hint: u64,
    staged: BTreeMap<Reservation, Entry>,
    executable: Vec<(u64, u64)>,
    access: Vec<(u64, u64, bool)>,
}

impl AddressSpaceAdapter {
    #[must_use]
    pub fn new(arena: Arc<VirtualMemory>) -> Self {
        let length = arena.length();
        Self::from_memory(
            Arc::new(MappingCoordinator::new(MappingHostAdapter::new(arena))),
            length,
        )
    }

    pub fn from_memory(memory: Arc<MappingCoordinator<MappingHostAdapter>>, length: usize) -> Self {
        Self {
            memory,
            length,
            next: 0,
            hint: 0,
            staged: BTreeMap::new(),
            executable: Vec::new(),
            access: Vec::new(),
        }
    }

    fn entry(&mut self, token: &Reservation) -> Result<&mut Entry, AddressSpaceError> {
        self.staged.get_mut(token).ok_or(AddressSpaceError::InvalidRange)
    }

    fn check(entry: &Entry, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        let end = offset.checked_add(size).ok_or(AddressSpaceError::InvalidRange)?;
        if size == 0 || end > entry.size {
            return Err(AddressSpaceError::InvalidRange);
        }
        Ok(())
    }

    fn memory_protection(value: ImageProtection) -> Result<Protection, AddressSpaceError> {
        let bits = value.bits();
        if bits & ImageProtection::WRITE != 0 && bits & ImageProtection::EXECUTE != 0 {
            return Err(AddressSpaceError::InvalidRange);
        }
        let mut result = Protection::NONE;
        for (bit, protection) in [
            (ImageProtection::READ, Protection::READ),
            (ImageProtection::WRITE, Protection::WRITE),
            (ImageProtection::EXECUTE, Protection::EXECUTE),
        ] {
            if bits & bit != 0 {
                result = result.union(protection);
            }
        }
        Ok(result)
    }

    fn unmap(&self, entries: &[Entry]) {
        for entry in entries {
            if let Ok(range) = hl_isa::AddressRange::nonempty(GuestAddress::new(entry.address), entry.size) {
                let _ = self.memory.unmap(range);
            }
        }
    }

    #[cfg(test)]
    pub(super) fn metadata_count(&self) -> usize {
        self.executable.len() + self.access.len()
    }
}

/// A hint is advisory: an image larger than the gap between two hints slides above everything
/// already staged instead of failing the load.
fn place(hint: u64, size: u64, staged: &[(u64, u64)], page: u64) -> u64 {
    let overlaps = |address: u64| {
        let end = address.saturating_add(size);
        staged
            .iter()
            .any(|(start, len)| address < start.saturating_add(*len) && *start < end)
    };
    if !overlaps(hint) {
        return hint;
    }
    staged
        .iter()
        .filter_map(|(start, len)| start.checked_add(*len))
        .max()
        .map_or(hint, |top| top.next_multiple_of(page))
}

impl AddressSpaceAdapter {
    fn spans(&self) -> Vec<(u64, u64)> {
        self.staged.values().map(|entry| (entry.address, entry.size)).collect()
    }
}

impl TransactionalAddressSpace for AddressSpaceAdapter {
    type Reservation = Reservation;

    fn reserve(
        &mut self,
        _: MappingKind,
        size: u64,
        placement: MappingPlacement,
    ) -> Result<ReservedMapping<Reservation>, AddressSpaceError> {
        const PAGE: u64 = 4096;
        if size == 0 || !size.is_multiple_of(PAGE) {
            return Err(AddressSpaceError::InvalidRange);
        }
        let address = match placement {
            MappingPlacement::Fixed(address) => address,
            MappingPlacement::Hint(Some(address)) => place(address, size, &self.spans(), PAGE),
            MappingPlacement::Hint(None) => place(self.hint, size, &self.spans(), PAGE),
        };
        let end = address.checked_add(size).ok_or(AddressSpaceError::InvalidRange)?;
        if address % PAGE != 0 || end > self.length as u64 {
            return Err(AddressSpaceError::InvalidRange);
        }
        if self
            .staged
            .values()
            .any(|entry| address < entry.address.saturating_add(entry.size) && entry.address < end)
        {
            return Err(AddressSpaceError::Conflict);
        }
        self.next = self.next.wrapping_add(1).max(1);
        self.hint = end;
        let token = Reservation(self.next);
        self.staged.insert(
            token,
            Entry {
                address,
                size,
                writes: Vec::new(),
                protections: Vec::new(),
                executable: Vec::new(),
                access: Vec::new(),
            },
        );
        Ok(ReservedMapping::new(token, address, size))
    }

    fn stage_write(&mut self, token: &Reservation, offset: u64, bytes: &[u8]) -> Result<(), AddressSpaceError> {
        let entry = self.entry(token)?;
        Self::check(entry, offset, bytes.len() as u64)?;
        entry.writes.push((offset, bytes.to_vec()));
        Ok(())
    }

    fn stage_zero(&mut self, token: &Reservation, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        let size = usize::try_from(size).map_err(|_| AddressSpaceError::OutOfMemory)?;
        self.stage_write(token, offset, &vec![0; size])
    }

    fn stage_protection(
        &mut self,
        token: &Reservation,
        offset: u64,
        size: u64,
        protection: ImageProtection,
    ) -> Result<(), AddressSpaceError> {
        Self::memory_protection(protection)?;
        let entry = self.entry(token)?;
        Self::check(entry, offset, size)?;
        entry.protections.push((offset, size, protection));
        Ok(())
    }

    fn commit(&mut self, tokens: &[Reservation]) -> Result<(), AddressSpaceError> {
        let entries = tokens
            .iter()
            .map(|token| self.staged.get(token).cloned().ok_or(AddressSpaceError::InvalidRange))
            .collect::<Result<Vec<_>, _>>()?;
        let mut mapped = Vec::new();
        for entry in &entries {
            let request = MapRequest {
                placement: Placement::Fixed(GuestAddress::new(entry.address)),
                length: entry.size,
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: entry.address,
                    shared: false,
                },
                backing_offset: 0,
            };
            if self.memory.map(request).is_err() {
                self.unmap(&mapped);
                return Err(AddressSpaceError::CommitFailed);
            }
            mapped.push(entry.clone());
        }
        if self.publish_entries(&entries).is_err() {
            self.unmap(&entries);
            return Err(AddressSpaceError::CommitFailed);
        }
        for token in tokens {
            let entry = self.staged.remove(token).ok_or(AddressSpaceError::CommitFailed)?;
            self.executable.extend(entry.executable);
            self.access.extend(entry.access);
        }
        Ok(())
    }

    fn rollback(&mut self, token: &Reservation) {
        self.staged.remove(token);
    }
}

impl AddressSpaceAdapter {
    fn publish_entries(&self, entries: &[Entry]) -> Result<(), ()> {
        for entry in entries {
            self.publish_entry(entry)?;
        }
        Ok(())
    }

    fn publish_entry(&self, entry: &Entry) -> Result<(), ()> {
        for (offset, bytes) in &entry.writes {
            let write = self
                .memory
                .prepare_write(GuestAddress::new(entry.address + offset), bytes.len() as u64)
                .map_err(|_| ())?;
            self.memory.commit_write(write, bytes).map_err(|_| ())?;
        }
        self.memory
            .protect(
                hl_isa::AddressRange::nonempty(GuestAddress::new(entry.address), entry.size).map_err(|_| ())?,
                Protection::NONE,
            )
            .map_err(|_| ())?;
        for (offset, size, protection) in &entry.protections {
            let protection = Self::memory_protection(*protection).map_err(|_| ())?;
            self.memory
                .protect(
                    hl_isa::AddressRange::nonempty(GuestAddress::new(entry.address + offset), *size).map_err(|_| ())?,
                    protection,
                )
                .map_err(|_| ())?;
        }
        Ok(())
    }
}

impl ImageProtectionRegistry<Reservation> for AddressSpaceAdapter {
    fn stage_executable(&mut self, token: &Reservation, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        let entry = self.entry(token)?;
        Self::check(entry, offset, size)?;
        entry.executable.push((offset, size));
        Ok(())
    }

    fn stage_guest_access(
        &mut self,
        token: &Reservation,
        address: u64,
        size: u64,
        read_only: bool,
    ) -> Result<(), AddressSpaceError> {
        if size == 0 {
            return Err(AddressSpaceError::InvalidRange);
        }
        self.entry(token)?.access.push((address, size, read_only));
        Ok(())
    }
}

#[cfg(test)]
mod placement_tests {
    use super::place;

    const PAGE: u64 = 4096;

    /// An unoccupied hint is honored exactly.
    #[test]
    fn a_free_hint_is_honored() {
        assert_eq!(place(0x20_00000, PAGE, &[(0x10_00000, PAGE)], PAGE), 0x20_00000);
    }

    /// A main image wider than the gap to the interpreter hint pushes the interpreter above it
    /// rather than colliding; this is the node-sized-image case.
    #[test]
    fn an_oversized_main_image_pushes_the_interpreter_up() {
        let main = (0x10_00000_u64, 0x80_00000_u64);
        assert_eq!(place(0x20_00000, 0x10000, &[main], PAGE), main.0 + main.1);
    }

    /// The slide lands page aligned even when the occupied top is not.
    #[test]
    fn the_slide_is_page_aligned() {
        assert_eq!(place(0x1000, PAGE, &[(0x1000, 0x1001)], PAGE), 0x3000);
    }
}
