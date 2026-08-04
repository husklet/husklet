use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelocationRecord {
    pub offset: u32,
    pub info: u32,
}

impl RelocationRecord {
    pub const ENCODED_SIZE: usize = 8;

    pub fn encode(self) -> [u8; Self::ENCODED_SIZE] {
        let mut bytes = [0; Self::ENCODED_SIZE];
        bytes[..4].copy_from_slice(&self.offset.to_le_bytes());
        bytes[4..].copy_from_slice(&self.info.to_le_bytes());
        bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocationTable(Vec<RelocationRecord>);

impl RelocationTable {
    pub fn new(records: Vec<RelocationRecord>, maximum: usize, code_size: usize) -> Result<Self, RelocationError> {
        if records.len() > maximum {
            return Err(RelocationError::TooMany);
        }
        for record in &records {
            let start = record.offset as usize;
            let end = start.checked_add(16).ok_or(RelocationError::OutOfBounds)?;
            if start % 4 != 0 || end > code_size {
                return Err(RelocationError::OutOfBounds);
            }
        }
        Ok(Self(records))
    }

    pub fn records(&self) -> &[RelocationRecord] {
        &self.0
    }
}

pub struct Materialization;

impl Materialization {
    pub fn slide(words: [u32; 4], slide: u64) -> Result<[u32; 4], RelocationError> {
        let register = words[0] & 0x1f;
        let expected = [0xd280_0000, 0xf2a0_0000, 0xf2c0_0000, 0xf2e0_0000];
        for (word, opcode) in words.iter().zip(expected) {
            if word & 0xffe0_001f != opcode | register {
                return Err(RelocationError::InvalidInstruction);
            }
        }
        let old = u64::from((words[0] >> 5) & 0xffff)
            | (u64::from((words[1] >> 5) & 0xffff) << 16)
            | (u64::from((words[2] >> 5) & 0xffff) << 32)
            | (u64::from((words[3] >> 5) & 0xffff) << 48);
        let value = old.wrapping_add(slide);
        Ok([
            0xd280_0000 | ((value as u32 & 0xffff) << 5) | register,
            0xf2a0_0000 | (((value >> 16) as u32 & 0xffff) << 5) | register,
            0xf2c0_0000 | (((value >> 32) as u32 & 0xffff) << 5) | register,
            0xf2e0_0000 | (((value >> 48) as u32 & 0xffff) << 5) | register,
        ])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelocationError {
    TooMany,
    OutOfBounds,
    InvalidInstruction,
}

impl fmt::Display for RelocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for RelocationError {}
