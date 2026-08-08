use super::abi;
use super::arena::{Ledger, Operation};
use super::virtual_advice::Advice;
use super::virtual_host::{GuestVm, LinuxGuestVm};
use super::virtual_lock::Locks;
use hl_memory::Protection;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

pub(super) const HOST_READ: i32 = 1;
const HOST_WRITE: i32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryError {
    InvalidRange,
    OutOfMemory,
    Host,
    Poisoned,
}

#[derive(Debug)]
pub(super) struct ArenaState {
    pub(super) poisoned: bool,
    next: u64,
    pub(super) mappings: Ledger,
    staged: BTreeMap<u64, Operation>,
}

#[derive(Debug, Default)]
struct DirectPins {
    next: u64,
    ranges: BTreeMap<u64, hl_isa::AddressRange>,
}

/// Owns one inaccessible host reservation used for a guest address space.
///
/// Guest addresses accepted by adapters are offsets within this reservation,
/// never host pointers.
#[derive(Debug)]
pub struct Memory {
    resource: crate::native_host::HostResourceLease,
    pub(super) host: Arc<dyn GuestVm>,
    reservation: usize,
    reservation_length: usize,
    page_size: usize,
    base: usize,
    length: usize,
    pub(super) shared: Option<Arc<hl_memory::SharedObjectStore>>,
    pub(super) shared_backings: Option<Arc<super::shared_backing::Registry>>,
    pub(super) snapshot_backings: bool,
    pub(super) inherited_shared: bool,
    pub(super) files: Arc<Mutex<BTreeMap<(u64, u64), super::virtual_file::Registered>>>,
    pub(super) bus_fault: AtomicU64,
    pub(super) advice: Mutex<Advice>,
    pub(super) locks: Mutex<Locks>,
    pub(super) reservations: Arc<hl_memory::ReservationEpochs>,
    direct_pins: Mutex<DirectPins>,
    pub(super) state: Mutex<ArenaState>,
    #[cfg(test)]
    fault: Mutex<Fault>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct Fault {
    call: usize,
    failures: std::collections::BTreeSet<usize>,
}

impl Memory {
    pub fn reserve(length: usize) -> Result<Self, MemoryError> {
        Self::reserve_in(crate::native_host::HostResourceContext::new(), length)
    }

    pub(crate) fn reserve_in(
        context: Arc<crate::native_host::HostResourceContext>,
        length: usize,
    ) -> Result<Self, MemoryError> {
        // SAFETY: sysconf receives no pointer, retains no state, and cannot unwind.
        let page = unsafe { abi::sysconf(abi::_SC_PAGESIZE) };
        let page = usize::try_from(page).map_err(|_| MemoryError::Host)?;
        if length == 0 || !page.is_power_of_two() || !length.is_multiple_of(page) {
            return Err(MemoryError::InvalidRange);
        }
        let reservation_length = length
            .checked_add(page.checked_mul(2).ok_or(MemoryError::InvalidRange)?)
            .ok_or(MemoryError::InvalidRange)?;
        let ownership = context.reserve();
        let host: Arc<dyn GuestVm> = Arc::new(LinuxGuestVm);
        let address = host
            .reserve(reservation_length)
            .map_err(|()| MemoryError::OutOfMemory)?;
        let resource = ownership.publish(super::virtual_reservation::Reservation::new(
            address,
            reservation_length,
            Arc::clone(&host),
        ));
        Ok(Self {
            resource,
            host,
            reservation: address,
            reservation_length,
            page_size: page,
            base: address + page,
            length,
            shared: None,
            shared_backings: None,
            snapshot_backings: false,
            inherited_shared: false,
            files: Arc::new(Mutex::new(BTreeMap::new())),
            bus_fault: AtomicU64::new(u64::MAX),
            advice: Mutex::new(Advice::default()),
            locks: Mutex::new(Locks::default()),
            reservations: Arc::new(hl_memory::ReservationEpochs::default()),
            direct_pins: Mutex::new(DirectPins::default()),
            state: Mutex::new(ArenaState {
                poisoned: false,
                next: 0,
                mappings: Ledger::default(),
                staged: BTreeMap::new(),
            }),
            #[cfg(test)]
            fault: Mutex::new(Fault::default()),
        })
    }

    pub(crate) fn resource_context(&self) -> Arc<crate::native_host::HostResourceContext> {
        self.resource.context()
    }

    /// Native virtual-memory page geometry used by this reservation.
    #[must_use]
    pub const fn page_size(&self) -> usize {
        self.page_size
    }

    #[must_use]
    pub fn with_shared_store(mut self, store: Arc<hl_memory::SharedObjectStore>) -> Self {
        self.shared = Some(store);
        self
    }

    #[must_use]
    pub(super) fn with_shared_backings(mut self, backings: Arc<super::shared_backing::Registry>) -> Self {
        self.shared_backings = Some(backings);
        self
    }

    #[must_use]
    pub(super) fn with_inherited_backings(mut self, source: &Self) -> Self {
        self.shared_backings.clone_from(&source.shared_backings);
        self.snapshot_backings = source.snapshot_backings;
        self
    }

    /// Enables the bounded byte-backed adapter used only while staging a
    /// durable checkpoint image whose native backings have not yet been
    /// rebound.
    #[must_use]
    #[cfg(test)]
    pub(super) fn with_snapshot_backings(mut self) -> Self {
        self.snapshot_backings = true;
        self
    }

    #[must_use]
    pub(super) fn with_inherited_store(mut self, store: Arc<hl_memory::SharedObjectStore>) -> Self {
        self.shared = Some(store);
        self.inherited_shared = true;
        self
    }

    #[must_use]
    pub(super) fn with_file_registry(
        mut self,
        files: Arc<Mutex<BTreeMap<(u64, u64), super::virtual_file::Registered>>>,
    ) -> Self {
        self.files = files;
        self
    }

    pub(super) fn file_registry(&self) -> Arc<Mutex<BTreeMap<(u64, u64), super::virtual_file::Registered>>> {
        Arc::clone(&self.files)
    }

    #[must_use]
    pub const fn length(&self) -> usize {
        self.length
    }

    pub(super) fn host_range(&self, offset: u64, length: u64) -> Result<(*mut core::ffi::c_void, usize), MemoryError> {
        let offset = usize::try_from(offset).map_err(|_| MemoryError::InvalidRange)?;
        let length = usize::try_from(length).map_err(|_| MemoryError::InvalidRange)?;
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= self.length)
            .ok_or(MemoryError::InvalidRange)?;
        if length == 0 || end > self.length {
            return Err(MemoryError::InvalidRange);
        }
        Ok(((self.base + offset) as *mut core::ffi::c_void, length))
    }

    pub(super) fn storage_address(&self, address: u64, length: u64) -> Option<u64> {
        self.host_range(address, length).ok().map(|(value, _)| value as u64)
    }

    pub(super) fn guest_address(&self, storage: u64) -> Option<u64> {
        let offset = storage.checked_sub(self.base as u64)?;
        (offset < self.length as u64).then_some(offset)
    }

    pub(super) fn bus_fault(&self, address: u64, length: u64) -> Option<u64> {
        let end = address.checked_add(length)?;
        if length == 0 {
            return None;
        }
        let fault = self.bus_fault.load(std::sync::atomic::Ordering::Acquire);
        (fault >= address && fault < end).then_some(fault)
    }

    pub(super) fn stage(&self, operation: Operation) -> Result<u64, MemoryError> {
        if self.operation_pinned(operation)? {
            return Err(MemoryError::Host);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(MemoryError::Poisoned);
        }
        state.next = state.next.wrapping_add(1).max(1);
        let token = state.next;
        state.staged.insert(token, operation);
        Ok(token)
    }

    pub(super) fn pin_direct(&self, range: hl_isa::AddressRange) -> Result<u64, MemoryError> {
        self.host_range(range.start().get(), range.length())?;
        let mut pins = self
            .direct_pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pins.next = pins.next.wrapping_add(1).max(1);
        let token = pins.next;
        pins.ranges.insert(token, range);
        Ok(token)
    }

    pub(super) fn unpin_direct(&self, token: u64) {
        self.direct_pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ranges
            .remove(&token);
    }

    fn operation_pinned(&self, operation: Operation) -> Result<bool, MemoryError> {
        let mut affected = Vec::with_capacity(2);
        match operation {
            Operation::Backing(_) => return Ok(false),
            Operation::Map(start, request) => affected.push((start, request.length)),
            Operation::Unmap(start, length) | Operation::Protect(start, length, _) => affected.push((start, length)),
            Operation::Remap(source, destination, request, _) => {
                affected.push((source.start().get(), source.length()));
                affected.push((destination, request.length));
            }
        }
        let ranges = affected
            .into_iter()
            .map(|(start, length)| {
                hl_isa::AddressRange::nonempty(hl_isa::GuestAddress::new(start), length)
                    .map_err(|_| MemoryError::InvalidRange)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pins = self
            .direct_pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(pins.ranges.values().any(|pin| {
            ranges
                .iter()
                .any(|range| pin.start() < range.end() && range.start() < pin.end())
        }))
    }

    pub(super) fn rollback(&self, token: u64) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .staged
            .remove(&token);
    }

    pub(super) fn commit(&self, tokens: &[u64]) -> Result<(), MemoryError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(MemoryError::Poisoned);
        }
        let operations = tokens
            .iter()
            .map(|token| state.staged.get(token).copied().ok_or(MemoryError::Host))
            .collect::<Result<Vec<_>, _>>()?;
        let mut candidate = state.mappings.clone();
        for operation in &operations {
            candidate.apply(*operation)?;
        }
        let mut advice = self.advice.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let advice_candidate = advice.apply(&operations)?;
        let mut locks = self.locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let lock_candidate = locks.apply(&operations)?;
        let applied = self.apply_operations(&mut state, &operations)?;
        if let Err(error) = self.transition_locks(&locks, &lock_candidate) {
            if self.compensate(applied).is_err() || error == MemoryError::Poisoned {
                self.poison(&mut state);
            }
            return Err(error);
        }
        for token in tokens {
            state.staged.remove(token);
        }
        state.mappings = candidate;
        *advice = advice_candidate;
        *locks = lock_candidate;
        Ok(())
    }

    fn apply_operations(
        &self,
        state: &mut ArenaState,
        operations: &[Operation],
    ) -> Result<Vec<Operation>, MemoryError> {
        let mut applied = Vec::new();
        let mut shadow = state.mappings.clone();
        for operation in operations {
            let inverse = shadow.inverse(*operation)?;
            if let Err(error) = self.apply_host(*operation) {
                // Compensate a partial current host mutation as well as earlier ones.
                applied.extend(inverse);
                self.failed_operation(state, applied, error)?;
                unreachable!("failed operation returns an error");
            }
            shadow.apply(*operation)?;
            applied.extend(inverse);
        }
        Ok(applied)
    }

    pub(super) fn apply_host(&self, operation: Operation) -> Result<(), MemoryError> {
        self.host_step()?;
        if let Operation::Backing(change) = operation {
            return self.apply_backing(change);
        }
        if let Operation::Map(offset, request) = operation {
            return self.map_host(offset, request);
        }
        if let Operation::Remap(source, destination, request, keep) = operation {
            return self.remap_host(source, destination, request, keep);
        }
        let (offset, length, protection) = match operation {
            Operation::Backing(_) => unreachable!("backing change handled above"),
            Operation::Map(_, _) => unreachable!("map handled above"),
            Operation::Remap(_, _, _, _) => unreachable!("remap handled above"),
            Operation::Unmap(offset, length) => (offset, length, Protection::NONE),
            Operation::Protect(offset, length, protection) => (offset, length, protection),
        };
        let (address, length) = self.host_range(offset, length)?;
        let native = Self::native_protection(protection)?;
        // SAFETY: the checked range is wholly inside this owner's reservation;
        // no pointer escapes, the transaction mutex excludes adapter access,
        // Linux retains nothing, and mprotect cannot unwind.
        self.host
            .protect(address as usize, length, native)
            .map_err(|()| MemoryError::Host)?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn inject_failures(&self, failures: &[usize]) {
        let mut fault = self.fault.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        fault.call = 0;
        fault.failures = failures.iter().copied().collect();
    }

    #[cfg(test)]
    fn host_step(&self) -> Result<(), MemoryError> {
        let mut fault = self.fault.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        fault.call += 1;
        if fault.failures.contains(&fault.call) {
            return Err(MemoryError::Host);
        }
        Ok(())
    }

    #[cfg(not(test))]
    const fn host_step(&self) -> Result<(), MemoryError> {
        Ok(())
    }

    pub(super) fn disable(&self) {
        // SAFETY: the address and length are this owner's entire live
        // reservation; removing access is fail-closed, Linux retains nothing,
        // and mprotect cannot unwind.
        let _failed = self
            .host
            .protect(self.reservation, self.reservation_length, abi::PROT_NONE);
    }

    pub(super) fn native_protection(protection: Protection) -> Result<i32, MemoryError> {
        // Guest executable bytes are decoded as data and never entered at this
        // address, so EXECUTE projects to host READ. Host WRITE includes READ:
        // this is required on hosts without a representable write-only mapping
        // and by translated read-modify-write operations. NONE remains truly
        // inaccessible instead of carrying permanent checkpoint authority.
        let mut native = abi::PROT_NONE;
        if protection.contains(Protection::READ) || protection.contains(Protection::EXECUTE) {
            native |= HOST_READ;
        }
        if protection.contains(Protection::WRITE) {
            native |= HOST_READ | HOST_WRITE;
        }
        Ok(native)
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn host_permissions(memory: &Memory, offset: u64) -> String {
        let address = memory.storage_address(offset, 1).unwrap() as usize;
        std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .find_map(|line| {
                let (range, rest) = line.split_once(' ')?;
                let (first, last) = range.split_once('-')?;
                let first = usize::from_str_radix(first, 16).ok()?;
                let last = usize::from_str_radix(last, 16).ok()?;
                (first <= address && address < last).then(|| rest[..4].to_owned())
            })
            .unwrap()
    }

    #[test]
    fn bus_span_projection() {
        let memory = Memory::reserve(8192).unwrap();
        let storage = memory.storage_address(4096, 8).unwrap();
        assert_eq!(memory.guest_address(storage), Some(4096));
        memory.bus_fault.store(4100, Ordering::Release);
        assert_eq!(memory.bus_fault(4096, 8), Some(4100));
        assert_eq!(memory.bus_fault(4096, 4), None);
        assert_eq!(memory.bus_fault(4096, 0), None);
        assert_eq!(memory.bus_fault(u64::MAX, 2), None);
    }

    #[test]
    fn resource_lifetime() {
        let context = crate::native_host::HostResourceContext::new();
        let memory = Memory::reserve_in(Arc::clone(&context), 8192).unwrap();
        assert!(Arc::ptr_eq(&memory.resource_context(), &context));
        assert_eq!(context.live(), 1);
        drop(memory);
        assert_eq!(context.live(), 0);
    }

    #[test]
    fn snapshot_authority_tracks_protect_and_unmap() {
        let memory = Memory::reserve(4096).unwrap();
        let request = hl_memory::MapRequest {
            placement: hl_memory::Placement::Fixed(hl_isa::GuestAddress::new(0)),
            length: 4096,
            alignment: 4096,
            protection: Protection::READ.union(Protection::WRITE),
            backing: hl_memory::Backing::Anonymous {
                identity: 1,
                shared: false,
            },
            backing_offset: 0,
        };
        let map = memory.stage(Operation::Map(0, request)).unwrap();
        memory.commit(&[map]).unwrap();
        assert_eq!(host_permissions(&memory, 0), "rw-p");
        assert!(memory.snapshot_read(0, &mut [0; 1], Protection::READ).is_ok());
        let protect = memory.stage(Operation::Protect(0, 4096, Protection::NONE)).unwrap();
        memory.commit(&[protect]).unwrap();
        assert_eq!(host_permissions(&memory, 0), "---p");
        assert!(memory.snapshot_read(0, &mut [0; 1], Protection::READ).is_err());
        assert!(memory.snapshot_read(0, &mut [0; 1], Protection::NONE).is_err());
        assert_eq!(host_permissions(&memory, 0), "---p");
        let protect = memory.stage(Operation::Protect(0, 4096, Protection::EXECUTE)).unwrap();
        memory.commit(&[protect]).unwrap();
        assert_eq!(host_permissions(&memory, 0), "r--p");
        let protect = memory.stage(Operation::Protect(0, 4096, Protection::READ)).unwrap();
        memory.commit(&[protect]).unwrap();
        assert_eq!(host_permissions(&memory, 0), "r--p");
        let unmap = memory.stage(Operation::Unmap(0, 4096)).unwrap();
        memory.commit(&[unmap]).unwrap();
        assert_eq!(host_permissions(&memory, 0), "---p");
        assert!(memory.snapshot_read(0, &mut [0; 1], Protection::READ).is_err());
        assert!(memory.snapshot_read(0, &mut [0; 1], Protection::NONE).is_err());
    }

    #[test]
    fn dynamic_capacity() {
        // The retained C oracle grows past an initial 4,096-slot table. Rust
        // has no corresponding fixed slot table: each typed arena owns its raw
        // reservation directly. Keep every arena live so this proves concurrent
        // native-resource capacity rather than sequential reuse.
        // SAFETY: sysconf receives no pointer, retains no state, and cannot unwind.
        let page = usize::try_from(unsafe { abi::sysconf(abi::_SC_PAGESIZE) }).unwrap();
        let context = crate::native_host::HostResourceContext::new();
        let mut memories = Vec::with_capacity(4098);
        for _ in 0..4098 {
            memories.push(Memory::reserve_in(Arc::clone(&context), page).unwrap());
        }
        assert_eq!(context.live(), 4098);
        drop(memories);
        assert_eq!(context.live(), 0);
    }
}
