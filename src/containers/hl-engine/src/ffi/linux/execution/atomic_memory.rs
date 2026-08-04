use super::operand::{ArenaMemory, ImageMemory, SliceMemory};
use hl_execution::{
    AtomicOperation, AtomicValue, ExclusiveLoad, ExclusiveMemory, ExclusiveReservation, MappingGeneration, MemoryOrder,
};
use hl_isa::GuestAddress;
use hl_memory::{
    AtomicOperation as MemoryOperation, AtomicOrder, AtomicValue as MemoryValue,
    ExclusiveReservation as MemoryReservation,
};

macro_rules! impl_exclusive_memory {
    ($memory:ty) => {
        impl ExclusiveMemory for $memory {
            fn load_ordered(&mut self, address: u64, bytes: u8, order: MemoryOrder) -> Result<u64, ()> {
                self.with_mappings(|mappings| {
                    mappings
                        .load_ordered(GuestAddress::new(address), bytes, ArenaMemory::order(order))
                        .map_err(|_| ())
                })
            }

            fn store_ordered(&mut self, address: u64, bytes: u8, value: u64, order: MemoryOrder) -> Result<(), ()> {
                self.with_mappings(|mappings| {
                    mappings
                        .store_ordered(
                            GuestAddress::new(address),
                            bytes,
                            value,
                            ArenaMemory::order(order),
                        )
                        .map_err(|_| ())
                })
            }

            fn load_exclusive(
                &mut self,
                address: u64,
                bytes: u8,
                pair: bool,
                order: MemoryOrder,
            ) -> Result<ExclusiveLoad, ()> {
                let lease = self.selected_lease();
                let (value, reservation) = lease
                    .mappings_ref()
                    .load_exclusive(GuestAddress::new(address), bytes, pair, ArenaMemory::order(order))
                    .map_err(|_| ())?;
                Ok(ExclusiveLoad {
                    value: ArenaMemory::guest_value(value),
                    reservation: ExclusiveReservation::versioned(
                        address,
                        bytes,
                        pair,
                        MappingGeneration::new(lease.generation()),
                        MappingGeneration::new(reservation.write_epoch()),
                    ),
                })
            }

            fn store_exclusive(
                &mut self,
                reservation: ExclusiveReservation,
                value: AtomicValue,
                order: MemoryOrder,
            ) -> Result<bool, ()> {
                let lease = self.selected_lease();
                if reservation.generation().value() != lease.generation() {
                    return Ok(false);
                }
                let mappings = lease.mappings_ref();
                mappings
                    .store_exclusive(
                        MemoryReservation::new(
                            reservation.address(),
                            reservation.element_bytes(),
                            reservation.pair(),
                            mappings.ledger().generation(),
                            reservation.write_epoch().value(),
                        ),
                        ArenaMemory::memory_value(value),
                        ArenaMemory::order(order),
                    )
                    .map_err(|_| ())
            }

            fn compare_exchange(
                &mut self,
                address: u64,
                bytes: u8,
                pair: bool,
                expected: AtomicValue,
                replacement: AtomicValue,
                order: MemoryOrder,
            ) -> Result<AtomicValue, ()> {
                self.with_mappings(|mappings| {
                    mappings
                        .compare_exchange(
                            GuestAddress::new(address),
                            bytes,
                            pair,
                            ArenaMemory::memory_value(expected),
                            ArenaMemory::memory_value(replacement),
                            ArenaMemory::order(order),
                        )
                        .map(ArenaMemory::guest_value)
                        .map_err(|_| ())
                })
            }

            fn fetch_update(
                &mut self,
                address: u64,
                bytes: u8,
                operation: AtomicOperation,
                operand: u64,
                order: MemoryOrder,
            ) -> Result<u64, ()> {
                self.with_mappings(|mappings| {
                    mappings
                        .fetch_update(
                            GuestAddress::new(address),
                            bytes,
                            ArenaMemory::operation(operation),
                            operand,
                            ArenaMemory::order(order),
                        )
                        .map_err(|_| ())
                })
            }
        }
    };
}

impl_exclusive_memory!(ArenaMemory);
impl_exclusive_memory!(SliceMemory<'_>);

impl ArenaMemory {
    fn order(order: MemoryOrder) -> AtomicOrder {
        match order {
            MemoryOrder::Relaxed => AtomicOrder::Relaxed,
            MemoryOrder::Acquire => AtomicOrder::Acquire,
            MemoryOrder::Release => AtomicOrder::Release,
            MemoryOrder::AcquireRelease | MemoryOrder::SequentiallyConsistent => AtomicOrder::AcquireRelease,
        }
    }

    fn operation(operation: AtomicOperation) -> MemoryOperation {
        match operation {
            AtomicOperation::Swap => MemoryOperation::Swap,
            AtomicOperation::Add => MemoryOperation::Add,
            AtomicOperation::Clear => MemoryOperation::Clear,
            AtomicOperation::ExclusiveOr => MemoryOperation::ExclusiveOr,
            AtomicOperation::Set => MemoryOperation::Set,
            AtomicOperation::SignedMaximum => MemoryOperation::SignedMaximum,
            AtomicOperation::SignedMinimum => MemoryOperation::SignedMinimum,
            AtomicOperation::UnsignedMaximum => MemoryOperation::UnsignedMaximum,
            AtomicOperation::UnsignedMinimum => MemoryOperation::UnsignedMinimum,
        }
    }

    fn memory_value(value: AtomicValue) -> MemoryValue {
        MemoryValue {
            low: value.low,
            high: value.high,
        }
    }

    fn guest_value(value: MemoryValue) -> AtomicValue {
        AtomicValue {
            low: value.low,
            high: value.high,
        }
    }
}
