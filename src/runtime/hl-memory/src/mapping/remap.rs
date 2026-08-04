use hl_isa::{AddressRange, GuestAddress};

use super::host::Coordinator;
use super::plan::{Operation, PlannedOperation};
use super::port::Host;
use crate::{MapRequest, MemoryError};

impl<H: Host> Coordinator<H> {
    pub fn remap(
        &self,
        source: AddressRange,
        request: MapRequest,
        keep_source: bool,
    ) -> Result<GuestAddress, MemoryError> {
        self.remap_with_charge(source, request, keep_source, 0, false)
    }

    pub fn remap_charged(
        &self,
        source: AddressRange,
        request: MapRequest,
        keep_source: bool,
        charge: u64,
    ) -> Result<GuestAddress, MemoryError> {
        self.remap_with_charge(source, request, keep_source, charge, true)
    }

    fn remap_with_charge(
        &self,
        source: AddressRange,
        request: MapRequest,
        keep_source: bool,
        charge: u64,
        reserved: bool,
    ) -> Result<GuestAddress, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self.transaction.lock().unwrap_or_else(|error| error.into_inner());
        let mut transition = self.transition();
        let mut operations = vec![if reserved {
            Operation::ReplaceCharged(request, charge)
        } else {
            Operation::Replace(request)
        }];
        if !keep_source {
            operations.push(Operation::Unmap(source));
        }
        let addresses = self.ledger.batch_transaction(&operations, |plan, regions| {
            let pins = self.prepare_pins(regions)?;
            let destination = Self::destination(plan)?;
            let reservation = self.host.stage_remap(source, destination, request, keep_source)?;
            if let Err(error) = self.host.commit(&[reservation]) {
                self.host.rollback(reservation);
                return Err(error);
            }
            self.publish_pins(regions, pins);
            Ok(())
        })?;
        let address = addresses.first().copied().ok_or(MemoryError::InvariantViolation)?;
        transition.published(self.ledger.generation());
        Ok(address)
    }

    fn destination(plan: &[PlannedOperation]) -> Result<GuestAddress, MemoryError> {
        plan.iter()
            .find_map(|operation| match operation {
                PlannedOperation::Map(address, _) => Some(*address),
                _ => None,
            })
            .ok_or(MemoryError::InvariantViolation)
    }
}
