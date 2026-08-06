//! Bounded devpts namespace owning pair indices and controlling-terminal claims.
use crate::pty::{MAXIMUM_PAIRS, Pair, PairId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogError {
    Capacity,
    NotFound,
    Stale,
    WrongEndpoint,
}
#[derive(Default)]
struct CatalogState {
    pairs: BTreeMap<u16, Arc<Pair>>,
    generations: BTreeMap<u16, u64>,
    controlling: BTreeMap<u32, PairId>,
}

/// Bounded devpts namespace whose indices remain stable until pair retirement.
#[derive(Default)]
pub struct Catalog {
    state: Mutex<CatalogState>,
}

impl Catalog {
    pub fn acquire(&self, session: u32, pair: PairId) -> Result<(), CatalogError> {
        self.acquire_changed(session, pair).map(|_| ())
    }

    /// Acquires a controlling terminal and reports whether this call created
    /// the session binding, so a cross-domain caller can compensate safely.
    pub fn acquire_changed(&self, session: u32, pair: PairId) -> Result<bool, CatalogError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.pairs.get(&pair.index).is_none_or(|current| current.id() != pair) {
            return Err(CatalogError::Stale);
        }
        if state.controlling.get(&session).copied() == Some(pair) {
            return Ok(false);
        }
        if state.controlling.contains_key(&session) || state.controlling.values().any(|owner| *owner == pair) {
            return Err(CatalogError::WrongEndpoint);
        }
        state.controlling.insert(session, pair);
        Ok(true)
    }

    pub fn controlling(&self, session: u32) -> Result<Arc<Pair>, CatalogError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = *state.controlling.get(&session).ok_or(CatalogError::NotFound)?;
        state
            .pairs
            .get(&id.index)
            .filter(|pair| pair.id() == id)
            .cloned()
            .ok_or(CatalogError::Stale)
    }

    pub fn detach(&self, session: u32, pair: PairId) -> Result<(), CatalogError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.controlling.get(&session).copied() != Some(pair) {
            return Err(CatalogError::NotFound);
        }
        state.controlling.remove(&session);
        if let Some(current) = state.pairs.get(&pair.index).filter(|current| current.id() == pair) {
            current.clear_foreground();
        }
        Ok(())
    }

    pub fn controlling_session(&self, pair: PairId) -> Option<u32> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .controlling
            .iter()
            .find_map(|(session, owner)| (*owner == pair).then_some(*session))
    }

    pub fn allocate(&self) -> Result<Arc<Pair>, CatalogError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = (0..MAXIMUM_PAIRS)
            .find(|index| !state.pairs.contains_key(index))
            .ok_or(CatalogError::Capacity)?;
        let generation = state
            .generations
            .get(&index)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(CatalogError::Capacity)?;
        let id = PairId { index, generation };
        let pair = Arc::new(Pair::new(id));
        state.generations.insert(index, generation);
        state.pairs.insert(index, Arc::clone(&pair));
        Ok(pair)
    }

    pub fn get(&self, id: PairId) -> Result<Arc<Pair>, CatalogError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pair = state.pairs.get(&id.index).ok_or(CatalogError::NotFound)?;
        if pair.id() != id {
            return Err(CatalogError::Stale);
        }
        Ok(Arc::clone(pair))
    }

    pub fn current(&self, index: u16) -> Result<Arc<Pair>, CatalogError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pairs
            .get(&index)
            .cloned()
            .ok_or(CatalogError::NotFound)
    }

    #[must_use]
    pub fn indices(&self) -> Vec<u16> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pairs
            .keys()
            .copied()
            .collect()
    }

    pub fn retire(&self, id: PairId) -> Result<(), CatalogError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pair = state.pairs.get(&id.index).ok_or(CatalogError::NotFound)?;
        if pair.id() != id {
            return Err(CatalogError::Stale);
        }
        pair.retire();
        state.pairs.remove(&id.index);
        state.controlling.retain(|_, pair| *pair != id);
        Ok(())
    }
}
