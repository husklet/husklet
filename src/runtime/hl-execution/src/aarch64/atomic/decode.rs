use crate::{Aarch64DecodeError, Aarch64Instruction, AtomicOperation, MemoryOrder, MemoryWidth};

pub(crate) struct Decoder;

impl Decoder {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        if word & 0x3fff_fc00 == 0x38bf_c000 && word >> 26 & 1 == 0 {
            return Some(Ok(Aarch64Instruction::OrderedAccess {
                load: true,
                base: (word >> 5 & 31) as u8,
                transfer: (word & 31) as u8,
                width: Self::width(word >> 30 & 3),
                order: MemoryOrder::Acquire,
            }));
        }
        if word & 0x3f00_0000 == 0x0800_0000 {
            return Some(Self::exclusive(word));
        }
        if word & 0x3b20_0c00 == 0x3820_0000 {
            return Some(Self::update(word));
        }
        None
    }

    fn exclusive(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 26 & 1 != 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let size = word >> 30 & 3;
        let width = Self::width(size);
        let o2 = word >> 23 & 1 != 0;
        let load = word >> 22 & 1 != 0;
        let pair = word >> 21 & 1 != 0;
        let release = word >> 15 & 1 != 0;
        let status = (word >> 16 & 31) as u8;
        let second = (word >> 10 & 31) as u8;
        let transfer = (word & 31) as u8;

        if o2 && pair {
            if second != 31 {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Self::compare_exchange(word, width, false);
        }
        if o2 {
            if status != 31 || second != 31 {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::OrderedAccess {
                load,
                base: (word >> 5 & 31) as u8,
                transfer,
                width,
                order: if load {
                    MemoryOrder::Acquire
                } else {
                    MemoryOrder::Release
                },
            });
        }
        if pair && word >> 31 == 0 {
            if second != 31 || status & 1 != 0 || transfer & 1 != 0 {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Self::compare_exchange(
                word,
                if word >> 30 & 1 != 0 {
                    MemoryWidth::Double
                } else {
                    MemoryWidth::Word
                },
                true,
            );
        }
        if pair && size < 2 {
            return Err(Aarch64DecodeError::Reserved);
        }
        if load {
            if status != 31 || (!pair && second != 31) {
                return Err(Aarch64DecodeError::Reserved);
            }
            Ok(Aarch64Instruction::ExclusiveLoad {
                base: (word >> 5 & 31) as u8,
                first: transfer,
                second: pair.then_some(second),
                width,
                order: MemoryOrder::from_bits(release, false),
            })
        } else {
            if !pair && second != 31 {
                return Err(Aarch64DecodeError::Reserved);
            }
            Ok(Aarch64Instruction::ExclusiveStore {
                base: (word >> 5 & 31) as u8,
                status,
                first: transfer,
                second: pair.then_some(second),
                width,
                order: MemoryOrder::from_bits(false, release),
            })
        }
    }

    fn compare_exchange(word: u32, width: MemoryWidth, pair: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        Ok(Aarch64Instruction::AtomicCompareExchange {
            base: (word >> 5 & 31) as u8,
            expected: (word >> 16 & 31) as u8,
            replacement: (word & 31) as u8,
            width,
            pair,
            order: MemoryOrder::from_bits(word >> 22 & 1 != 0, word >> 15 & 1 != 0),
        })
    }

    fn update(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 26 & 1 != 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let operation = if word >> 15 & 1 != 0 {
            if word >> 12 & 7 != 0 {
                return Err(Aarch64DecodeError::Reserved);
            }
            AtomicOperation::Swap
        } else {
            match word >> 12 & 7 {
                0 => AtomicOperation::Add,
                1 => AtomicOperation::Clear,
                2 => AtomicOperation::ExclusiveOr,
                3 => AtomicOperation::Set,
                4 => AtomicOperation::SignedMaximum,
                5 => AtomicOperation::SignedMinimum,
                6 => AtomicOperation::UnsignedMaximum,
                _ => AtomicOperation::UnsignedMinimum,
            }
        };
        Ok(Aarch64Instruction::AtomicUpdate {
            base: (word >> 5 & 31) as u8,
            source: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
            width: Self::width(word >> 30 & 3),
            operation,
            order: MemoryOrder::from_bits(word >> 23 & 1 != 0, word >> 22 & 1 != 0),
        })
    }

    fn width(size: u32) -> MemoryWidth {
        match size {
            0 => MemoryWidth::Byte,
            1 => MemoryWidth::Half,
            2 => MemoryWidth::Word,
            _ => MemoryWidth::Double,
        }
    }
}
