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

/// The guest range a planned operation changes the meaning of.
pub(crate) fn planned_range(operation: &PlannedOperation) -> Option<AddressRange> {
    match operation {
        PlannedOperation::Map(address, request) => AddressRange::nonempty(*address, request.length).ok(),
        PlannedOperation::Unmap(range) | PlannedOperation::Protect(range, _) => Some(*range),
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
