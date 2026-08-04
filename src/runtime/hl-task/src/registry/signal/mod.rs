//! Registry-owned signal delivery, transition planning, and wait reservations.
//!
//! Signal values and queue invariants belong to `crate::signal`. These modules
//! coordinate them with process/thread lifecycle while that state remains under
//! the registry lock. Moving them into the signal model would expose registry
//! internals and reverse the ownership direction.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use super::{State, TaskRegistry};
use crate::{PreparedSignalWait, ThreadId, port::SignalActivity, signal::SignalReservationKey};

mod delivery;
mod plan;
mod wait;

#[cfg(test)]
mod test;

/// Signal coordination that lives outside the main task-state lock.
pub(super) struct Coordination {
    pub(super) activity: Arc<SignalActivity>,
    pub(super) reservations: Arc<Mutex<BTreeSet<SignalReservationKey>>>,
    pub(super) forced: Arc<Mutex<BTreeMap<ThreadId, PreparedSignalWait>>>,
}

impl Coordination {
    pub(super) fn new() -> Self {
        Self {
            activity: Arc::new(SignalActivity::default()),
            reservations: Arc::new(Mutex::new(BTreeSet::new())),
            forced: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}
