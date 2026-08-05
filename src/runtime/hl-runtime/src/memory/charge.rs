use std::sync::{Arc, Mutex};

use hl_memory::{MemoryError, Region};

use super::AnonymousMemoryAccount;

#[derive(Debug)]
pub enum ChargeTransitionError<E> {
    Limit,
    Operation(E),
}

/// Container-account contribution owned by one concrete address space.
///
/// Byte-exact provenance lives in the mapping ledger and is committed by the
/// same transaction as the host mapping. This lease only serializes the
/// corresponding aggregate account delta; it never owns a second range model.
#[derive(Debug)]
pub struct AnonymousMemoryLease {
    account: Arc<dyn AnonymousMemoryAccount>,
    current: Mutex<u64>,
}

impl AnonymousMemoryLease {
    #[must_use]
    pub fn new(account: Arc<dyn AnonymousMemoryAccount>) -> Self {
        Self {
            account,
            current: Mutex::new(0),
        }
    }

    pub fn restore(account: Arc<dyn AnonymousMemoryAccount>, regions: &[Region]) -> Result<Self, MemoryError> {
        let current = Self::total(regions)?;
        if current != 0 && !account.reserve(current) {
            return Err(MemoryError::ResourceLimit);
        }
        Ok(Self {
            account,
            current: Mutex::new(current),
        })
    }

    pub fn fork(&self, regions: &[Region]) -> Result<Self, MemoryError> {
        Self::restore(Arc::clone(&self.account), regions)
    }

    #[must_use]
    pub fn account(&self) -> Arc<dyn AnonymousMemoryAccount> {
        Arc::clone(&self.account)
    }

    #[must_use]
    pub fn current(&self) -> u64 {
        *self.current.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn transition<T, E>(
        &self,
        target: u64,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, ChargeTransitionError<E>> {
        let mut current = self.current.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let reserve = target.saturating_sub(*current);
        if reserve != 0 && !self.account.reserve(reserve) {
            return Err(ChargeTransitionError::Limit);
        }
        let result = match operation() {
            Ok(result) => result,
            Err(error) => {
                if reserve != 0 {
                    self.account.refund(reserve);
                }
                return Err(ChargeTransitionError::Operation(error));
            }
        };
        if *current > target {
            self.account.refund(*current - target);
        }
        *current = target;
        Ok(result)
    }

    pub fn total(regions: &[Region]) -> Result<u64, MemoryError> {
        regions.iter().try_fold(0_u64, |total, region| {
            total
                .checked_add(region.charge().map_or(0, |range| range.length()))
                .ok_or(MemoryError::ResourceLimit)
        })
    }
}

impl Drop for AnonymousMemoryLease {
    fn drop(&mut self) {
        let current = *self
            .current
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current != 0 {
            self.account.refund(current);
        }
    }
}
