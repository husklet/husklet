use hl_isa::{AddressRange, GuestAddress};

use crate::{MapRequest, Protection};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Map(MapRequest),
    Unmap(AddressRange),
    Protect(AddressRange, Protection),
    Replace(MapRequest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlannedOperation {
    Map(GuestAddress, MapRequest),
    Unmap(AddressRange),
    Protect(AddressRange, Protection),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Batch {
    pub(crate) operations: Vec<Operation>,
}

impl Batch {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, operation: Operation) {
        self.operations.push(operation);
    }
}
