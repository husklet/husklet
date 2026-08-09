use hl_isa::{AddressRange, GuestAddress};

use crate::{MapRequest, Protection};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Map(MapRequest),
    MapCharged(MapRequest, u64),
    Unmap(AddressRange),
    Protect(AddressRange, Protection),
    Replace(MapRequest),
    ReplaceCharged(MapRequest, u64),
    Charge(AddressRange),
    Uncharge(AddressRange),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlannedOperation {
    Map(GuestAddress, MapRequest),
    Unmap(AddressRange),
    Protect(AddressRange, Protection),
}

impl PlannedOperation {
    /// The guest range this operation changes the meaning of.
    pub(crate) fn range(&self) -> Option<AddressRange> {
        match self {
            Self::Map(address, request) => AddressRange::nonempty(*address, request.length).ok(),
            Self::Unmap(range) | Self::Protect(range, _) => Some(*range),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Batch {
    pub(crate) operations: Vec<Operation>,
}

impl Batch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, operation: Operation) {
        self.operations.push(operation);
    }
}
