use std::marker::PhantomData;
use std::time::Duration;

use crate::{GuestAccess, GuestMemory};

pub const IOCB_FLAG_RESFD: u32 = 1;
const CONTROL_SIZE: usize = 64;
const EVENT_SIZE: usize = 32;
const TIMESPEC_SIZE: usize = 16;
pub const SUBMISSION_MAXIMUM: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Opcode {
    Pread,
    Pwrite,
    Fsync,
    Fdatasync,
    Preadv,
    Pwritev,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlBlock {
    pub address: u64,
    pub data: u64,
    pub opcode: Opcode,
    pub priority: i16,
    pub descriptor: i32,
    pub buffer: u64,
    pub count: u64,
    pub offset: i64,
    pub flags: u32,
    pub result_descriptor: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub data: u64,
    pub object: u64,
    pub result: i64,
    pub secondary: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarshalError {
    Fault,
    Invalid,
}

pub struct Abi<'a, M> {
    memory: &'a M,
    marker: PhantomData<M>,
}

impl<'a, M: GuestMemory> Abi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M) -> Self {
        Self {
            memory,
            marker: PhantomData,
        }
    }

    pub fn context(&self, address: u64) -> Result<u64, MarshalError> {
        let bytes = self.read_exact(address, 8)?;
        Ok(u64::from_ne_bytes(bytes.try_into().unwrap_or([0; 8])))
    }

    pub fn write_context(&self, address: u64, value: u64) -> Result<(), MarshalError> {
        self.write_exact(address, &value.to_ne_bytes())
    }

    pub fn controls(&self, address: u64, count: u64) -> Result<Vec<ControlBlock>, MarshalError> {
        self.pointers(address, count)?
            .into_iter()
            .map(|location| {
                if location == 0 {
                    Err(MarshalError::Fault)
                } else {
                    self.control(location)
                }
            })
            .collect()
    }

    pub fn pointers(&self, address: u64, count: u64) -> Result<Vec<u64>, MarshalError> {
        let count = usize::try_from(count).map_err(|_| MarshalError::Invalid)?;
        if count > SUBMISSION_MAXIMUM {
            return Err(MarshalError::Invalid);
        }
        let size = count.checked_mul(8).ok_or(MarshalError::Invalid)?;
        let pointers = self.read_exact(address, size)?;
        let mut controls = Vec::with_capacity(count);
        for pointer in pointers.chunks_exact(8) {
            let location = u64::from_ne_bytes(pointer.try_into().unwrap_or([0; 8]));
            controls.push(location);
        }
        Ok(controls)
    }

    pub fn control(&self, address: u64) -> Result<ControlBlock, MarshalError> {
        let bytes = self.read_exact(address, CONTROL_SIZE)?;
        let opcode = match u16::from_ne_bytes([bytes[16], bytes[17]]) {
            0 => Opcode::Pread,
            1 => Opcode::Pwrite,
            2 => Opcode::Fsync,
            3 => Opcode::Fdatasync,
            7 => Opcode::Preadv,
            8 => Opcode::Pwritev,
            _ => return Err(MarshalError::Invalid),
        };
        Ok(ControlBlock {
            address,
            data: u64::from_ne_bytes(bytes[0..8].try_into().unwrap_or([0; 8])),
            opcode,
            priority: i16::from_ne_bytes(bytes[18..20].try_into().unwrap_or([0; 2])),
            descriptor: u32::from_ne_bytes(bytes[20..24].try_into().unwrap_or([0; 4])) as i32,
            buffer: u64::from_ne_bytes(bytes[24..32].try_into().unwrap_or([0; 8])),
            count: u64::from_ne_bytes(bytes[32..40].try_into().unwrap_or([0; 8])),
            offset: i64::from_ne_bytes(bytes[40..48].try_into().unwrap_or([0; 8])),
            flags: u32::from_ne_bytes(bytes[56..60].try_into().unwrap_or([0; 4])),
            result_descriptor: u32::from_ne_bytes(bytes[60..64].try_into().unwrap_or([0; 4])) as i32,
        })
    }

    pub fn timeout(&self, address: u64) -> Result<Option<Duration>, MarshalError> {
        if address == 0 {
            return Ok(None);
        }
        let bytes = self.read_exact(address, TIMESPEC_SIZE)?;
        let seconds = i64::from_ne_bytes(bytes[0..8].try_into().unwrap_or([0; 8]));
        let nanos = i64::from_ne_bytes(bytes[8..16].try_into().unwrap_or([0; 8]));
        if seconds < 0 || !(0..1_000_000_000).contains(&nanos) {
            return Err(MarshalError::Invalid);
        }
        let seconds = u64::try_from(seconds).map_err(|_| MarshalError::Invalid)?;
        Ok(Some(Duration::new(seconds, nanos as u32)))
    }

    pub fn stage_events(&self, address: u64, maximum: usize) -> Result<StagedEvents<'_, M>, MarshalError> {
        let length = maximum.checked_mul(EVENT_SIZE).ok_or(MarshalError::Invalid)?;
        if length != 0
            && self
                .memory
                .probe(address, length, GuestAccess::Write)
                .map_err(|_| MarshalError::Fault)?
                != length
        {
            return Err(MarshalError::Fault);
        }
        Ok(StagedEvents {
            memory: self.memory,
            address,
            maximum,
        })
    }

    fn read_exact(&self, address: u64, length: usize) -> Result<Vec<u8>, MarshalError> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let mut bytes = vec![0; length];
        if self.memory.read(address, &mut bytes).map_err(|_| MarshalError::Fault)? != length {
            return Err(MarshalError::Fault);
        }
        Ok(bytes)
    }

    fn write_exact(&self, address: u64, bytes: &[u8]) -> Result<(), MarshalError> {
        if self.memory.write(address, bytes).map_err(|_| MarshalError::Fault)? != bytes.len() {
            return Err(MarshalError::Fault);
        }
        Ok(())
    }
}

pub struct StagedEvents<'a, M> {
    memory: &'a M,
    address: u64,
    maximum: usize,
}

impl<M: GuestMemory> StagedEvents<'_, M> {
    pub fn publish(&self, events: &[Event]) -> Result<(), MarshalError> {
        if events.len() > self.maximum {
            return Err(MarshalError::Invalid);
        }
        let mut bytes = Vec::with_capacity(events.len() * EVENT_SIZE);
        for event in events {
            bytes.extend_from_slice(&event.data.to_ne_bytes());
            bytes.extend_from_slice(&event.object.to_ne_bytes());
            bytes.extend_from_slice(&event.result.to_ne_bytes());
            bytes.extend_from_slice(&event.secondary.to_ne_bytes());
        }
        if bytes.is_empty() {
            return Ok(());
        }
        if self
            .memory
            .write(self.address, &bytes)
            .map_err(|_| MarshalError::Fault)?
            != bytes.len()
        {
            return Err(MarshalError::Fault);
        }
        Ok(())
    }
}
