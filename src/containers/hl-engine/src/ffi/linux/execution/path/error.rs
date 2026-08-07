use hl_runtime::RuntimePathError;

use super::native;

pub(super) struct HostError;

impl HostError {
    pub(super) fn map(error: std::io::Error) -> RuntimePathError {
        if let Some(errno) = error.raw_os_error() {
            return match errno {
                native::LOOP => RuntimePathError::Loop,
                libc::EISDIR => RuntimePathError::IsDirectory,
                libc::ENAMETOOLONG => RuntimePathError::NameTooLong,
                libc::EXDEV => RuntimePathError::CrossDevice,
                libc::EROFS => RuntimePathError::ReadOnly,
                libc::ENOTEMPTY => RuntimePathError::DirectoryNotEmpty,
                libc::EPERM => RuntimePathError::OperationNotPermitted,
                libc::EFBIG => RuntimePathError::FileTooLarge,
                libc::ETXTBSY => RuntimePathError::TextBusy,
                libc::EDQUOT => RuntimePathError::Quota,
                libc::EOPNOTSUPP => RuntimePathError::NotSupported,
                _ => Self::kind(error),
            };
        }
        Self::kind(error)
    }

    fn kind(error: std::io::Error) -> RuntimePathError {
        match error.kind() {
            std::io::ErrorKind::NotFound => RuntimePathError::NotFound,
            std::io::ErrorKind::AlreadyExists => RuntimePathError::Exists,
            std::io::ErrorKind::PermissionDenied => RuntimePathError::Access,
            std::io::ErrorKind::NotADirectory => RuntimePathError::NotDirectory,
            std::io::ErrorKind::InvalidInput => RuntimePathError::Invalid,
            _ => RuntimePathError::Io,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HostError;
    use hl_runtime::RuntimePathError;

    #[test]
    fn preserves_linux_path_errno_classes() {
        let cases = [
            (libc::EISDIR, RuntimePathError::IsDirectory),
            (libc::ENAMETOOLONG, RuntimePathError::NameTooLong),
            (libc::EFBIG, RuntimePathError::FileTooLarge),
            (libc::ETXTBSY, RuntimePathError::TextBusy),
            (libc::EDQUOT, RuntimePathError::Quota),
            (libc::EOPNOTSUPP, RuntimePathError::NotSupported),
        ];

        for (raw, expected) in cases {
            assert_eq!(HostError::map(std::io::Error::from_raw_os_error(raw)), expected);
            assert_eq!(
                HostError::map(std::io::Error::from_raw_os_error(raw)).errno().raw(),
                raw
            );
        }
    }
}
