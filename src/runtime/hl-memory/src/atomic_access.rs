use std::sync::atomic::Ordering;

use hl_isa::{AddressRange, GuestAddress};

use crate::{
    AtomicOperation, AtomicOrder, AtomicValue, Backing, ExclusiveReservation, MappingCoordinator, MemoryAccessHost,
    MemoryError, Protection, ReservationCoordinate, ReservationEpochs,
};

const RESERVATION_GRANULE: u64 = 64;

impl<H: MemoryAccessHost> MappingCoordinator<H> {
    pub fn load_ordered(&self, address: GuestAddress, bytes: u8, _order: AtomicOrder) -> Result<u64, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(self.read_atomic(address, bytes, false)?.low)
    }

    pub fn store_ordered(
        &self,
        address: GuestAddress,
        bytes: u8,
        value: u64,
        _order: AtomicOrder,
    ) -> Result<(), MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.write_atomic(address, bytes, false, AtomicValue { low: value, high: 0 })
    }

    pub fn compare_apply_many<E>(
        &self,
        mapping_generation: u64,
        comparisons: &[(GuestAddress, u32)],
        apply: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<Result<Option<usize>, E>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.ledger.generation() != mapping_generation {
            return Err(MemoryError::NoAddressSpace);
        }
        for (index, (address, expected)) in comparisons.iter().enumerate() {
            let observed = self.read_atomic(*address, 4, false)?.low as u32;
            if observed != *expected {
                return Ok(Ok(Some(index)));
            }
        }
        Ok(apply().map(|()| None))
    }

    pub fn compare_apply_u32<E>(
        &self,
        address: GuestAddress,
        expected: u32,
        apply: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<Result<bool, E>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let resolution = self
            .ledger
            .resolve(address, Protection::READ)
            .ok_or(MemoryError::NoAddressSpace)?;
        if matches!(resolution.region.backing(), crate::Backing::Shared(_)) {
            let pin = self.retained_pin(resolution.region)?;
            let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
            return pin
                .compare_apply_u32(offset, expected, apply)
                .map_err(MemoryError::from);
        }
        let observed = self.read_atomic(address, 4, false)?.low as u32;
        if observed != expected {
            return Ok(Ok(false));
        }
        Ok(apply().map(|()| true))
    }

    pub fn load_exclusive(
        &self,
        address: GuestAddress,
        element_bytes: u8,
        pair: bool,
        _order: AtomicOrder,
    ) -> Result<(AtomicValue, ExclusiveReservation), MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let value = self.read_atomic(address, element_bytes, pair)?;
        let write_epoch = self.exclusive_epoch(address)?;
        Ok((
            value,
            ExclusiveReservation {
                address: address.get(),
                element_bytes,
                pair,
                mapping_generation: self.ledger.generation(),
                write_epoch,
            },
        ))
    }

    pub fn store_exclusive(
        &self,
        reservation: ExclusiveReservation,
        replacement: AtomicValue,
        _order: AtomicOrder,
    ) -> Result<bool, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let address = GuestAddress::new(reservation.address);
        self.validate_atomic(address, reservation.element_bytes, reservation.pair, Protection::WRITE)?;
        let current = self.exclusive_epoch(address)?;
        if reservation.mapping_generation != self.ledger.generation() || current != reservation.write_epoch {
            return Ok(false);
        }
        self.write_atomic(address, reservation.element_bytes, reservation.pair, replacement)?;
        Ok(true)
    }

    pub fn compare_exchange(
        &self,
        address: GuestAddress,
        element_bytes: u8,
        pair: bool,
        expected: AtomicValue,
        replacement: AtomicValue,
        _order: AtomicOrder,
    ) -> Result<AtomicValue, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let observed = self.read_atomic(address, element_bytes, pair)?;
        if observed == Self::masked_value(expected, element_bytes, pair) {
            self.write_atomic(address, element_bytes, pair, replacement)?;
        }
        Ok(observed)
    }

    pub fn fetch_update(
        &self,
        address: GuestAddress,
        bytes: u8,
        operation: AtomicOperation,
        operand: u64,
        _order: AtomicOrder,
    ) -> Result<u64, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let observed = self.read_atomic(address, bytes, false)?.low;
        let replacement = Self::updated(observed, operand, bytes, operation)?;
        self.write_atomic(
            address,
            bytes,
            false,
            AtomicValue {
                low: replacement,
                high: 0,
            },
        )?;
        Ok(observed)
    }

    fn read_atomic(&self, address: GuestAddress, element_bytes: u8, pair: bool) -> Result<AtomicValue, MemoryError> {
        let range = self.validate_atomic(address, element_bytes, pair, Protection::READ)?;
        let mut bytes = [0_u8; 16];
        let length = usize::try_from(range.length()).map_err(|_| MemoryError::AddressOverflow)?;
        self.read(address, &mut bytes[..length], Protection::READ)?;
        let width = usize::from(element_bytes);
        Ok(AtomicValue {
            low: Self::decode_atomic(&bytes[..width]),
            high: if pair {
                Self::decode_atomic(&bytes[width..length])
            } else {
                0
            },
        })
    }

    fn write_atomic(
        &self,
        address: GuestAddress,
        element_bytes: u8,
        pair: bool,
        value: AtomicValue,
    ) -> Result<(), MemoryError> {
        let range = self.validate_atomic(address, element_bytes, pair, Protection::WRITE)?;
        let width = usize::from(element_bytes);
        let mut bytes = [0_u8; 16];
        bytes[..width].copy_from_slice(&value.low.to_le_bytes()[..width]);
        let length = width * if pair { 2 } else { 1 };
        if pair {
            bytes[width..length].copy_from_slice(&value.high.to_le_bytes()[..width]);
        }
        let resolution = self
            .ledger
            .resolve(address, Protection::WRITE)
            .ok_or(MemoryError::NoAddressSpace)?;
        let shared = if matches!(resolution.region.backing(), Backing::Shared(_)) {
            let pin = self.retained_pin(resolution.region)?;
            let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
            let mut previous = [0_u8; 16];
            pin.read(offset, &mut previous[..length])?;
            pin.write(offset, &bytes[..length])?;
            Some((pin, offset, previous))
        } else {
            None
        };
        if let Err(error) = self.host.write_atomic(range, &bytes[..length]) {
            if let Some((pin, offset, previous)) = shared {
                pin.write(offset, &previous[..length])?;
            }
            return Err(error);
        }
        self.invalidate_exclusive(range)?;
        self.host
            .executable
            .publish(self.executable_write_ranges(range.start(), resolution, range.length()));
        self.host.epoch.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub(crate) fn invalidate_exclusive(&self, range: AddressRange) -> Result<(), MemoryError> {
        // Native exits invalidate whole projections, so resolve once per region
        // rather than once per granule.
        let mut address = range.start().get();
        while address < range.end().get() {
            let resolution = self
                .ledger
                .resolve(GuestAddress::new(address), Protection::NONE)
                .ok_or(MemoryError::NoAddressSpace)?;
            let backing = resolution.region.backing();
            let region_end = resolution.region.range().end().get().min(range.end().get());
            let base = address;
            let mut epochs = None;
            while address < region_end {
                let coordinate = ReservationCoordinate::from_mapping(
                    backing,
                    resolution.backing_offset + (address - base),
                    address,
                )?;
                let table = epochs.get_or_insert_with(|| {
                    if coordinate.shared() {
                        self.shared.as_ref().map_or_else(
                            || std::sync::Arc::clone(&self.host.reservations),
                            |store| store.reservation_epochs(),
                        )
                    } else {
                        std::sync::Arc::clone(&self.host.reservations)
                    }
                });
                table.invalidate_at(coordinate);
                let next = (address | (RESERVATION_GRANULE - 1)).saturating_add(1);
                if next <= address {
                    return Ok(());
                }
                address = next.min(region_end);
            }
        }
        Ok(())
    }

    fn exclusive_epoch(&self, address: GuestAddress) -> Result<u64, MemoryError> {
        let (epochs, coordinate) = self.exclusive_key(address)?;
        Ok(epochs.capture_at(coordinate))
    }

    fn exclusive_key(
        &self,
        address: GuestAddress,
    ) -> Result<(std::sync::Arc<ReservationEpochs>, ReservationCoordinate), MemoryError> {
        let resolution = self
            .ledger
            .resolve(address, Protection::NONE)
            .ok_or(MemoryError::NoAddressSpace)?;
        let coordinate =
            ReservationCoordinate::from_mapping(resolution.region.backing(), resolution.backing_offset, address.get())?;
        let epochs = if coordinate.shared() {
            self.shared.as_ref().map_or_else(
                || std::sync::Arc::clone(&self.host.reservations),
                |store| store.reservation_epochs(),
            )
        } else {
            std::sync::Arc::clone(&self.host.reservations)
        };
        Ok((epochs, coordinate))
    }

    fn validate_atomic(
        &self,
        address: GuestAddress,
        element_bytes: u8,
        pair: bool,
        access: Protection,
    ) -> Result<AddressRange, MemoryError> {
        if !matches!(element_bytes, 1 | 2 | 4 | 8) {
            return Err(MemoryError::InvariantViolation);
        }
        let length = u64::from(element_bytes) * if pair { 2 } else { 1 };
        let range = AddressRange::nonempty(address, length).map_err(|_| MemoryError::AddressOverflow)?;
        let resolution = self
            .ledger
            .resolve(address, access)
            .ok_or(MemoryError::NoAddressSpace)?;
        if resolution.contiguous < length {
            return Err(MemoryError::NoAddressSpace);
        }
        Ok(range)
    }

    fn decode_atomic(bytes: &[u8]) -> u64 {
        let mut value = [0_u8; 8];
        value[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(value)
    }

    fn masked_value(value: AtomicValue, bytes: u8, pair: bool) -> AtomicValue {
        let mask = if bytes == 8 {
            u64::MAX
        } else {
            (1_u64 << (bytes * 8)) - 1
        };
        AtomicValue {
            low: value.low & mask,
            high: if pair { value.high & mask } else { 0 },
        }
    }

    fn updated(old: u64, operand: u64, bytes: u8, operation: AtomicOperation) -> Result<u64, MemoryError> {
        if !matches!(bytes, 1 | 2 | 4 | 8) {
            return Err(MemoryError::InvariantViolation);
        }
        let mask = if bytes == 8 {
            u64::MAX
        } else {
            (1_u64 << (bytes * 8)) - 1
        };
        let old = old & mask;
        let operand = operand & mask;
        Ok(match operation {
            AtomicOperation::Swap => operand,
            AtomicOperation::Add => old.wrapping_add(operand) & mask,
            AtomicOperation::Clear => old & !operand & mask,
            AtomicOperation::ExclusiveOr => old ^ operand,
            AtomicOperation::Set => old | operand,
            AtomicOperation::SignedMaximum | AtomicOperation::SignedMinimum => {
                let shift = 64 - bytes * 8;
                let old_signed = ((old << shift) as i64) >> shift;
                let operand_signed = ((operand << shift) as i64) >> shift;
                let select = if operation == AtomicOperation::SignedMaximum {
                    operand_signed > old_signed
                } else {
                    operand_signed < old_signed
                };
                if select { operand } else { old }
            }
            AtomicOperation::UnsignedMaximum => old.max(operand),
            AtomicOperation::UnsignedMinimum => old.min(operand),
        })
    }
}
