use crate::Errno;

/// Engine-internal operation status.
///
/// The discriminants match `hl_status` in the C engine. Status remains
/// separate from [`Errno`]: conversion is a Linux-personality decision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(i32)]
pub enum Status {
    Ok = 0,
    InvalidArgument = 1,
    AbiMismatch = 2,
    NotSupported = 3,
    OutOfMemory = 4,
    ResourceLimit = 5,
    NotFound = 6,
    AlreadyExists = 7,
    PermissionDenied = 8,
    WouldBlock = 9,
    Interrupted = 10,
    Io = 11,
    PlatformFailure = 12,
    Corrupt = 13,
    Busy = 14,
    NotDirectory = 15,
    IsDirectory = 16,
    NameTooLong = 17,
    SymlinkLoop = 18,
    ReadOnly = 19,
    Disconnected = 20,
    ProcessLimit = 21,
    CrossDevice = 22,
    NotEmpty = 23,
    NoSpace = 24,
    Quota = 25,
    FileTooLarge = 26,
    TimedOut = 27,
    ConnectionRefused = 28,
    ConnectionReset = 29,
    NetworkUnreachable = 30,
    AddressInUse = 31,
}

impl Status {
    #[must_use]
    pub const fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Ok,
            1 => Self::InvalidArgument,
            2 => Self::AbiMismatch,
            3 => Self::NotSupported,
            4 => Self::OutOfMemory,
            5 => Self::ResourceLimit,
            6 => Self::NotFound,
            7 => Self::AlreadyExists,
            8 => Self::PermissionDenied,
            9 => Self::WouldBlock,
            10 => Self::Interrupted,
            11 => Self::Io,
            12 => Self::PlatformFailure,
            13 => Self::Corrupt,
            14 => Self::Busy,
            15 => Self::NotDirectory,
            16 => Self::IsDirectory,
            17 => Self::NameTooLong,
            18 => Self::SymlinkLoop,
            19 => Self::ReadOnly,
            20 => Self::Disconnected,
            21 => Self::ProcessLimit,
            22 => Self::CrossDevice,
            23 => Self::NotEmpty,
            24 => Self::NoSpace,
            25 => Self::Quota,
            26 => Self::FileTooLarge,
            27 => Self::TimedOut,
            28 => Self::ConnectionRefused,
            29 => Self::ConnectionReset,
            30 => Self::NetworkUnreachable,
            31 => Self::AddressInUse,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Converts a failed status to its Linux errno.
    ///
    /// `Ok` has no errno and is represented by `None`.
    #[must_use]
    pub const fn errno(self) -> Option<Errno> {
        Some(match self {
            Self::Ok => return None,
            Self::Interrupted => Errno::EINTR,
            Self::NotFound => Errno::EBADF,
            Self::WouldBlock => Errno::EAGAIN,
            Self::OutOfMemory => Errno::ENOMEM,
            Self::PermissionDenied => Errno::EACCES,
            Self::Busy => Errno::EBUSY,
            Self::NotDirectory => Errno::ENOTDIR,
            Self::IsDirectory => Errno::EISDIR,
            Self::NameTooLong => Errno::ENAMETOOLONG,
            Self::SymlinkLoop => Errno::ELOOP,
            Self::ReadOnly => Errno::EROFS,
            Self::AlreadyExists => Errno::EEXIST,
            Self::ResourceLimit => Errno::ENFILE,
            Self::ProcessLimit => Errno::EMFILE,
            Self::Disconnected => Errno::EPIPE,
            Self::CrossDevice => Errno::EXDEV,
            Self::NotEmpty => Errno::ENOTEMPTY,
            Self::NoSpace => Errno::ENOSPC,
            Self::Quota => Errno::EDQUOT,
            Self::FileTooLarge => Errno::EFBIG,
            Self::TimedOut => Errno::ETIMEDOUT,
            Self::ConnectionRefused => Errno::ECONNREFUSED,
            Self::ConnectionReset => Errno::ECONNRESET,
            Self::NetworkUnreachable => Errno::ENETUNREACH,
            Self::AddressInUse => Errno::EADDRINUSE,
            Self::InvalidArgument | Self::AbiMismatch | Self::Corrupt => Errno::EINVAL,
            Self::NotSupported => Errno::ENOSYS,
            Self::Io | Self::PlatformFailure => Errno::EIO,
        })
    }

    /// Returns the C personality's syscall result: zero or a negative errno.
    #[must_use]
    pub const fn linux_result(self) -> i64 {
        match self.errno() {
            Some(errno) => errno.negative_i64(),
            None => 0,
        }
    }

    /// Converts a raw C status code, preserving the default-to-`EIO` mapping.
    #[must_use]
    pub const fn result_from_code(code: i32) -> i64 {
        match Self::from_code(code) {
            Some(status) => status.linux_result(),
            None => Errno::EIO.negative_i64(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminants_match_abi() {
        for code in 0..=31 {
            let status = Status::from_code(code).expect("defined C status");
            assert_eq!(status.code(), code);
        }
        assert_eq!(Status::from_code(-1), None);
        assert_eq!(Status::from_code(32), None);
    }

    #[test]
    fn personality_results_match() {
        let expected = [
            0, -22, -22, -38, -12, -23, -9, -17, -13, -11, -4, -5, -5, -22, -16, -20, -21, -36, -40, -30, -32, -24,
            -18, -39, -28, -122, -27, -110, -111, -104, -101, -98,
        ];
        for (code, result) in expected.into_iter().enumerate() {
            assert_eq!(
                Status::from_code(i32::try_from(code).unwrap()).unwrap().linux_result(),
                result
            );
        }
    }

    #[test]
    fn limits_remain_distinct() {
        assert_eq!(Status::ResourceLimit.errno(), Some(Errno::ENFILE));
        assert_eq!(Status::ProcessLimit.errno(), Some(Errno::EMFILE));
        assert_eq!(Status::ResourceLimit.linux_result(), -23);
        assert_eq!(Status::ProcessLimit.linux_result(), -24);
    }

    #[test]
    fn unknown_codes_default() {
        assert_eq!(Status::result_from_code(-1), -5);
        assert_eq!(Status::result_from_code(32), -5);
        assert_eq!(Status::result_from_code(i32::MAX), -5);
    }
}
