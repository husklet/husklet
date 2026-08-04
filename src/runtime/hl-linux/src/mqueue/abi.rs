use crate::{GuestAccess, GuestMemory};

const ATTRIBUTE_SIZE: usize = 32;
const EVENT_SIZE: usize = 64;
const NAME_MAXIMUM: usize = 255;
const PRIORITY_MAXIMUM: u32 = 32_768;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Fault,
    Invalid,
    NameTooLong,
    MessageTooBig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Attributes {
    pub flags: i64,
    pub maximum_messages: i64,
    pub message_bytes: i64,
    pub current_messages: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Timespec {
    pub seconds: u64,
    pub nanoseconds: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Notify {
    Signal,
    None,
    Thread,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub notify: Notify,
    pub signal: i32,
    pub value: u64,
}

pub struct Abi<'a, M> {
    memory: &'a M,
}

impl<'a, M: GuestMemory> Abi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M) -> Self {
        Self { memory }
    }

    /// Copies a kernel-facing mqueue name component, including its terminator.
    ///
    /// # Errors
    ///
    /// Returns a fault for inaccessible storage, invalid for empty names or
    /// embedded slashes, and name-too-long if no terminator occurs in bounds.
    pub fn name(&self, address: u64) -> Result<Vec<u8>, Error> {
        if address == 0 {
            return Err(Error::Fault);
        }
        let mut name = Vec::with_capacity(NAME_MAXIMUM);
        for index in 0..=NAME_MAXIMUM {
            let location = address.checked_add(index as u64).ok_or(Error::Fault)?;
            let byte = self.read::<1>(location)?[0];
            if byte == 0 {
                if name.is_empty() || name.contains(&b'/') {
                    return Err(Error::Invalid);
                }
                return Ok(name);
            }
            name.push(byte);
        }
        Err(Error::NameTooLong)
    }

    /// # Errors
    ///
    /// Returns a fault unless the complete LP64 structure is readable.
    pub fn attributes(&self, address: u64) -> Result<Attributes, Error> {
        let bytes = self.read::<ATTRIBUTE_SIZE>(address)?;
        Ok(Attributes {
            flags: i64::from_ne_bytes(bytes[0..8].try_into().unwrap_or([0; 8])),
            maximum_messages: i64::from_ne_bytes(bytes[8..16].try_into().unwrap_or([0; 8])),
            message_bytes: i64::from_ne_bytes(bytes[16..24].try_into().unwrap_or([0; 8])),
            current_messages: i64::from_ne_bytes(bytes[24..32].try_into().unwrap_or([0; 8])),
        })
    }

    /// Validates the absolute `CLOCK_REALTIME` timeout before descriptor lookup.
    ///
    /// # Errors
    ///
    /// Returns a fault for inaccessible storage and invalid for negative seconds
    /// or nanoseconds outside `[0, 1_000_000_000)`.
    pub fn timeout(&self, address: u64) -> Result<Option<Timespec>, Error> {
        if address == 0 {
            return Ok(None);
        }
        let bytes = self.read::<16>(address)?;
        let seconds = i64::from_ne_bytes(bytes[0..8].try_into().unwrap_or([0; 8]));
        let nanoseconds = i64::from_ne_bytes(bytes[8..16].try_into().unwrap_or([0; 8]));
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(Error::Invalid);
        }
        Ok(Some(Timespec {
            seconds: u64::try_from(seconds).map_err(|_| Error::Invalid)?,
            nanoseconds: u32::try_from(nanoseconds).map_err(|_| Error::Invalid)?,
        }))
    }

    /// Copies and validates the complete 64-byte LP64 `sigevent`.
    ///
    /// # Errors
    ///
    /// Returns a fault for inaccessible storage and invalid for unsupported
    /// notification modes or an invalid signal notification number.
    pub fn event(&self, address: u64) -> Result<Event, Error> {
        let bytes = self.read::<EVENT_SIZE>(address)?;
        let value = u64::from_ne_bytes(bytes[0..8].try_into().unwrap_or([0; 8]));
        let signal = i32::from_ne_bytes(bytes[8..12].try_into().unwrap_or([0; 4]));
        let notify = match i32::from_ne_bytes(bytes[12..16].try_into().unwrap_or([0; 4])) {
            0 => Notify::Signal,
            1 => Notify::None,
            2 => Notify::Thread,
            _ => return Err(Error::Invalid),
        };
        if notify == Notify::Signal && !(1..=64).contains(&signal) {
            return Err(Error::Invalid);
        }
        Ok(Event { notify, signal, value })
    }

    /// # Errors
    ///
    /// Returns invalid when the priority exceeds Linux `MQ_PRIO_MAX`.
    pub const fn priority(priority: u32) -> Result<u32, Error> {
        if priority < PRIORITY_MAXIMUM {
            Ok(priority)
        } else {
            Err(Error::Invalid)
        }
    }

    /// Copies a send payload only after the caller supplies queue geometry.
    ///
    /// # Errors
    ///
    /// Returns message-too-big before allocation, or a fault unless the complete
    /// payload is readable.
    pub fn message(&self, address: u64, length: usize, maximum: usize) -> Result<Vec<u8>, Error> {
        if length > maximum {
            return Err(Error::MessageTooBig);
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        let mut bytes = vec![0; length];
        if self.memory.read(address, &mut bytes).map_err(|_| Error::Fault)? != length {
            return Err(Error::Fault);
        }
        Ok(bytes)
    }

    /// Preflights an attribute destination without mutating guest memory.
    ///
    /// # Errors
    ///
    /// Returns a fault unless all 32 bytes are writable.
    pub fn stage_attributes(&self, address: u64, value: Attributes) -> Result<StagedAttributes<'a, M>, Error> {
        self.probe(address, ATTRIBUTE_SIZE)?;
        Ok(StagedAttributes {
            memory: self.memory,
            address,
            value,
        })
    }

    /// Preflights both receive destinations before a queue message is consumed.
    ///
    /// # Errors
    ///
    /// Returns a fault unless the payload and optional priority are writable.
    pub fn stage_receive(&self, payload: u64, length: usize, priority: u64) -> Result<StagedReceive<'a, M>, Error> {
        self.probe(payload, length)?;
        if priority != 0 {
            self.probe(priority, 4)?;
        }
        Ok(StagedReceive {
            memory: self.memory,
            payload,
            length,
            priority: (priority != 0).then_some(priority),
        })
    }

    fn probe(&self, address: u64, length: usize) -> Result<(), Error> {
        if length != 0
            && self
                .memory
                .probe(address, length, GuestAccess::Write)
                .map_err(|_| Error::Fault)?
                != length
        {
            return Err(Error::Fault);
        }
        Ok(())
    }

    fn read<const N: usize>(&self, address: u64) -> Result<[u8; N], Error> {
        let mut bytes = [0; N];
        if self.memory.read(address, &mut bytes).map_err(|_| Error::Fault)? != N {
            return Err(Error::Fault);
        }
        Ok(bytes)
    }
}

pub struct StagedAttributes<'a, M> {
    memory: &'a M,
    address: u64,
    value: Attributes,
}

impl<M: GuestMemory> StagedAttributes<'_, M> {
    /// # Errors
    ///
    /// Returns a fault if the preflighted mapping changed before publication.
    pub fn commit(self) -> Result<(), Error> {
        let mut bytes = [0; ATTRIBUTE_SIZE];
        bytes[0..8].copy_from_slice(&self.value.flags.to_ne_bytes());
        bytes[8..16].copy_from_slice(&self.value.maximum_messages.to_ne_bytes());
        bytes[16..24].copy_from_slice(&self.value.message_bytes.to_ne_bytes());
        bytes[24..32].copy_from_slice(&self.value.current_messages.to_ne_bytes());
        write_exact(self.memory, self.address, &bytes)
    }
}

pub struct StagedReceive<'a, M> {
    memory: &'a M,
    payload: u64,
    length: usize,
    priority: Option<u64>,
}

impl<M: GuestMemory> StagedReceive<'_, M> {
    /// # Errors
    ///
    /// Returns invalid for a mismatched payload and fault if mappings changed.
    pub fn commit(self, payload: &[u8], priority: u32) -> Result<(), Error> {
        if payload.len() != self.length {
            return Err(Error::Invalid);
        }
        write_exact(self.memory, self.payload, payload)?;
        if let Some(address) = self.priority {
            write_exact(self.memory, address, &priority.to_ne_bytes())?;
        }
        Ok(())
    }
}

fn write_exact<M: GuestMemory>(memory: &M, address: u64, bytes: &[u8]) -> Result<(), Error> {
    if bytes.is_empty() {
        return Ok(());
    }
    if memory.write(address, bytes).map_err(|_| Error::Fault)? != bytes.len() {
        return Err(Error::Fault);
    }
    Ok(())
}
