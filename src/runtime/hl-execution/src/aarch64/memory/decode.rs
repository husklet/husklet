use crate::{
    Aarch64DecodeError, Aarch64Instruction, IndexExtension, LoadExtension, MemoryAddress, MemoryWidth, Writeback,
};

pub(crate) struct Decoder;
pub(crate) type Aarch64MemoryDecoder = Decoder;

impl Aarch64MemoryDecoder {
    pub(crate) fn decode(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word & 0xbf00_0000 == 0x0d00_0000 {
            return Self::structure_lane(word);
        }
        if word & 0xbf20_0000 == 0x0c00_0000 {
            return if word >> 23 & 1 != 0 {
                Self::structure_post(word)
            } else {
                Self::structure_load(word)
            };
        }
        if word & 0x3b00_0000 == 0x1800_0000 {
            return Self::literal(word);
        }
        if word & 0x3a00_0000 == 0x2800_0000 {
            return Self::pair(word);
        }
        Self::single(word)
    }

    fn structure_lane(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let opcode = word >> 13 & 7;
        let pair = word >> 21 & 1;
        let count = if opcode & 1 == 0 { pair + 1 } else { pair + 3 } as u8;
        let replicate = opcode >= 6;
        if replicate && word >> 22 & 1 == 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let size = word >> 10 & 3;
        let (lane_bits, lane) = if replicate {
            (8_u8 << size, 0)
        } else {
            match opcode & 6 {
                0 => (8, ((word >> 30 & 1) << 3 | (word >> 12 & 1) << 2 | size) as u8),
                2 if size & 1 == 0 => (16, ((word >> 30 & 1) << 2 | (word >> 12 & 1) << 1 | size >> 1) as u8),
                4 if size == 0 => (32, ((word >> 30 & 1) << 1 | (word >> 12 & 1)) as u8),
                4 if size == 1 && word >> 12 & 1 == 0 => (64, (word >> 30 & 1) as u8),
                _ => return Err(Aarch64DecodeError::Reserved),
            }
        };
        let base = (word >> 5 & 31) as u8;
        let bytes = lane_bits / 8;
        let address = if word >> 23 & 1 == 0 {
            MemoryAddress::Base {
                register: base,
                displacement: 0,
                writeback: Writeback::None,
            }
        } else {
            let index = (word >> 16 & 31) as u8;
            if index == 31 {
                MemoryAddress::Base {
                    register: base,
                    displacement: i64::from(bytes * count),
                    writeback: Writeback::PostIndex,
                }
            } else {
                MemoryAddress::PostRegister { base, index }
            }
        };
        Ok(Aarch64Instruction::VectorStructureLane {
            first: (word & 31) as u8,
            count,
            lane_bits,
            lane,
            load: word >> 22 & 1 != 0,
            replicate,
            wide: word >> 30 & 1 != 0,
            address,
        })
    }

    fn structure_post(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let first = (word & 31) as u8;
        let bytes = if word >> 30 & 1 != 0 { 16 } else { 8 };
        let opcode = word >> 12 & 15;
        let count = match opcode {
            0 => 4,
            4 => 3,
            8 => 2,
            7 => 1,
            10 => 2,
            6 => 3,
            2 => 4,
            _ => return Err(Aarch64DecodeError::Unsupported),
        };
        let base = (word >> 5 & 31) as u8;
        let index = (word >> 16 & 31) as u8;
        let address = if index == 31 {
            MemoryAddress::Base {
                register: base,
                displacement: i64::from(bytes * count),
                writeback: Writeback::PostIndex,
            }
        } else {
            MemoryAddress::PostRegister { base, index }
        };
        let load = word >> 22 & 1 != 0;
        if matches!(opcode, 0 | 4 | 8) {
            let lane_bits = 8_u8 << (word >> 10 & 3);
            if !matches!((word >> 30 & 1, lane_bits), (1, _) | (0, 8 | 16 | 32)) {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::VectorStructureGroup {
                first,
                count,
                lane_bits,
                load,
                wide: word >> 30 & 1 != 0,
                address,
            });
        }
        Ok(if count == 1 {
            if load {
                Aarch64Instruction::VectorLoad {
                    destination: first,
                    bytes,
                    address,
                }
            } else {
                Aarch64Instruction::VectorStore {
                    source: first,
                    bytes,
                    address,
                }
            }
        } else if count == 2 {
            if load {
                Aarch64Instruction::VectorLoadPair {
                    first,
                    second: first.wrapping_add(1) & 31,
                    bytes,
                    address,
                }
            } else {
                Aarch64Instruction::VectorStorePair {
                    first,
                    second: first.wrapping_add(1) & 31,
                    bytes,
                    address,
                }
            }
        } else if load {
            Aarch64Instruction::VectorLoadGroup {
                first,
                count,
                bytes,
                address,
            }
        } else {
            Aarch64Instruction::VectorStoreGroup {
                first,
                count,
                bytes,
                address,
            }
        })
    }

    fn structure_load(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 16 & 31 != 0 {
            return Err(Aarch64DecodeError::Unsupported);
        }
        let first = (word & 31) as u8;
        let bytes = if word >> 30 & 1 != 0 { 16 } else { 8 };
        let address = MemoryAddress::Base {
            register: (word >> 5 & 31) as u8,
            displacement: 0,
            writeback: Writeback::None,
        };
        let opcode = word >> 12 & 15;
        let count = match opcode {
            0 => 4,
            4 => 3,
            8 => 2,
            7 => 1,
            10 => 2,
            6 => 3,
            2 => 4,
            _ => return Err(Aarch64DecodeError::Unsupported),
        };
        let load = word >> 22 & 1 != 0;
        if matches!(opcode, 0 | 4 | 8) {
            let lane_bits = 8_u8 << (word >> 10 & 3);
            if !matches!((word >> 30 & 1, lane_bits), (1, _) | (0, 8 | 16 | 32)) {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::VectorStructureGroup {
                first,
                count,
                lane_bits,
                load,
                wide: word >> 30 & 1 != 0,
                address,
            });
        }
        Ok(if count == 1 {
            if load {
                Aarch64Instruction::VectorLoad {
                    destination: first,
                    bytes,
                    address,
                }
            } else {
                Aarch64Instruction::VectorStore {
                    source: first,
                    bytes,
                    address,
                }
            }
        } else if count == 2 {
            if load {
                Aarch64Instruction::VectorLoadPair {
                    first,
                    second: first.wrapping_add(1) & 31,
                    bytes,
                    address,
                }
            } else {
                Aarch64Instruction::VectorStorePair {
                    first,
                    second: first.wrapping_add(1) & 31,
                    bytes,
                    address,
                }
            }
        } else if load {
            Aarch64Instruction::VectorLoadGroup {
                first,
                count,
                bytes,
                address,
            }
        } else {
            Aarch64Instruction::VectorStoreGroup {
                first,
                count,
                bytes,
                address,
            }
        })
    }

    fn literal(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 26 & 1 != 0 {
            let bytes = match word >> 30 & 3 {
                0 => 4,
                1 => 8,
                2 => 16,
                3 => return Err(Aarch64DecodeError::Reserved),
                _ => unreachable!(),
            };
            return Ok(Aarch64Instruction::VectorLoad {
                destination: (word & 31) as u8,
                bytes,
                address: MemoryAddress::Literal {
                    displacement: Self::sign_extend(u64::from(word >> 5 & 0x7ffff), 19) << 2,
                },
            });
        }
        let destination = (word & 31) as u8;
        let displacement = Self::sign_extend(u64::from(word >> 5 & 0x7ffff), 19) << 2;
        let (width, extension) = match word >> 30 & 3 {
            0 => (MemoryWidth::Word, LoadExtension::Zero),
            1 => (MemoryWidth::Double, LoadExtension::Zero),
            2 => (MemoryWidth::Word, LoadExtension::SignTo64),
            3 => return Ok(Aarch64Instruction::Nop),
            _ => unreachable!(),
        };
        Ok(Aarch64Instruction::Load {
            destination,
            width,
            extension,
            address: MemoryAddress::Literal { displacement },
        })
    }

    fn pair(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 26 & 1 != 0 {
            return Self::vector_pair(word);
        }
        let load = word >> 22 & 1 != 0;
        let operation = word >> 30 & 3;
        if operation == 3 || (operation == 1 && !load) {
            return Err(Aarch64DecodeError::Reserved);
        }
        let (width, sign_extend) = match operation {
            0 => (MemoryWidth::Word, false),
            1 => (MemoryWidth::Word, true),
            2 => (MemoryWidth::Double, false),
            _ => unreachable!(),
        };
        let displacement =
            Self::sign_extend(u64::from(word >> 15 & 0x7f), 7) << if width == MemoryWidth::Double { 3 } else { 2 };
        let writeback = match word >> 23 & 3 {
            1 => Writeback::PostIndex,
            3 => Writeback::PreIndex,
            _ => Writeback::None,
        };
        let address = MemoryAddress::Base {
            register: (word >> 5 & 31) as u8,
            displacement,
            writeback,
        };
        let first = (word & 31) as u8;
        let second = (word >> 10 & 31) as u8;
        Ok(if load {
            Aarch64Instruction::LoadPair {
                first,
                second,
                width,
                sign_extend,
                address,
            }
        } else {
            Aarch64Instruction::StorePair {
                first,
                second,
                width,
                address,
            }
        })
    }

    fn vector_pair(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let operation = word >> 30 & 3;
        if operation == 3 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let bytes = 4 << operation;
        let displacement = Self::sign_extend(u64::from(word >> 15 & 0x7f), 7) * i64::from(bytes);
        let writeback = match word >> 23 & 3 {
            1 => Writeback::PostIndex,
            3 => Writeback::PreIndex,
            _ => Writeback::None,
        };
        let address = MemoryAddress::Base {
            register: (word >> 5 & 31) as u8,
            displacement,
            writeback,
        };
        let first = (word & 31) as u8;
        let second = (word >> 10 & 31) as u8;
        Ok(if word >> 22 & 1 != 0 {
            Aarch64Instruction::VectorLoadPair {
                first,
                second,
                bytes: bytes as u8,
                address,
            }
        } else {
            Aarch64Instruction::VectorStorePair {
                first,
                second,
                bytes: bytes as u8,
                address,
            }
        })
    }

    fn single(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let scaled = word & 0x3b00_0000 == 0x3900_0000;
        let register_offset = word & 0x3b20_0c00 == 0x3820_0800;
        let unscaled = word & 0x3b20_0000 == 0x3800_0000;
        if matches!(word & 0x3b20_0c00, 0x3820_0400 | 0x3820_0c00) {
            return Err(Aarch64DecodeError::Reserved);
        }
        if !scaled && !register_offset && !unscaled {
            return Err(Aarch64DecodeError::Unsupported);
        }
        let size = (word >> 30 & 3) as u8;
        if word >> 26 & 1 != 0 {
            return Self::vector_single(word, size, scaled, register_offset);
        }
        // PRFM/PRFUM are non-faulting implementation hints. Their Rt field
        // names a prefetch operation rather than a destination register, and
        // an implementation is permitted to perform no prefetch at all.
        if size == 3 && word >> 22 & 3 == 2 {
            return Ok(Aarch64Instruction::Nop);
        }
        let width = match size {
            0 => MemoryWidth::Byte,
            1 => MemoryWidth::Half,
            2 => MemoryWidth::Word,
            _ => MemoryWidth::Double,
        };
        let address = if scaled {
            MemoryAddress::Base {
                register: (word >> 5 & 31) as u8,
                displacement: i64::from(word >> 10 & 0xfff) << size,
                writeback: Writeback::None,
            }
        } else if register_offset {
            Self::register_address(word, size)?
        } else {
            Self::unscaled_address(word)
        };
        Self::single_operation(word, width, address)
    }

    fn vector_single(
        word: u32,
        size: u8,
        scaled: bool,
        register_offset: bool,
    ) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let operation = (word >> 22 & 3) as u8;
        let bytes = if operation & 2 == 0 {
            1 << size
        } else if size == 0 {
            16
        } else {
            return Err(Aarch64DecodeError::Reserved);
        };
        let scale = if bytes == 16 { 4 } else { size };
        let address = if scaled {
            MemoryAddress::Base {
                register: (word >> 5 & 31) as u8,
                displacement: i64::from(word >> 10 & 0xfff) << scale,
                writeback: Writeback::None,
            }
        } else if register_offset {
            Self::register_address(word, scale)?
        } else {
            Self::unscaled_address(word)
        };
        let register = (word & 31) as u8;
        Ok(if operation & 1 == 0 {
            Aarch64Instruction::VectorStore {
                source: register,
                bytes,
                address,
            }
        } else {
            Aarch64Instruction::VectorLoad {
                destination: register,
                bytes,
                address,
            }
        })
    }

    fn register_address(word: u32, size: u8) -> Result<MemoryAddress, Aarch64DecodeError> {
        let extension = match word >> 13 & 7 {
            2 => IndexExtension::Unsigned32,
            3 => IndexExtension::Unsigned64,
            6 => IndexExtension::Signed32,
            7 => IndexExtension::Signed64,
            _ => return Err(Aarch64DecodeError::Reserved),
        };
        Ok(MemoryAddress::Register {
            base: (word >> 5 & 31) as u8,
            index: (word >> 16 & 31) as u8,
            extension,
            shift: if word >> 12 & 1 != 0 { size } else { 0 },
        })
    }

    fn unscaled_address(word: u32) -> MemoryAddress {
        let mode = word >> 10 & 3;
        MemoryAddress::Base {
            register: (word >> 5 & 31) as u8,
            displacement: Self::sign_extend(u64::from(word >> 12 & 0x1ff), 9),
            writeback: match mode {
                1 => Writeback::PostIndex,
                3 => Writeback::PreIndex,
                _ => Writeback::None,
            },
        }
    }

    fn single_operation(
        word: u32,
        width: MemoryWidth,
        address: MemoryAddress,
    ) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let register = (word & 31) as u8;
        match word >> 22 & 3 {
            0 => Ok(Aarch64Instruction::Store {
                source: register,
                width,
                address,
            }),
            1 => Ok(Aarch64Instruction::Load {
                destination: register,
                width,
                extension: LoadExtension::Zero,
                address,
            }),
            2 if width == MemoryWidth::Double => Err(Aarch64DecodeError::Unsupported),
            2 => Ok(Aarch64Instruction::Load {
                destination: register,
                width,
                extension: LoadExtension::SignTo64,
                address,
            }),
            3 if matches!(width, MemoryWidth::Word | MemoryWidth::Double) => Err(Aarch64DecodeError::Reserved),
            3 => Ok(Aarch64Instruction::Load {
                destination: register,
                width,
                extension: LoadExtension::SignTo32,
                address,
            }),
            _ => unreachable!(),
        }
    }

    fn sign_extend(value: u64, bits: u32) -> i64 {
        ((value << (64 - bits)) as i64) >> (64 - bits)
    }
}
