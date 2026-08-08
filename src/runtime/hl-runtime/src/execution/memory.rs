use std::collections::BTreeMap;
use std::sync::Arc;

use hl_execution::{
    AtomicOperation as ExecutionAtomicOperation, AtomicValue as ExecutionAtomicValue, ExclusiveLoad, ExclusiveMemory,
    ExclusiveReservation as ExecutionReservation, FetchError, GuestOperandMemory, InstructionFetch, MappingGeneration,
    MemoryOrder,
};
use hl_isa::GuestAddress;
use hl_memory::{
    AtomicOperation, AtomicOrder, AtomicValue, ExclusiveReservation, MappingCoordinator, MemoryAccessHost, Protection,
    WriteTransaction,
};

pub struct RuntimeExecutionMemory<H: MemoryAccessHost> {
    memory: Arc<MappingCoordinator<H>>,
    next_exclusive: u64,
    exclusives: BTreeMap<u64, ExclusiveReservation>,
}

pub struct RuntimeWrite<H: MemoryAccessHost> {
    transaction: WriteTransaction<H>,
    widths: Vec<u8>,
}

impl<H: MemoryAccessHost> RuntimeExecutionMemory<H> {
    #[must_use]
    pub fn new(memory: Arc<MappingCoordinator<H>>) -> Self {
        Self {
            memory,
            next_exclusive: 1,
            exclusives: BTreeMap::new(),
        }
    }
}

impl<H: MemoryAccessHost> InstructionFetch for RuntimeExecutionMemory<H> {
    fn fetch(&self, address: u64, destination: &mut [u8]) -> Result<(), FetchError> {
        self.memory
            .read(GuestAddress::new(address), destination, Protection::EXECUTE)
            .map_err(|_| FetchError)
    }
}

impl<H: MemoryAccessHost> GuestOperandMemory for RuntimeExecutionMemory<H> {
    type Reservation = RuntimeWrite<H>;
    type BatchReservation = RuntimeWrite<H>;

    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        let mut value = [0_u8; 8];
        self.memory
            .read(
                GuestAddress::new(address),
                &mut value[..usize::from(bytes)],
                Protection::READ,
            )
            .map_err(|_| ())?;
        Ok(u64::from_le_bytes(value))
    }

    fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()> {
        let transaction = self
            .memory
            .prepare_write(GuestAddress::new(address), u64::from(bytes))
            .map_err(|_| ())?;
        Ok(RuntimeWrite {
            transaction,
            widths: vec![bytes],
        })
    }

    fn commit_write(&mut self, reservation: Self::Reservation, value: u64) -> Result<(), ()> {
        self.commit_write_batch(reservation, &[value])
    }

    fn reserve_write_batch(&self, writes: &[(u64, u8)]) -> Result<Self::BatchReservation, u64> {
        let Some((start, _)) = writes.first().copied() else {
            return Err(0);
        };
        let mut next = start;
        let mut length = 0_u64;
        let mut widths = Vec::with_capacity(writes.len());
        for (address, bytes) in writes {
            if *address != next {
                return Err(*address);
            }
            next = next.checked_add(u64::from(*bytes)).ok_or(*address)?;
            length = length.checked_add(u64::from(*bytes)).ok_or(*address)?;
            widths.push(*bytes);
        }
        let transaction = self
            .memory
            .prepare_write(GuestAddress::new(start), length)
            .map_err(|_| start)?;
        Ok(RuntimeWrite { transaction, widths })
    }

    fn commit_write_batch(&mut self, reservation: Self::BatchReservation, values: &[u64]) -> Result<(), ()> {
        if reservation.widths.len() != values.len() {
            return Err(());
        }
        let mut bytes = Vec::new();
        for (width, value) in reservation.widths.iter().zip(values) {
            bytes.extend_from_slice(&value.to_le_bytes()[..usize::from(*width)]);
        }
        self.memory
            .commit_write(reservation.transaction, &bytes)
            .map(|_| ())
            .map_err(|_| ())
    }
}

impl<H: MemoryAccessHost> ExclusiveMemory for RuntimeExecutionMemory<H> {
    fn load_ordered(&mut self, address: u64, bytes: u8, order: MemoryOrder) -> Result<u64, ()> {
        self.memory
            .load_ordered(GuestAddress::new(address), bytes, Self::order(order))
            .map_err(|_| ())
    }

    fn store_ordered(&mut self, address: u64, bytes: u8, value: u64, order: MemoryOrder) -> Result<(), ()> {
        self.memory
            .store_ordered(GuestAddress::new(address), bytes, value, Self::order(order))
            .map_err(|_| ())
    }

    fn load_exclusive(
        &mut self,
        address: u64,
        element_bytes: u8,
        pair: bool,
        order: MemoryOrder,
    ) -> Result<ExclusiveLoad, ()> {
        let (value, reservation) = self
            .memory
            .load_exclusive(GuestAddress::new(address), element_bytes, pair, Self::order(order))
            .map_err(|_| ())?;
        let token = self.next_exclusive;
        self.next_exclusive = self.next_exclusive.wrapping_add(1).max(1);
        self.exclusives.insert(token, reservation);
        Ok(ExclusiveLoad {
            value: Self::guest_value(value),
            reservation: ExecutionReservation::new(address, element_bytes, pair, MappingGeneration::new(token)),
        })
    }

    fn store_exclusive(
        &mut self,
        reservation: ExecutionReservation,
        replacement: ExecutionAtomicValue,
        order: MemoryOrder,
    ) -> Result<bool, ()> {
        let token = reservation.generation().value();
        let Some(memory_reservation) = self.exclusives.remove(&token) else {
            return Ok(false);
        };
        if memory_reservation.address() != reservation.address()
            || memory_reservation.element_bytes() != reservation.element_bytes()
            || memory_reservation.pair() != reservation.pair()
        {
            return Ok(false);
        }
        self.memory
            .store_exclusive(memory_reservation, Self::memory_value(replacement), Self::order(order))
            .map_err(|_| ())
    }

    fn discard_exclusive(&mut self, reservation: ExecutionReservation) {
        self.exclusives.remove(&reservation.generation().value());
    }

    fn compare_exchange(
        &mut self,
        address: u64,
        element_bytes: u8,
        pair: bool,
        expected: ExecutionAtomicValue,
        replacement: ExecutionAtomicValue,
        order: MemoryOrder,
    ) -> Result<ExecutionAtomicValue, ()> {
        self.memory
            .compare_exchange(
                GuestAddress::new(address),
                element_bytes,
                pair,
                Self::memory_value(expected),
                Self::memory_value(replacement),
                Self::order(order),
            )
            .map(Self::guest_value)
            .map_err(|_| ())
    }

    fn fetch_update(
        &mut self,
        address: u64,
        bytes: u8,
        operation: ExecutionAtomicOperation,
        operand: u64,
        order: MemoryOrder,
    ) -> Result<u64, ()> {
        self.memory
            .fetch_update(
                GuestAddress::new(address),
                bytes,
                Self::operation(operation),
                operand,
                Self::order(order),
            )
            .map_err(|_| ())
    }
}

impl<H: MemoryAccessHost> RuntimeExecutionMemory<H> {
    fn order(order: MemoryOrder) -> AtomicOrder {
        match order {
            MemoryOrder::Relaxed => AtomicOrder::Relaxed,
            MemoryOrder::Acquire => AtomicOrder::Acquire,
            MemoryOrder::Release => AtomicOrder::Release,
            MemoryOrder::AcquireRelease => AtomicOrder::AcquireRelease,
            // The memory coordinator serializes every atomic transaction, so
            // its strongest public ordering is already sequentially consistent.
            MemoryOrder::SequentiallyConsistent => AtomicOrder::AcquireRelease,
        }
    }

    fn operation(operation: ExecutionAtomicOperation) -> AtomicOperation {
        match operation {
            ExecutionAtomicOperation::Swap => AtomicOperation::Swap,
            ExecutionAtomicOperation::Add => AtomicOperation::Add,
            ExecutionAtomicOperation::Clear => AtomicOperation::Clear,
            ExecutionAtomicOperation::ExclusiveOr => AtomicOperation::ExclusiveOr,
            ExecutionAtomicOperation::Set => AtomicOperation::Set,
            ExecutionAtomicOperation::SignedMaximum => AtomicOperation::SignedMaximum,
            ExecutionAtomicOperation::SignedMinimum => AtomicOperation::SignedMinimum,
            ExecutionAtomicOperation::UnsignedMaximum => AtomicOperation::UnsignedMaximum,
            ExecutionAtomicOperation::UnsignedMinimum => AtomicOperation::UnsignedMinimum,
        }
    }

    fn memory_value(value: ExecutionAtomicValue) -> AtomicValue {
        AtomicValue {
            low: value.low,
            high: value.high,
        }
    }

    fn guest_value(value: AtomicValue) -> ExecutionAtomicValue {
        ExecutionAtomicValue {
            low: value.low,
            high: value.high,
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use hl_execution::{
        Aarch64CpuState, Aarch64ExecutionExit, Aarch64Interpreter, AtomicValue as GuestValue, BarrierKind,
        ExclusiveMemory, ExclusiveReservation as GuestReservation, GuestSystemPort,
    };
    use hl_isa::{AddressRange, GuestAddress};
    use hl_memory::{Backing, MapRequest, MappingHost, MemoryError, Placement, WriteReservation};

    use super::*;

    #[derive(Debug, Default)]
    struct Host {
        bytes: Mutex<BTreeMap<u64, u8>>,
        next: AtomicU64,
        writes: Mutex<BTreeMap<u64, AddressRange>>,
    }

    struct System;

    impl GuestSystemPort for System {
        fn barrier(&mut self, _: BarrierKind, _: u8) {}

        fn counter_frequency(&self) -> u64 {
            0
        }

        fn counter_value(&self) -> u64 {
            0
        }
    }

    impl MappingHost for Host {
        fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
            Ok(0)
        }

        fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
            Ok(0)
        }

        fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
            Ok(0)
        }

        fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
            Ok(())
        }

        fn rollback(&self, _: u64) {}
    }

    impl MemoryAccessHost for Host {
        type Projection = u64;

        fn read(&self, range: AddressRange, output: &mut [u8], _: Protection) -> Result<(), MemoryError> {
            let bytes = self.bytes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            for (offset, byte) in output.iter_mut().enumerate() {
                *byte = bytes
                    .get(&range.start().get().wrapping_add(offset as u64))
                    .copied()
                    .unwrap_or(0);
            }
            Ok(())
        }

        fn prepare_write(&self, range: AddressRange) -> Result<WriteReservation, MemoryError> {
            let token = self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            self.writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(token, range);
            Ok(WriteReservation::new(token, range))
        }

        fn commit_write(&self, reservation: WriteReservation, input: &[u8]) -> Result<(), MemoryError> {
            let reservation = reservation.token;
            let range = self
                .writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&reservation)
                .ok_or(MemoryError::InvariantViolation)?;
            if range.length() != input.len() as u64 {
                return Err(MemoryError::InvariantViolation);
            }
            let mut bytes = self.bytes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            for (offset, byte) in input.iter().enumerate() {
                bytes.insert(range.start().get().wrapping_add(offset as u64), *byte);
            }
            Ok(())
        }

        fn rollback_write(&self, reservation: WriteReservation) {
            let reservation = reservation.token;
            self.writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&reservation);
        }
    }

    fn memory() -> (Arc<MappingCoordinator<Host>>, RuntimeExecutionMemory<Host>) {
        let coordinator = Arc::new(MappingCoordinator::new(Host::default()));
        coordinator
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0x1000)),
                length: 4096,
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        (Arc::clone(&coordinator), RuntimeExecutionMemory::new(coordinator))
    }

    #[test]
    fn discard_consumes_token() {
        let (_, mut memory) = memory();
        let loaded = memory.load_exclusive(0x1000, 4, false, MemoryOrder::Relaxed).unwrap();
        assert_eq!(memory.exclusives.len(), 1);

        memory.discard_exclusive(loaded.reservation);
        assert!(memory.exclusives.is_empty());
        assert!(
            !memory
                .store_exclusive(loaded.reservation, GuestValue::default(), MemoryOrder::Relaxed)
                .unwrap()
        );
    }

    #[test]
    fn clrex_consumes_token() {
        let (_, mut memory) = memory();
        let loaded = memory.load_exclusive(0x1000, 4, false, MemoryOrder::Relaxed).unwrap();
        let mut cpu = Aarch64CpuState {
            pc: 0x2000,
            exclusive: Some(loaded.reservation),
            ..Default::default()
        };

        assert_eq!(
            Aarch64Interpreter::execute_concurrent(&mut cpu, &mut memory, &mut System, 0xd503_3f5f),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.exclusive, None);
        assert!(memory.exclusives.is_empty());
    }

    #[test]
    fn adapter_rejects_shape() {
        let (_, mut memory) = memory();
        for (address, bytes) in [(0x1004, 4), (0x1000, 8)] {
            let loaded = memory.load_exclusive(0x1000, 4, false, MemoryOrder::Relaxed).unwrap();
            let mismatched = GuestReservation::new(address, bytes, false, loaded.reservation.generation());
            assert!(
                !memory
                    .store_exclusive(mismatched, GuestValue { low: 9, high: 0 }, MemoryOrder::Relaxed)
                    .unwrap()
            );
            assert!(memory.exclusives.is_empty());
        }
    }

    #[test]
    fn matching_and_retry() {
        let (coordinator, mut memory) = memory();
        let loaded = memory.load_exclusive(0x1000, 4, false, MemoryOrder::Relaxed).unwrap();
        assert!(
            memory
                .store_exclusive(loaded.reservation, GuestValue { low: 7, high: 0 }, MemoryOrder::Relaxed)
                .unwrap()
        );
        assert!(memory.exclusives.is_empty());

        let loaded = memory.load_exclusive(0x1000, 4, false, MemoryOrder::Relaxed).unwrap();
        let write = coordinator.prepare_write(GuestAddress::new(0x1004), 4).unwrap();
        coordinator.commit_write(write, &9_u32.to_le_bytes()).unwrap();
        assert!(
            !memory
                .store_exclusive(loaded.reservation, GuestValue { low: 8, high: 0 }, MemoryOrder::Relaxed)
                .unwrap()
        );
        assert!(memory.exclusives.is_empty());
    }
}
