use hl_linux::{Errno, MemoryMarshalError};
use hl_memory::{MemoryError, SharedError};

use crate::RuntimeMemoryError;

pub(crate) struct ErrorMap;

impl ErrorMap {
    pub(crate) fn marshal(error: MemoryMarshalError) -> Errno {
        match error {
            MemoryMarshalError::Marshal(error) => error.errno(),
            MemoryMarshalError::Invalid => Errno::EINVAL,
            MemoryMarshalError::Unsupported => Errno::EOPNOTSUPP,
            MemoryMarshalError::Overflow => Errno::EOVERFLOW,
            MemoryMarshalError::NoAddressSpace => Errno::ENOMEM,
        }
    }

    pub(crate) fn ledger(error: MemoryError) -> Errno {
        match error {
            MemoryError::AlreadyMapped => Errno::EEXIST,
            MemoryError::NoAddressSpace
            | MemoryError::ResourceLimit
            | MemoryError::Unmapped
            | MemoryError::Shared(SharedError::ResourceLimit) => Errno::ENOMEM,
            MemoryError::Shared(SharedError::Sealed) => Errno::EPERM,
            MemoryError::Shared(SharedError::Busy) => Errno::EBUSY,
            _ => Errno::EINVAL,
        }
    }

    pub(crate) fn runtime(error: RuntimeMemoryError) -> Errno {
        match error {
            RuntimeMemoryError::Invalid => Errno::EINVAL,
            RuntimeMemoryError::NoMemory => Errno::ENOMEM,
            RuntimeMemoryError::Exists => Errno::EEXIST,
            RuntimeMemoryError::BadDescriptor => Errno::EBADF,
            RuntimeMemoryError::Permission => Errno::EPERM,
            RuntimeMemoryError::Busy => Errno::EBUSY,
            RuntimeMemoryError::Unsupported => Errno::ENOSYS,
            RuntimeMemoryError::Failed => Errno::EIO,
        }
    }
}
