use hl_descriptor::{DescriptorError, ObjectError};
use hl_linux::{Errno, FilesystemMarshalError};

pub(crate) struct FileErrno;

pub(crate) use FileErrno as FilesystemErrno;

impl FileErrno {
    pub(crate) fn descriptor(error: DescriptorError) -> Errno {
        match error {
            DescriptorError::BadDescriptor => Errno::EBADF,
            DescriptorError::InvalidArgument | DescriptorError::AlreadyExists => Errno::EINVAL,
            DescriptorError::TooManyOpenFiles => Errno::EMFILE,
            DescriptorError::CheckpointFrozen => Errno::EBUSY,
            DescriptorError::StaleReservation | DescriptorError::Corrupt => Errno::EIO,
        }
    }

    pub(crate) fn marshal(error: FilesystemMarshalError) -> Errno {
        match error {
            FilesystemMarshalError::Marshal(error) => error.errno(),
            FilesystemMarshalError::NoEntry => Errno::ENOENT,
            FilesystemMarshalError::Invalid => Errno::EINVAL,
            FilesystemMarshalError::Range => Errno::ERANGE,
            FilesystemMarshalError::TooBig => Errno::E2BIG,
            FilesystemMarshalError::NameTooLong => Errno::ENAMETOOLONG,
            FilesystemMarshalError::Overflow | FilesystemMarshalError::Encoding => Errno::EOVERFLOW,
        }
    }

    pub(crate) fn object(error: ObjectError) -> Errno {
        match error {
            ObjectError::BadDescriptor | ObjectError::Retired => Errno::EBADF,
            ObjectError::NoSuchProcess => Errno::ESRCH,
            ObjectError::InvalidArgument => Errno::EINVAL,
            ObjectError::WouldBlock => Errno::EAGAIN,
            ObjectError::Interrupted | ObjectError::Canceled => Errno::EINTR,
            ObjectError::ResourceLimit => Errno::ENFILE,
            ObjectError::NoSpace => Errno::ENOSPC,
            ObjectError::NoExtent => Errno::ENXIO,
            ObjectError::PermissionDenied => Errno::EPERM,
            ObjectError::Busy => Errno::EBUSY,
            ObjectError::BrokenPipe => Errno::EPIPE,
            ObjectError::NotSupported => Errno::ENOSYS,
            ObjectError::Io => Errno::EIO,
        }
    }
}
