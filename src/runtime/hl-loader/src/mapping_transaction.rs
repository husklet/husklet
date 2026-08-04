use crate::{
    AddressSpaceError, ImageProtectionRegistry, MappingKind, MappingPlacement, Protection, ReservedMapping,
    TransactionalAddressSpace,
};

pub(crate) struct MappingTransaction<'a, A>
where
    A: TransactionalAddressSpace + ImageProtectionRegistry<A::Reservation>,
{
    address_space: &'a mut A,
    reservations: Vec<A::Reservation>,
    committed: bool,
}

impl<'a, A> MappingTransaction<'a, A>
where
    A: TransactionalAddressSpace + ImageProtectionRegistry<A::Reservation>,
{
    pub(crate) fn new(address_space: &'a mut A) -> Self {
        Self {
            address_space,
            reservations: Vec::new(),
            committed: false,
        }
    }

    pub(crate) fn reserve(
        &mut self,
        kind: MappingKind,
        size: u64,
        placement: MappingPlacement,
    ) -> Result<ReservedMapping<A::Reservation>, AddressSpaceError> {
        let mapping = self.address_space.reserve(kind, size, placement)?;
        self.reservations.push(mapping.token().clone());
        Ok(mapping)
    }

    pub(crate) fn stage_write(
        &mut self,
        mapping: &ReservedMapping<A::Reservation>,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), AddressSpaceError> {
        self.address_space.stage_write(mapping.token(), offset, bytes)
    }

    pub(crate) fn stage_zero(
        &mut self,
        mapping: &ReservedMapping<A::Reservation>,
        offset: u64,
        size: u64,
    ) -> Result<(), AddressSpaceError> {
        self.address_space.stage_zero(mapping.token(), offset, size)
    }

    pub(crate) fn stage_protection(
        &mut self,
        mapping: &ReservedMapping<A::Reservation>,
        offset: u64,
        size: u64,
        protection: Protection,
    ) -> Result<(), AddressSpaceError> {
        self.address_space
            .stage_protection(mapping.token(), offset, size, protection)
    }

    pub(crate) fn stage_executable(
        &mut self,
        mapping: &ReservedMapping<A::Reservation>,
    ) -> Result<(), AddressSpaceError> {
        self.address_space.stage_executable(mapping.token(), 0, mapping.size())
    }

    pub(crate) fn stage_guest_access(
        &mut self,
        mapping: &ReservedMapping<A::Reservation>,
        guest_address: u64,
        size: u64,
        read_only: bool,
    ) -> Result<(), AddressSpaceError> {
        self.address_space
            .stage_guest_access(mapping.token(), guest_address, size, read_only)
    }

    pub(crate) fn commit(&mut self) -> Result<(), AddressSpaceError> {
        self.address_space.commit(&self.reservations)?;
        self.committed = true;
        Ok(())
    }
}

impl<A> Drop for MappingTransaction<'_, A>
where
    A: TransactionalAddressSpace + ImageProtectionRegistry<A::Reservation>,
{
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for reservation in self.reservations.iter().rev() {
            self.address_space.rollback(reservation);
        }
    }
}
