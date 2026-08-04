use std::sync::{Arc, Mutex};

use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{Backing, MapRequest, MappingCoordinator, MappingHost, Placement, Protection};

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

pub trait BrkAccount: std::fmt::Debug + Send + Sync {
    fn reserve(&self, bytes: u64) -> bool;
    fn refund(&self, bytes: u64);
    fn current(&self) -> u64;
}

#[derive(Clone, Copy, Debug)]
struct BrkState {
    snapshot: BrkSnapshot,
    uncharged_ceiling: GuestAddress,
    charged: u64,
}

#[derive(Debug)]
pub struct BrkRegion<H: MappingHost> {
    memory: Arc<MappingCoordinator<H>>,
    state: Mutex<BrkState>,
    account: Option<Arc<dyn BrkAccount>>,
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
            state: Mutex::new(BrkState { snapshot, uncharged_ceiling: snapshot.lower, charged: 0 }),
            account: None,
        };
        let end = Self::mapped_end(snapshot.current)?;
        if end > snapshot.lower {
            region.memory.map(Self::request(
                snapshot.lower,
                end.get() - snapshot.lower.get(),
                snapshot,
            ))?;
        }
        Ok(region)
    }

    pub fn restore(memory: Arc<MappingCoordinator<H>>, snapshot: BrkSnapshot) -> Result<Self, hl_memory::MemoryError> {
        Self::validate(snapshot)?;
        Ok(Self {
            memory,
            state: Mutex::new(BrkState { snapshot, uncharged_ceiling: snapshot.current, charged: 0 }),
            account: None,
        })
    }

    #[must_use]
    pub fn with_account(mut self, account: Arc<dyn BrkAccount>) -> Self {
        self.account = Some(account);
        self
    }

    pub fn fork(&self, memory: Arc<MappingCoordinator<H>>) -> Result<Self, hl_memory::MemoryError> {
        let mut child = Self::restore(memory, self.snapshot())?;
        child.account = self.account.clone();
        Ok(child)
    }

    #[must_use]
    pub fn account(&self) -> Option<Arc<dyn BrkAccount>> {
        self.account.clone()
    }

    #[must_use]
    pub fn set(&self, requested: u64) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
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
        let target_charge = requested.get().saturating_sub(state.uncharged_ceiling.get());
        let reserve = target_charge.saturating_sub(state.charged);
        let reserved = reserve == 0 || self.account.as_ref().is_none_or(|account| account.reserve(reserve));
        if !reserved {
            return old.get();
        }
        let transition = if new_end > old_end {
            self.memory
                .map(Self::request(old_end, new_end.get() - old_end.get(), state.snapshot))
                .map(|_| ())
        } else if new_end < old_end {
            AddressRange::nonempty(new_end, old_end.get() - new_end.get())
                .map_err(|_| hl_memory::MemoryError::AddressOverflow)
                .and_then(|range| self.memory.unmap(range))
        } else {
            Ok(())
        };
        if transition.is_ok() {
            if target_charge < state.charged {
                let refund = state.charged - target_charge;
                if let Some(account) = &self.account {
                    account.refund(refund);
                }
            }
            state.charged = target_charge;
            state.snapshot.current = requested;
        } else if reserve != 0 {
            if let Some(account) = &self.account {
                account.refund(reserve);
            }
        }
        state.snapshot.current.get()
    }

    #[must_use]
    pub fn snapshot(&self) -> BrkSnapshot {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).snapshot
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
}

impl<H: MappingHost> Drop for BrkRegion<H> {
    fn drop(&mut self) {
        let charged = self.state.lock().unwrap_or_else(|error| error.into_inner()).charged;
        if charged != 0 {
            if let Some(account) = &self.account {
                account.refund(charged);
            }
        }
    }
}
