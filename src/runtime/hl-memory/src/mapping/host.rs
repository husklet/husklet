use super::plan::{Batch, PlannedOperation};
use super::port::{Host, MemoryAccessHost};
use super::transition::{NoopObserver, TransitionObserver};
use crate::{
    Backing, MapRequest, MemoryError, MemoryLedger, MemoryLedgerSnapshot, Protection, Region, SharedBackingPin,
    SharedObjectStore,
};
use hl_isa::{AddressRange, GuestAddress};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Debug)]
pub(crate) struct AccessState<H> {
    pub(crate) host: H,
    pub(crate) epoch: AtomicU64,
    pub(crate) executable: crate::executable::ExecutableVersions,
    pub(crate) reservations: Arc<crate::ReservationEpochs>,
}

impl<H: Host> AccessState<H> {
    fn new(host: H) -> Self {
        let reservations = host
            .reservation_epochs()
            .unwrap_or_else(|| Arc::new(crate::ReservationEpochs::default()));
        Self {
            host,
            epoch: AtomicU64::new(0),
            executable: crate::executable::ExecutableVersions::default(),
            reservations,
        }
    }
}

impl<H> std::ops::Deref for AccessState<H> {
    type Target = H;
    fn deref(&self) -> &Self::Target {
        &self.host
    }
}

pub struct WriteTransaction<H: MemoryAccessHost> {
    pub(super) access: Arc<AccessState<H>>,
    pub(super) reservation: u64,
    pub(crate) generation: u64,
    pub(crate) observed_epoch: Option<u64>,
    pub(super) range: AddressRange,
    pub(super) committed: bool,
}

pub struct WriteSpanTransaction<H: MemoryAccessHost> {
    transactions: Vec<WriteTransaction<H>>,
    length: usize,
}

impl<H: MemoryAccessHost> Drop for WriteTransaction<H> {
    fn drop(&mut self) {
        if !self.committed {
            self.access.rollback_write(self.reservation);
        }
    }
}

#[derive(Debug)]
pub struct Coordinator<H> {
    pub(crate) ledger: MemoryLedger,
    pub(crate) host: Arc<AccessState<H>>,
    pub(crate) shared: Option<Arc<SharedObjectStore>>,
    pub(crate) pins: Mutex<Vec<PinnedRegion>>,
    pub(crate) transaction: Mutex<()>,
    pub(crate) mapping_requests: Arc<AtomicU64>,
    pub(crate) activity: Arc<crate::CheckpointActivity>,
    address_space: Option<crate::AddressSpaceId>,
    pub(crate) observer: RwLock<Arc<dyn TransitionObserver>>,
}

#[derive(Debug)]
pub(crate) struct PinnedRegion {
    region: Region,
    _pin: SharedBackingPin,
}

impl<H: Host> Coordinator<H> {
    pub(crate) fn request_mapping_change(&self) {
        let _ = self
            .mapping_requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |requests| {
                Some(requests.saturating_add(1))
            });
    }

    pub(crate) fn executable_write_ranges(
        &self,
        address: GuestAddress,
        resolution: crate::Resolution,
        length: u64,
    ) -> Vec<AddressRange> {
        let mut ranges = Vec::new();
        if resolution.region.protection().contains(Protection::EXECUTE)
            && let Ok(range) = AddressRange::nonempty(address, length)
        {
            ranges.push(range);
        }
        let Backing::Shared(source) = resolution.region.backing() else {
            return ranges;
        };
        let Some(first) = source.offset.checked_add(resolution.backing_offset) else {
            return vec![resolution.region.range()];
        };
        let Some(last) = first.checked_add(length) else {
            return vec![resolution.region.range()];
        };
        for region in self.ledger.regions() {
            let Backing::Shared(alias) = region.backing() else {
                continue;
            };
            if alias.object != source.object || !region.protection().contains(Protection::EXECUTE) {
                continue;
            }
            let Some(alias_first) = alias.offset.checked_add(region.backing_offset()) else {
                ranges.push(region.range());
                continue;
            };
            let Some(alias_last) = alias_first.checked_add(region.range().length()) else {
                ranges.push(region.range());
                continue;
            };
            let overlap_first = first.max(alias_first);
            let overlap_last = last.min(alias_last);
            if overlap_first < overlap_last {
                let address = region.range().start().get() + (overlap_first - alias_first);
                if let Ok(range) = AddressRange::nonempty(GuestAddress::new(address), overlap_last - overlap_first) {
                    ranges.push(range);
                }
            }
        }
        ranges
    }

    #[must_use]
    pub fn shared_objects(&self) -> Option<Arc<SharedObjectStore>> {
        self.shared.clone()
    }

    pub fn new(host: H) -> Self {
        Self {
            ledger: MemoryLedger::new(),
            host: Arc::new(AccessState::new(host)),
            shared: None,
            pins: Mutex::new(Vec::new()),
            transaction: Mutex::new(()),
            mapping_requests: Arc::new(AtomicU64::new(0)),
            activity: Arc::new(crate::CheckpointActivity::default()),
            address_space: None,
            observer: RwLock::new(Arc::new(NoopObserver)),
        }
    }

    pub fn with_shared(host: H, shared: Arc<SharedObjectStore>) -> Self {
        Self {
            ledger: MemoryLedger::new(),
            host: Arc::new(AccessState::new(host)),
            shared: Some(shared),
            pins: Mutex::new(Vec::new()),
            transaction: Mutex::new(()),
            mapping_requests: Arc::new(AtomicU64::new(0)),
            activity: Arc::new(crate::CheckpointActivity::default()),
            address_space: None,
            observer: RwLock::new(Arc::new(NoopObserver)),
        }
    }

    pub fn restore(
        host: H,
        shared: Arc<SharedObjectStore>,
        snapshot: MemoryLedgerSnapshot,
    ) -> Result<Self, MemoryError> {
        let ledger = MemoryLedger::restore(snapshot)?;
        let regions = ledger.regions();
        let pins = Self::pin_regions(Some(&shared), &regions, &[])?;
        Ok(Self {
            ledger,
            host: Arc::new(AccessState::new(host)),
            shared: Some(shared),
            pins: Mutex::new(
                pins.into_iter()
                    .map(|(region, pin)| PinnedRegion { region, _pin: pin })
                    .collect(),
            ),
            transaction: Mutex::new(()),
            mapping_requests: Arc::new(AtomicU64::new(0)),
            activity: Arc::new(crate::CheckpointActivity::default()),
            address_space: None,
            observer: RwLock::new(Arc::new(NoopObserver)),
        })
    }

    pub fn with_address_space(host: H, address_space: crate::AddressSpaceId) -> Self {
        let mut coordinator = Self::new(host);
        coordinator.address_space = Some(address_space);
        coordinator
    }

    pub fn with_shared_space(host: H, shared: Arc<SharedObjectStore>, address_space: crate::AddressSpaceId) -> Self {
        let mut coordinator = Self::with_shared(host, shared);
        coordinator.address_space = Some(address_space);
        coordinator
    }

    #[must_use]
    pub const fn address_space(&self) -> Option<crate::AddressSpaceId> {
        self.address_space
    }

    pub fn map(&self, request: MapRequest) -> Result<GuestAddress, MemoryError> {
        self.map_with(request, 0, false, false)
    }

    pub fn map_charged(&self, request: MapRequest, charge: u64) -> Result<GuestAddress, MemoryError> {
        self.map_with(request, charge, true, false)
    }

    pub fn map_inherited(&self, request: MapRequest) -> Result<GuestAddress, MemoryError> {
        self.map_with(request, 0, false, true)
    }

    pub fn map_inherited_reserved(
        &self,
        request: MapRequest,
        charge: u64,
        reserved: bool,
    ) -> Result<GuestAddress, MemoryError> {
        self.map_with(request, charge, reserved, true)
    }

    fn map_with(
        &self,
        request: MapRequest,
        charge: u64,
        reserved: bool,
        inherited: bool,
    ) -> Result<GuestAddress, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut transition = self.transition();
        let result = self
            .ledger
            .map_transaction(request, charge, reserved, |address, regions| {
                let pins = self.prepare_pins_with(regions, inherited)?;
                let reservation = self.host.stage_map(address, request)?;
                self.finish(&[reservation])?;
                self.publish_pins(regions, pins);
                Ok(())
            });
        if result.is_ok() {
            self.publish_transition(&mut transition, self.ledger.generation());
        }
        result
    }

    pub fn unmap(&self, range: AddressRange) -> Result<(), MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut transition = self.transition();
        let result = self.ledger.unmap_transaction(range, |regions| {
            let pins = self.prepare_pins(regions)?;
            let reservation = self.host.stage_unmap(range)?;
            self.finish(&[reservation])?;
            self.publish_pins(regions, pins);
            Ok(())
        });
        if result.is_ok() {
            self.publish_transition(&mut transition, self.ledger.generation());
        }
        result
    }

    pub fn protect(&self, range: AddressRange, protection: Protection) -> Result<(), MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut transition = self.transition();
        let result = self.ledger.protect_transaction(range, protection, |regions| {
            let pins = self.prepare_pins(regions)?;
            let reservation = self.host.stage_protect(range, protection)?;
            self.finish(&[reservation])?;
            self.publish_pins(regions, pins);
            Ok(())
        });
        if result.is_ok() {
            self.publish_transition(&mut transition, self.ledger.generation());
        }
        result
    }

    pub fn apply(&self, batch: &Batch) -> Result<Vec<GuestAddress>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut transition = self.transition();
        let result = self.ledger.batch_transaction(&batch.operations, |plan, regions| {
            let pins = self.prepare_pins(regions)?;
            let reservations = self.stage_plan(plan)?;
            if let Err(error) = self.host.commit(&reservations) {
                self.rollback(&reservations);
                return Err(error);
            }
            self.publish_pins(regions, pins);
            Ok(())
        });
        if result.is_ok() {
            self.publish_transition(&mut transition, self.ledger.generation());
        }
        result
    }

    fn stage_plan(&self, plan: &[PlannedOperation]) -> Result<Vec<u64>, MemoryError> {
        let mut reservations = Vec::new();
        for operation in plan {
            let staged = match operation {
                PlannedOperation::Map(address, request) => self.host.stage_map(*address, *request),
                PlannedOperation::Unmap(range) => self.host.stage_unmap(*range),
                PlannedOperation::Protect(range, protection) => self.host.stage_protect(*range, *protection),
            };
            match staged {
                Ok(reservation) => reservations.push(reservation),
                Err(error) => {
                    self.rollback(&reservations);
                    return Err(error);
                }
            }
        }
        Ok(reservations)
    }

    pub fn ledger(&self) -> &MemoryLedger {
        &self.ledger
    }

    /// Reports logical guest coverage without consulting retained host mappings.
    #[must_use]
    pub fn contains(&self, range: AddressRange) -> bool {
        self.ledger.contains(range)
    }

    pub fn snapshot(&self) -> MemoryLedgerSnapshot {
        let _admission = self.activity.admit();
        self.ledger.snapshot()
    }

    pub fn fork_restore(&self, host: H) -> Result<Self, MemoryError> {
        let snapshot = self.snapshot();
        let ledger = MemoryLedger::restore(snapshot)?;
        let regions = ledger.regions();
        let pins = Self::pin_regions(self.shared.as_ref(), &regions, &[])?;
        Ok(Self {
            ledger,
            host: Arc::new(AccessState::new(host)),
            shared: self.shared.clone(),
            pins: Mutex::new(
                pins.into_iter()
                    .map(|(region, pin)| PinnedRegion { region, _pin: pin })
                    .collect(),
            ),
            transaction: Mutex::new(()),
            mapping_requests: Arc::new(AtomicU64::new(0)),
            activity: Arc::new(crate::CheckpointActivity::default()),
            address_space: None,
            observer: RwLock::new(Arc::clone(
                &self.observer.read().unwrap_or_else(std::sync::PoisonError::into_inner),
            )),
        })
    }

    fn pin_regions(
        store: Option<&Arc<SharedObjectStore>>,
        regions: &[Region],
        retained: &[Region],
    ) -> Result<Vec<(Region, SharedBackingPin)>, MemoryError> {
        let mut pins = Vec::new();
        for region in regions {
            let Backing::Shared(reference) = region.backing() else {
                continue;
            };
            if retained.contains(region) {
                continue;
            }
            let store = store.ok_or(MemoryError::Shared(crate::SharedError::NotFound))?;
            let end = region
                .backing_offset()
                .checked_add(region.range().length())
                .ok_or(MemoryError::BackingOverflow)?;
            if end > reference.length {
                return Err(MemoryError::Shared(crate::SharedError::Range));
            }
            pins.push((
                *region,
                store.pin_backing(
                    reference,
                    reference.write_shared && region.protection().contains(Protection::WRITE),
                )?,
            ));
        }
        Ok(pins)
    }

    pub(crate) fn prepare_pins(&self, regions: &[Region]) -> Result<Vec<(Region, SharedBackingPin)>, MemoryError> {
        self.prepare_pins_with(regions, false)
    }

    fn prepare_pins_with(
        &self,
        regions: &[Region],
        inherited: bool,
    ) -> Result<Vec<(Region, SharedBackingPin)>, MemoryError> {
        let retained: Vec<_> = self
            .pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|entry| entry.region)
            .collect();
        if !inherited {
            return Self::pin_regions(self.shared.as_ref(), regions, &retained);
        }
        let mut pins = Vec::new();
        for region in regions {
            let Backing::Shared(reference) = region.backing() else {
                continue;
            };
            if retained.contains(region) {
                continue;
            }
            let store = self
                .shared
                .as_ref()
                .ok_or(MemoryError::Shared(crate::SharedError::NotFound))?;
            pins.push((
                *region,
                store.pin_inherited(
                    reference,
                    reference.write_shared && region.protection().contains(Protection::WRITE),
                )?,
            ));
        }
        Ok(pins)
    }

    pub(crate) fn publish_pins(&self, regions: &[Region], mut new: Vec<(Region, SharedBackingPin)>) {
        let mut live = self.pins.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut old = std::mem::take(&mut *live);
        for region in regions {
            if matches!(region.backing(), Backing::Shared(_)) {
                live.push(Self::committed_pin(*region, &mut old, &mut new));
            }
        }
    }

    fn committed_pin(
        region: Region,
        old: &mut Vec<PinnedRegion>,
        new: &mut Vec<(Region, SharedBackingPin)>,
    ) -> PinnedRegion {
        if let Some(index) = old.iter().position(|entry| entry.region == region) {
            return old.swap_remove(index);
        }
        let index = new
            .iter()
            .position(|(candidate, _)| *candidate == region)
            .expect("every new shared region was pinned before host staging");
        let (region, pin) = new.swap_remove(index);
        PinnedRegion { region, _pin: pin }
    }

    fn finish(&self, reservations: &[u64]) -> Result<(), MemoryError> {
        if let Err(error) = self.host.commit(reservations) {
            self.rollback(reservations);
            return Err(error);
        }
        Ok(())
    }

    fn rollback(&self, reservations: &[u64]) {
        for reservation in reservations.iter().rev() {
            self.host.rollback(*reservation);
        }
    }
}

impl<H: MemoryAccessHost> Coordinator<H> {
    pub(crate) fn retained_pin(&self, region: Region) -> Result<SharedBackingPin, MemoryError> {
        self.pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|entry| entry.region == region)
            .map(|entry| entry._pin.retain())
            .ok_or(MemoryError::InvariantViolation)?
            .map_err(MemoryError::from)
    }

    pub fn read(&self, address: GuestAddress, output: &mut [u8], access: Protection) -> Result<(), MemoryError> {
        let length = u64::try_from(output.len()).map_err(|_| MemoryError::AddressOverflow)?;
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        let resolution = self
            .ledger
            .resolve(address, access)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        if matches!(resolution.region.backing(), Backing::Shared(_)) {
            let pin = self.retained_pin(resolution.region)?;
            let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
            return pin.read(offset, output).map_err(MemoryError::from);
        }
        if let Backing::File { identity, .. } = resolution.region.backing() {
            self.host
                .host
                .validate_file(identity, resolution.backing_offset, length, address)?;
        }
        self.host.read(range, output, access)
    }
    pub fn prepare_write(&self, address: GuestAddress, length: u64) -> Result<WriteTransaction<H>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        let resolution = self
            .ledger
            .resolve(address, Protection::WRITE)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        if let Backing::File { identity, .. } = resolution.region.backing() {
            self.host
                .host
                .validate_file(identity, resolution.backing_offset, length, address)?;
        }
        let generation = self.ledger.generation();
        let reservation = self.host.prepare_write(range)?;
        Ok(WriteTransaction {
            access: Arc::clone(&self.host),
            reservation,
            generation,
            observed_epoch: None,
            range,
            committed: false,
        })
    }

    pub fn prepare_write_spans(
        &self,
        address: GuestAddress,
        length: u64,
    ) -> Result<WriteSpanTransaction<H>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        address.get().checked_add(length).ok_or(MemoryError::AddressOverflow)?;
        if length == 0 {
            return Err(MemoryError::EmptyRange);
        }
        let mut transactions = Vec::new();
        let mut copied = 0_u64;
        while copied < length {
            let current = address.get().checked_add(copied).ok_or(MemoryError::AddressOverflow)?;
            let available = self.access_prefix(GuestAddress::new(current), length - copied, Protection::WRITE)?;
            if available == 0 {
                return Err(MemoryError::NoAddressSpace);
            }
            transactions.push(self.prepare_write(GuestAddress::new(current), available)?);
            copied = copied.checked_add(available).ok_or(MemoryError::AddressOverflow)?;
        }
        Ok(WriteSpanTransaction {
            transactions,
            length: usize::try_from(length).map_err(|_| MemoryError::AddressOverflow)?,
        })
    }

    pub fn commit_write_spans(&self, mut prepared: WriteSpanTransaction<H>, input: &[u8]) -> Result<u64, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if input.len() != prepared.length
            || prepared.transactions.is_empty()
            || prepared
                .transactions
                .iter()
                .any(|span| span.generation != self.ledger.generation())
        {
            return Err(MemoryError::InvariantViolation);
        }
        let mut executable = Vec::new();
        let mut copied = 0_usize;
        for span in &prepared.transactions {
            let length = usize::try_from(span.range.length()).map_err(|_| MemoryError::AddressOverflow)?;
            let end = copied.checked_add(length).ok_or(MemoryError::AddressOverflow)?;
            if end > input.len() {
                return Err(MemoryError::InvariantViolation);
            }
            let resolution = self
                .ledger
                .resolve(span.range.start(), Protection::WRITE)
                .ok_or(MemoryError::NoAddressSpace)?;
            if resolution.contiguous < span.range.length() {
                return Err(MemoryError::NoAddressSpace);
            }
            copied = end;
        }
        if copied != input.len() {
            return Err(MemoryError::InvariantViolation);
        }
        let mut copied = 0_usize;
        for span in &mut prepared.transactions {
            let length = usize::try_from(span.range.length()).map_err(|_| MemoryError::AddressOverflow)?;
            let end = copied + length;
            let resolution = self
                .ledger
                .resolve(span.range.start(), Protection::WRITE)
                .ok_or(MemoryError::NoAddressSpace)?;
            if let Backing::Shared(_) = resolution.region.backing() {
                let pin = self.retained_pin(resolution.region)?;
                let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
                pin.write(offset, &input[copied..end])?;
            }
            if let Err(error) = self.host.commit_write(span.reservation, &input[copied..end]) {
                self.host.executable.publish(executable);
                return Err(error);
            }
            span.committed = true;
            executable.extend(self.executable_write_ranges(span.range.start(), resolution, span.range.length()));
            self.invalidate_exclusive(span.range)?;
            copied = end;
        }
        self.host.executable.publish(executable);
        Ok(self.host.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1))
    }

    pub fn commit_write(&self, mut prepared: WriteTransaction<H>, input: &[u8]) -> Result<u64, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.request_mapping_change();
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if input.len() as u64 != prepared.range.length()
            || self.ledger.generation() != prepared.generation
            || prepared
                .observed_epoch
                .is_some_and(|epoch| self.observer_epoch() != epoch)
        {
            return Err(MemoryError::InvariantViolation);
        }
        let resolution = self
            .ledger
            .resolve(prepared.range.start(), Protection::WRITE)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < prepared.range.length() {
            return Err(MemoryError::NoAddressSpace);
        }
        let shared = match resolution.region.backing() {
            Backing::Shared(_) => {
                let pin = self.retained_pin(resolution.region)?;
                let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
                let mut previous = vec![0; input.len()];
                pin.read(offset, &mut previous)?;
                pin.write(offset, input)?;
                Some((pin, offset, previous))
            }
            _ => None,
        };
        if let Err(error) = self.host.commit_write(prepared.reservation, input) {
            if let Some((pin, offset, previous)) = shared {
                pin.write(offset, &previous)?;
            }
            return Err(error);
        }
        prepared.committed = true;
        self.invalidate_exclusive(prepared.range)?;
        self.host.executable.publish(self.executable_write_ranges(
            prepared.range.start(),
            resolution,
            prepared.range.length(),
        ));
        let epoch = self.host.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        Ok(epoch)
    }
}
