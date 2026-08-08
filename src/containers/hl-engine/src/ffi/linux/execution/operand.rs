use std::sync::Arc;

use hl_execution::{ExecutionInstructionMemory, GuestOperandMemory, InstructionEpoch};
use hl_isa::GuestAddress;
use hl_memory::Protection;

use super::{MappingHostAdapter, space};

pub(super) struct ArenaMemory {
    pub(super) space: Arc<space::AddressSpace>,
}

pub(super) struct SliceMemory<'a> {
    pub(super) space: &'a Arc<space::AddressSpace>,
    pub(super) lease: &'a space::SpaceLease,
}

pub(super) trait ImageMemory {
    fn lease(&self) -> space::SpaceLease;
    fn with_mappings<R>(&self, callback: impl FnOnce(&hl_memory::MappingCoordinator<MappingHostAdapter>) -> R) -> R;
    fn epoch(&self) -> InstructionEpoch;
    fn selected_lease(&self) -> space::SpaceLease;
    fn address_space(&self) -> &space::AddressSpace;
}

/// Element widths of one staged guest write, held inline for the scalar, pair,
/// and quad forms that dominate execution so no store allocates.
enum Widths {
    Inline { data: [u8; 4], count: u8 },
    Spilled(Vec<u8>),
}

impl Widths {
    fn new(widths: &[(u64, u8)]) -> Self {
        let Ok(count) = u8::try_from(widths.len()) else {
            return Self::Spilled(widths.iter().map(|(_, bytes)| *bytes).collect());
        };
        if widths.len() > 4 {
            return Self::Spilled(widths.iter().map(|(_, bytes)| *bytes).collect());
        }
        let mut data = [0_u8; 4];
        for (slot, (_, bytes)) in data.iter_mut().zip(widths) {
            *slot = *bytes;
        }
        Self::Inline { data, count }
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Inline { data, count } => &data[..usize::from(*count)],
            Self::Spilled(widths) => widths,
        }
    }
}

pub(super) struct ArenaWrite {
    lease: space::SpaceLease,
    transaction: hl_memory::WriteSpanTransaction<MappingHostAdapter>,
    widths: Widths,
}

impl ArenaWrite {
    fn prepare(memory: &impl ImageMemory, address: u64, length: u64, widths: Widths) -> Result<Self, ()> {
        let lease = ImageMemory::lease(memory);
        let transaction = lease
            .mappings()
            .prepare_write_spans(GuestAddress::new(address), length)
            .map_err(|_| ())?;
        Ok(Self {
            lease,
            transaction,
            widths,
        })
    }

    fn commit(self, input: &[u8]) -> Result<(), ()> {
        self.lease
            .mappings()
            .commit_write_spans(self.transaction, input)
            .map(drop)
            .map_err(|_| ())
    }
}

macro_rules! impl_operand_memory {
    ($memory:ty) => {
        impl GuestOperandMemory for $memory {
            type Reservation = ArenaWrite;
            type BatchReservation = ArenaWrite;

            fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
                let mut value = [0_u8; 8];
                self.with_mappings(|mappings| {
                    mappings.read_spans(
                        GuestAddress::new(address),
                        &mut value[..usize::from(bytes)],
                        Protection::READ,
                    )
                })
                .map_err(|_| ())?;
                Ok(u64::from_le_bytes(value))
            }

            fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()> {
                ArenaWrite::prepare(self, address, u64::from(bytes), Widths::new(&[(address, bytes)]))
            }

            fn commit_write(&mut self, reservation: Self::Reservation, value: u64) -> Result<(), ()> {
                let bytes = reservation.widths.as_slice()[0];
                reservation.commit(&value.to_le_bytes()[..usize::from(bytes)])
            }

            fn reserve_write_batch(&self, writes: &[(u64, u8)]) -> Result<Self::BatchReservation, u64> {
                let Some((mut next, _)) = writes.first().copied() else {
                    return Err(0);
                };
                for (address, bytes) in writes {
                    if *address != next {
                        return Err(*address);
                    }
                    next = next.checked_add(u64::from(*bytes)).ok_or(*address)?;
                }
                let length = next.checked_sub(writes[0].0).ok_or(writes[0].0)?;
                ArenaWrite::prepare(self, writes[0].0, length, Widths::new(writes)).map_err(|_| writes[0].0)
            }

            fn commit_write_batch(&mut self, reservation: Self::BatchReservation, values: &[u64]) -> Result<(), ()> {
                let widths = reservation.widths.as_slice();
                if widths.len() != values.len() {
                    return Err(());
                }
                // Guest stores narrower than the staging buffer never allocate.
                let mut staged = [0_u8; 64];
                let mut copied = 0_usize;
                let mut spilled = Vec::new();
                let mut overflowed = false;
                for (bytes, value) in widths.iter().zip(values) {
                    let bytes = usize::from(*bytes);
                    let encoded = value.to_le_bytes();
                    if !overflowed && copied + bytes <= staged.len() {
                        staged[copied..copied + bytes].copy_from_slice(&encoded[..bytes]);
                        copied += bytes;
                        continue;
                    }
                    if !overflowed {
                        overflowed = true;
                        spilled.extend_from_slice(&staged[..copied]);
                    }
                    spilled.extend_from_slice(&encoded[..bytes]);
                }
                if overflowed {
                    reservation.commit(&spilled)
                } else {
                    reservation.commit(&staged[..copied])
                }
            }
        }

        impl ExecutionInstructionMemory for $memory {
            fn fetch(&self, address: u64, bytes: &mut [u8]) -> Result<usize, ()> {
                self.with_mappings(|mappings| {
                    mappings.read_spans(GuestAddress::new(address), bytes, Protection::EXECUTE)
                })
                .map_err(|_| ())?;
                Ok(bytes.len())
            }

            fn instruction_epoch(&self) -> Option<InstructionEpoch> {
                Some(self.epoch())
            }

            fn invalidate_instruction(&mut self, address: u64) {
                self.with_mappings(|mappings| mappings.publish_instruction_at(address));
            }
        }
    };
}

impl_operand_memory!(ArenaMemory);
impl_operand_memory!(SliceMemory<'_>);

impl ImageMemory for ArenaMemory {
    fn lease(&self) -> space::SpaceLease {
        self.space.lease()
    }
    fn with_mappings<R>(&self, callback: impl FnOnce(&hl_memory::MappingCoordinator<MappingHostAdapter>) -> R) -> R {
        let lease = self.space.lease();
        callback(lease.mappings_ref())
    }
    fn epoch(&self) -> InstructionEpoch {
        let lease = self.space.lease();
        let mappings = lease.mappings_ref();
        InstructionEpoch {
            incarnation: lease.generation(),
            mappings: mappings.ledger().generation(),
            writes: mappings.instruction_epoch(),
        }
    }
    fn selected_lease(&self) -> space::SpaceLease {
        self.space.lease()
    }
    fn address_space(&self) -> &space::AddressSpace {
        &self.space
    }
}

impl ImageMemory for SliceMemory<'_> {
    fn lease(&self) -> space::SpaceLease {
        self.lease.clone()
    }
    fn with_mappings<R>(&self, callback: impl FnOnce(&hl_memory::MappingCoordinator<MappingHostAdapter>) -> R) -> R {
        callback(self.lease.mappings_ref())
    }
    fn epoch(&self) -> InstructionEpoch {
        let mappings = self.lease.mappings_ref();
        InstructionEpoch {
            incarnation: self.lease.generation(),
            mappings: mappings.ledger().generation(),
            writes: mappings.instruction_epoch(),
        }
    }
    fn selected_lease(&self) -> space::SpaceLease {
        self.lease.clone()
    }
    fn address_space(&self) -> &space::AddressSpace {
        self.space
    }
}
