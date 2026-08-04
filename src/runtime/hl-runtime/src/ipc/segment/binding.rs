use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_isa::GuestAddress;

use crate::MappingError;

use super::Mapping;

pub trait CommittedBindingSet {
    fn rollback(self: Box<Self>) -> Result<(), MappingError>;
    fn finish(self: Box<Self>);
}

pub trait PreparedBindingSet<'a> {
    fn commit(self: Box<Self>) -> Result<Box<dyn CommittedBindingSet + 'a>, MappingError>;
}

pub(super) struct PreparedBindings<'a> {
    pub(super) mappings: &'a Mutex<BTreeMap<GuestAddress, Mapping>>,
    pub(super) expected: BTreeMap<GuestAddress, Mapping>,
    pub(super) replacement: BTreeMap<GuestAddress, Mapping>,
}

struct CommittedBindings<'a> {
    mappings: &'a Mutex<BTreeMap<GuestAddress, Mapping>>,
    previous: BTreeMap<GuestAddress, Mapping>,
    published: BTreeMap<GuestAddress, Mapping>,
}

pub(crate) struct OwnedPreparedBindings {
    pub(super) mappings: Arc<Mutex<BTreeMap<GuestAddress, Mapping>>>,
    pub(super) expected: BTreeMap<GuestAddress, Mapping>,
    pub(super) replacement: BTreeMap<GuestAddress, Mapping>,
}

pub(crate) struct OwnedCommittedBindings {
    mappings: Arc<Mutex<BTreeMap<GuestAddress, Mapping>>>,
    previous: BTreeMap<GuestAddress, Mapping>,
    published: BTreeMap<GuestAddress, Mapping>,
}

impl OwnedPreparedBindings {
    pub(crate) fn commit(self) -> Result<OwnedCommittedBindings, MappingError> {
        let mut mappings = self.mappings.lock().unwrap_or_else(|error| error.into_inner());
        if *mappings != self.expected {
            return Err(MappingError::Invariant);
        }
        let previous = std::mem::replace(&mut *mappings, self.replacement);
        let published = mappings.clone();
        drop(mappings);
        Ok(OwnedCommittedBindings {
            mappings: self.mappings,
            previous,
            published,
        })
    }
}

impl OwnedCommittedBindings {
    pub(crate) fn rollback(self) -> Result<(), MappingError> {
        let mut mappings = self.mappings.lock().unwrap_or_else(|error| error.into_inner());
        if *mappings != self.published {
            return Err(MappingError::Invariant);
        }
        *mappings = self.previous;
        Ok(())
    }

    pub(crate) fn finish(self) {}
}

impl<'a> PreparedBindingSet<'a> for PreparedBindings<'a> {
    fn commit(self: Box<Self>) -> Result<Box<dyn CommittedBindingSet + 'a>, MappingError> {
        let mut mappings = self.mappings.lock().unwrap_or_else(|error| error.into_inner());
        if *mappings != self.expected {
            return Err(MappingError::Invariant);
        }
        let previous = std::mem::replace(&mut *mappings, self.replacement);
        let published = mappings.clone();
        drop(mappings);
        Ok(Box::new(CommittedBindings {
            mappings: self.mappings,
            previous,
            published,
        }))
    }
}

impl CommittedBindingSet for CommittedBindings<'_> {
    fn rollback(self: Box<Self>) -> Result<(), MappingError> {
        let mut mappings = self.mappings.lock().unwrap_or_else(|error| error.into_inner());
        if *mappings != self.published {
            return Err(MappingError::Invariant);
        }
        *mappings = self.previous;
        Ok(())
    }

    fn finish(self: Box<Self>) {}
}
