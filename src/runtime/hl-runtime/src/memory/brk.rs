use std::sync::{Arc, Mutex};

use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{
    Backing, MapRequest, MappingBatch, MappingCoordinator, MappingHost, MappingOperation, Placement, Protection,
};

use super::{AnonymousMemoryLease, charge::ChargeTransitionError};

const PAGE: u64 = 4096;
pub const BRK_BACKING_IDENTITY: u64 = 0x4252_4b;

/// Byte-exact process break and its page-rounded mapped extent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrkSnapshot {
    pub lower: GuestAddress,
    pub current: GuestAddress,
    pub upper: GuestAddress,
    pub backing_identity: u64,
}

pub trait AnonymousMemoryAccount: std::fmt::Debug + Send + Sync {
    fn reserve(&self, bytes: u64) -> bool;
    fn refund(&self, bytes: u64);
    fn current(&self) -> u64;
}

#[derive(Clone, Copy, Debug)]
struct BrkState {
    snapshot: BrkSnapshot,
}

#[derive(Debug)]
pub struct BrkRegion<H: MappingHost> {
    memory: Arc<MappingCoordinator<H>>,
    state: Mutex<BrkState>,
    lease: Option<Arc<AnonymousMemoryLease>>,
}

impl<H: MappingHost> BrkRegion<H> {
    fn validate(snapshot: BrkSnapshot) -> Result<(), hl_memory::MemoryError> {
        if !snapshot.lower.is_page_aligned(hl_isa::GuestPageSize::LINUX)
            || !snapshot.upper.is_page_aligned(hl_isa::GuestPageSize::LINUX)
            || snapshot.current < snapshot.lower
            || snapshot.current > snapshot.upper
        {
            return Err(hl_memory::MemoryError::InvariantViolation);
        }
        Ok(())
    }

    pub fn new(memory: Arc<MappingCoordinator<H>>, snapshot: BrkSnapshot) -> Result<Self, hl_memory::MemoryError> {
        Self::validate(snapshot)?;
        let region = Self {
            memory,
            state: Mutex::new(BrkState { snapshot }),
            lease: None,
        };
        let end = Self::mapped_end(snapshot.current)?;
        if end > snapshot.lower {
            region.memory.map_charged(
                Self::request(snapshot.lower, end.get() - snapshot.lower.get(), snapshot),
                snapshot.current.get() - snapshot.lower.get(),
            )?;
        }
        Ok(region)
    }

    pub fn restore(memory: Arc<MappingCoordinator<H>>, snapshot: BrkSnapshot) -> Result<Self, hl_memory::MemoryError> {
        Self::validate(snapshot)?;
        Ok(Self {
            memory,
            state: Mutex::new(BrkState { snapshot }),
            lease: None,
        })
    }

    #[must_use]
    pub fn with_account(mut self, account: Arc<dyn AnonymousMemoryAccount>) -> Result<Self, hl_memory::MemoryError> {
        self.lease = Some(Arc::new(AnonymousMemoryLease::restore(
            account,
            &self.memory.ledger().regions(),
        )?));
        Ok(self)
    }

    pub fn fork(&self, memory: Arc<MappingCoordinator<H>>) -> Result<Self, hl_memory::MemoryError> {
        let snapshot = self.snapshot();
        let mut child = Self::restore(Arc::clone(&memory), snapshot)?;
        let end = Self::mapped_end(snapshot.current)?;
        if end > snapshot.lower && memory.ledger().resolve(snapshot.lower, Protection::NONE).is_none() {
            memory.map_inherited_reserved(
                Self::request(snapshot.lower, end.get() - snapshot.lower.get(), snapshot),
                snapshot.current.get() - snapshot.lower.get(),
                true,
            )?;
        }
        if let Some(lease) = &self.lease {
            child.lease = Some(Arc::new(lease.fork(&memory.ledger().regions())?));
        }
        Ok(child)
    }

    #[must_use]
    pub fn lease(&self) -> Option<Arc<AnonymousMemoryLease>> {
        self.lease.clone()
    }

    #[must_use]
    pub fn set(&self, requested: u64) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let old = state.snapshot.current;
        if requested == 0 {
            return old.get();
        }
        let requested = GuestAddress::new(requested);
        if requested < state.snapshot.lower || requested > state.snapshot.upper {
            return old.get();
        }
        let Ok(old_end) = Self::mapped_end(old) else {
            return old.get();
        };
        let Ok(new_end) = Self::mapped_end(requested) else {
            return old.get();
        };
        let before = AnonymousMemoryLease::total(&self.memory.ledger().regions()).unwrap_or(u64::MAX);
        let mut batch = MappingBatch::new();
        let target = if requested > old {
            if new_end > old_end {
                batch.push(MappingOperation::MapCharged(
                    Self::request(old_end, new_end.get() - old_end.get(), state.snapshot),
                    0,
                ));
            }
            let Ok(added) = AddressRange::nonempty(old, requested.get() - old.get()) else { return old.get() };
            let overlap = Self::charged_overlap(&self.memory.ledger().regions(), added);
            batch.push(MappingOperation::Charge(added));
            before.saturating_add(added.length().saturating_sub(overlap))
        } else if requested < old {
            let Ok(removed) = AddressRange::nonempty(requested, old_end.get() - requested.get()) else {
                return old.get()
            };
            let overlap = Self::charged_overlap(&self.memory.ledger().regions(), removed);
            if new_end < old_end {
                let Ok(range) = AddressRange::nonempty(new_end, old_end.get() - new_end.get()) else {
                    return old.get();
                };
                batch.push(MappingOperation::Unmap(range));
            }
            if requested < new_end.min(old) {
                let Ok(range) = AddressRange::nonempty(requested, new_end.min(old).get() - requested.get()) else {
                    return old.get();
                };
                batch.push(MappingOperation::Uncharge(range));
            }
            before.saturating_sub(overlap)
        } else {
            return old.get();
        };
        let operation = || self.memory.apply(&batch).map(|_| ());
        let transition = match &self.lease {
            Some(lease) => lease.transition(target, operation),
            None => operation().map_err(ChargeTransitionError::Operation),
        };
        if transition.is_ok() {
            state.snapshot.current = requested;
        }
        state.snapshot.current.get()
    }

    #[must_use]
    pub fn snapshot(&self) -> BrkSnapshot {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot
    }

    fn request(start: GuestAddress, length: u64, state: BrkSnapshot) -> MapRequest {
        MapRequest {
            placement: Placement::FixedNoReplace(start),
            length,
            alignment: PAGE,
            protection: Protection::READ.union(Protection::WRITE),
            backing: Backing::Anonymous {
                identity: state.backing_identity,
                shared: false,
            },
            backing_offset: start.get() - state.lower.get(),
        }
    }

    fn mapped_end(value: GuestAddress) -> Result<GuestAddress, hl_memory::MemoryError> {
        let rounded = value
            .get()
            .checked_add(PAGE - 1)
            .ok_or(hl_memory::MemoryError::AddressOverflow)?
            & !(PAGE - 1);
        Ok(GuestAddress::new(rounded))
    }

    fn charged_overlap(regions: &[hl_memory::Region], range: AddressRange) -> u64 {
        regions
            .iter()
            .filter_map(|region| region.charge())
            .map(|charge| {
                let first = charge.start().max(range.start());
                let last = charge.end().min(range.end());
                last.get().saturating_sub(first.get())
            })
            .sum()
    }
}
