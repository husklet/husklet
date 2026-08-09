#[path = "host_access.rs"]
mod access;

pub use access::{WriteSpanTransaction, WriteTransaction};

use super::plan::{Batch, PlannedOperation};
use super::port::Host;
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
    pin: SharedBackingPin,
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
            if overlap_first >= overlap_last {
                continue;
            }
            let address = region.range().start().get() + (overlap_first - alias_first);
            if let Ok(range) = AddressRange::nonempty(GuestAddress::new(address), overlap_last - overlap_first) {
                ranges.push(range);
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
                    .map(|(region, pin)| PinnedRegion { region, pin })
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
        if let Ok(address) = result {
            let mapped = AddressRange::nonempty(address, request.length).ok();
            self.publish_transition_ranges(&mut transition, self.ledger.generation(), mapped);
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
            self.publish_transition_ranges(&mut transition, self.ledger.generation(), [range]);
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
            self.publish_transition_ranges(&mut transition, self.ledger.generation(), [range]);
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
        let mut touched = Vec::new();
        let result = self.ledger.batch_transaction(&batch.operations, |plan, regions| {
            let pins = self.prepare_pins(regions)?;
            let reservations = self.stage_plan(plan)?;
            if let Err(error) = self.host.commit(&reservations) {
                self.rollback(&reservations);
                return Err(error);
            }
            touched = plan.iter().filter_map(PlannedOperation::range).collect();
            self.publish_pins(regions, pins);
            Ok(())
        });
        if result.is_ok() {
            self.publish_transition_ranges(&mut transition, self.ledger.generation(), touched);
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
                    .map(|(region, pin)| PinnedRegion { region, pin })
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
        PinnedRegion { region, pin }
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
