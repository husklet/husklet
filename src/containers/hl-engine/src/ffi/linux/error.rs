use super::abi;
use crate::native_host::HostError;
use std::io;

pub(super) struct ErrnoMapper;

impl ErrnoMapper {
    pub(super) fn current() -> HostError {
        Self::from_errno(io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }

    pub(super) fn from_errno(code: i32) -> HostError {
        match code {
            abi::EINTR => HostError::Interrupted,
            abi::EAGAIN | 114 | 115 => HostError::WouldBlock,
            code if code == abi::EINVAL || code == abi::EBADF => HostError::Invalid,
            code if code == abi::EACCES || code == abi::EPERM => HostError::Denied,
            abi::ENOENT | abi::ESRCH => HostError::NotFound,
            abi::EEXIST => HostError::Exists,
            code if code == abi::EMFILE || code == abi::ENFILE || code == abi::ENOMEM => HostError::Exhausted,
            abi::ENOTSUP => HostError::Unsupported,
            _ => HostError::Failed,
        }
    }
}

#[cfg(test)]
mod test {
    use super::{ErrnoMapper, HostError, abi};

    #[test]
    fn missing_process_maps() {
        assert_eq!(ErrnoMapper::from_errno(abi::ESRCH), HostError::NotFound);
    }
}
