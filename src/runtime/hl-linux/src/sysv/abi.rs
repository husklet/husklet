use hl_isa::GuestArchitecture;

use super::values::{IPC_CREAT, IPC_EXCL};
use crate::{Errno, GuestAccess, GuestMarshaller, GuestMemory, IpcCommand, MSG_NOWAIT, MarshalError};

const SEMOP_MAXIMUM: usize = 500;
const MESSAGE_MAXIMUM: usize = 8_192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiError {
    Fault,
    Invalid,
    TooBig,
    Overflow,
}

impl AbiError {
    #[must_use]
    pub fn errno(self) -> Errno {
        let errno = match self {
            Self::Fault => Errno::EFAULT,
            Self::Invalid => Errno::EINVAL,
            Self::TooBig => Errno::E2BIG,
            Self::Overflow => Errno::EOVERFLOW,
        };
        hl_log::hl_debug!(hl_log::tag::IPC, "sysv abi error mapped error={:?} errno={}", self, errno.raw());
        errno
    }
}

impl From<MarshalError> for AbiError {
    fn from(error: MarshalError) -> Self {
        match error {
            MarshalError::Fault(_) => Self::Fault,
            MarshalError::Invalid => Self::Invalid,
            MarshalError::TooBig => Self::TooBig,
            MarshalError::Overflow => Self::Overflow,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedSysvCopyout {
    pub(super) destination: u64,
    pub(super) bytes: Vec<u8>,
}

impl StagedSysvCopyout {
    pub fn commit<M: GuestMemory>(self, marshaller: &GuestMarshaller<'_, M>) -> Result<(), AbiError> {
        marshaller
            .copy_to(self.destination, &self.bytes)
            .fault
            .map_or(Ok(()), |_| Err(AbiError::Fault))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identifier(pub i32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawIndex(pub i32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryGetPlan {
    pub key: i32,
    pub size: u64,
    pub mode: u16,
    pub create: bool,
    pub exclusive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryAttachPlan {
    pub identifier: Identifier,
    pub address: u64,
    pub flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryControlPlan {
    Remove { identifier: Identifier },
    Set { identifier: Identifier, source: u64 },
    Stat { identifier: Identifier, output: u64 },
    Information { usage: bool, output: u64 },
    IndexStat { index: RawIndex, any: bool, output: u64 },
    Lock { identifier: Identifier, unlock: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemaphoreGetPlan {
    pub key: i32,
    pub semaphores: i32,
    pub mode: u16,
    pub create: bool,
    pub exclusive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemaphoreOperation {
    pub index: u16,
    pub delta: i16,
    pub flags: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemaphoreOperatePlan {
    pub identifier: Identifier,
    pub operations: Vec<SemaphoreOperation>,
    pub timeout: Option<(i64, i64)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemaphoreControlPlan {
    Remove {
        identifier: Identifier,
    },
    Set {
        identifier: Identifier,
        source: u64,
    },
    Stat {
        identifier: Identifier,
        output: u64,
    },
    Information {
        usage: bool,
        output: u64,
    },
    IndexStat {
        index: RawIndex,
        any: bool,
        output: u64,
    },
    Scalar {
        identifier: Identifier,
        index: i32,
        command: IpcCommand,
        value: i32,
    },
    Array {
        identifier: Identifier,
        command: IpcCommand,
        address: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageGetPlan {
    pub key: i32,
    pub mode: u16,
    pub create: bool,
    pub exclusive: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageSendPlan {
    pub identifier: Identifier,
    pub message_type: i64,
    pub bytes: Vec<u8>,
    pub nowait: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageReceivePlan {
    pub identifier: Identifier,
    pub output: u64,
    pub maximum: usize,
    pub message_type: i64,
    pub flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessageControlPlan {
    Remove { identifier: Identifier },
    Set { identifier: Identifier, source: u64 },
    Stat { identifier: Identifier, output: u64 },
    Information { usage: bool, output: u64 },
    IndexStat { index: RawIndex, any: bool, output: u64 },
}

pub struct Abi<'a, M: GuestMemory> {
    pub(super) marshaller: GuestMarshaller<'a, M>,
}

impl<'a, M: GuestMemory> Abi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M, architecture: GuestArchitecture) -> Self {
        Self {
            marshaller: GuestMarshaller::new(memory, architecture),
        }
    }

    #[must_use]
    pub const fn shmget(key: u64, size: u64, flags: u32) -> MemoryGetPlan {
        MemoryGetPlan {
            key: key as i32,
            size,
            mode: (flags & 0o777) as u16,
            create: flags & IPC_CREAT != 0,
            exclusive: flags & IPC_EXCL != 0,
        }
    }

    #[must_use]
    pub const fn shmat(identifier: u64, address: u64, flags: u32) -> MemoryAttachPlan {
        MemoryAttachPlan {
            identifier: Identifier(identifier as i32),
            address,
            flags,
        }
    }

    #[must_use]
    pub const fn shmdt(address: u64) -> u64 {
        address
    }

    pub fn shmctl(&self, identifier: u64, command: u32, buffer: u64) -> Result<MemoryControlPlan, AbiError> {
        let identifier = Identifier(identifier as i32);
        match command {
            0 => Ok(MemoryControlPlan::Remove { identifier }),
            1 => Ok(MemoryControlPlan::Set {
                identifier,
                source: buffer,
            }),
            2 => Ok(MemoryControlPlan::Stat {
                identifier,
                output: buffer,
            }),
            3 | 14 => Ok(MemoryControlPlan::Information {
                usage: command == 14,
                output: buffer,
            }),
            11 | 12 => Ok(MemoryControlPlan::Lock {
                identifier,
                unlock: command == 12,
            }),
            13 | 15 => Ok(MemoryControlPlan::IndexStat {
                index: RawIndex(identifier.0),
                any: command == 15,
                output: buffer,
            }),
            _ => Err(AbiError::Invalid),
        }
    }

    #[must_use]
    pub const fn semget(key: u64, count: u64, flags: u32) -> SemaphoreGetPlan {
        SemaphoreGetPlan {
            key: key as i32,
            semaphores: count as i32,
            mode: (flags & 0o777) as u16,
            create: flags & IPC_CREAT != 0,
            exclusive: flags & IPC_EXCL != 0,
        }
    }

    pub fn semop(
        &self,
        identifier: u64,
        operations: u64,
        count: usize,
        timeout: Option<u64>,
    ) -> Result<SemaphoreOperatePlan, AbiError> {
        if count == 0 {
            return Err(AbiError::Invalid);
        }
        if count > SEMOP_MAXIMUM {
            return Err(AbiError::TooBig);
        }
        let bytes = self.read(operations, count.checked_mul(6).ok_or(AbiError::Overflow)?)?;
        let operations = bytes
            .chunks_exact(6)
            .map(|item| SemaphoreOperation {
                index: u16::from_le_bytes([item[0], item[1]]),
                delta: i16::from_le_bytes([item[2], item[3]]),
                flags: u16::from_le_bytes([item[4], item[5]]),
            })
            .collect::<Vec<_>>();
        let timeout = timeout
            .filter(|address| *address != 0)
            .map(|address| self.read_timespec(address))
            .transpose()?;
        Ok(SemaphoreOperatePlan {
            identifier: Identifier(identifier as i32),
            operations,
            timeout,
        })
    }

    pub fn semctl(
        &self,
        identifier: u64,
        index: u64,
        command: u32,
        argument: u64,
    ) -> Result<SemaphoreControlPlan, AbiError> {
        let identifier = Identifier(identifier as i32);
        let index = index as i32;
        match command {
            0 => Ok(SemaphoreControlPlan::Remove { identifier }),
            1 => Ok(SemaphoreControlPlan::Set {
                identifier,
                source: argument,
            }),
            2 => Ok(SemaphoreControlPlan::Stat {
                identifier,
                output: argument,
            }),
            3 | 19 => Ok(SemaphoreControlPlan::Information {
                usage: command == 19,
                output: argument,
            }),
            18 | 20 => Ok(SemaphoreControlPlan::IndexStat {
                index: RawIndex(identifier.0),
                any: command == 20,
                output: argument,
            }),
            11 => Ok(Self::sem_scalar(identifier, index, IpcCommand::GetPid, 0)),
            12 => Ok(Self::sem_scalar(identifier, index, IpcCommand::GetValue, 0)),
            13 => Ok(SemaphoreControlPlan::Array {
                identifier,
                command: IpcCommand::GetAll,
                address: argument,
            }),
            14 => Ok(Self::sem_scalar(identifier, index, IpcCommand::GetDecrementWaiters, 0)),
            15 => Ok(Self::sem_scalar(identifier, index, IpcCommand::GetZeroWaiters, 0)),
            16 => Ok(Self::sem_scalar(
                identifier,
                index,
                IpcCommand::SetValue,
                argument as i32,
            )),
            17 => Ok(SemaphoreControlPlan::Array {
                identifier,
                command: IpcCommand::SetAll,
                address: argument,
            }),
            _ => Err(AbiError::Invalid),
        }
    }

    #[must_use]
    pub const fn msgget(key: u64, flags: u32) -> MessageGetPlan {
        MessageGetPlan {
            key: key as i32,
            mode: (flags & 0o777) as u16,
            create: flags & IPC_CREAT != 0,
            exclusive: flags & IPC_EXCL != 0,
        }
    }

    pub fn msgsnd(&self, identifier: u64, message: u64, size: usize, flags: u32) -> Result<MessageSendPlan, AbiError> {
        if size > MESSAGE_MAXIMUM {
            return Err(AbiError::Invalid);
        }
        let bytes = self.read(message, size.checked_add(8).ok_or(AbiError::Overflow)?)?;
        let message_type = i64::from_le_bytes(bytes[..8].try_into().expect("eight bytes"));
        if message_type < 1 {
            return Err(AbiError::Invalid);
        }
        Ok(MessageSendPlan {
            identifier: Identifier(identifier as i32),
            message_type,
            bytes: bytes[8..].to_vec(),
            nowait: flags & MSG_NOWAIT != 0,
        })
    }

    pub fn msgrcv(
        &self,
        identifier: u64,
        output: u64,
        maximum: usize,
        message_type: u64,
        flags: u32,
    ) -> Result<MessageReceivePlan, AbiError> {
        let length = maximum.checked_add(8).ok_or(AbiError::Overflow)?;
        self.preflight(output, length)?;
        Ok(MessageReceivePlan {
            identifier: Identifier(identifier as i32),
            output,
            maximum,
            message_type: message_type as i64,
            flags,
        })
    }

    pub fn msgctl(&self, identifier: u64, command: u32, buffer: u64) -> Result<MessageControlPlan, AbiError> {
        let identifier = Identifier(identifier as i32);
        match command {
            0 => Ok(MessageControlPlan::Remove { identifier }),
            1 => Ok(MessageControlPlan::Set {
                identifier,
                source: buffer,
            }),
            2 => Ok(MessageControlPlan::Stat {
                identifier,
                output: buffer,
            }),
            3 | 12 => Ok(MessageControlPlan::Information {
                usage: command == 12,
                output: buffer,
            }),
            11 | 13 => Ok(MessageControlPlan::IndexStat {
                index: RawIndex(identifier.0),
                any: command == 13,
                output: buffer,
            }),
            _ => Err(AbiError::Invalid),
        }
    }

    pub(super) fn read(&self, address: u64, length: usize) -> Result<Vec<u8>, AbiError> {
        let mut bytes = vec![0; length];
        let progress = self.marshaller.copy_from(address, &mut bytes);
        progress.fault.map_or(Ok(bytes), |_| Err(AbiError::Fault))
    }

    pub(super) fn preflight(&self, address: u64, length: usize) -> Result<(), AbiError> {
        match self.marshaller.probe(address, length, GuestAccess::Write) {
            Ok(available) if available == length => Ok(()),
            // A wrapped or otherwise inaccessible user range is EFAULT for
            // SysV IPC copyout, just like copy_to/copy_from and Linux's
            // copy_to_user boundary. EOVERFLOW is reserved for IPC values
            // that cannot be represented, not malformed pointers.
            Ok(_) | Err(_) => Err(AbiError::Fault),
        }
    }

    fn read_timespec(&self, address: u64) -> Result<(i64, i64), AbiError> {
        let bytes = self.read(address, 16)?;
        let seconds = i64::from_le_bytes(bytes[..8].try_into().expect("eight bytes"));
        let nanoseconds = i64::from_le_bytes(bytes[8..].try_into().expect("eight bytes"));
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(AbiError::Invalid);
        }
        Ok((seconds, nanoseconds))
    }

    const fn sem_scalar(identifier: Identifier, index: i32, command: IpcCommand, value: i32) -> SemaphoreControlPlan {
        SemaphoreControlPlan::Scalar {
            identifier,
            index,
            command,
            value,
        }
    }
}
