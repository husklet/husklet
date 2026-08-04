use hl_descriptor::ObjectError;
use hl_runtime::RuntimePathError;

pub(super) struct Error;

impl Error {
    pub(super) fn path(error: crate::native::ProjectionError) -> RuntimePathError {
        match error {
            crate::native::ProjectionError::Linux(2) => RuntimePathError::NotFound,
            crate::native::ProjectionError::Linux(13) => RuntimePathError::Access,
            crate::native::ProjectionError::Linux(17) => RuntimePathError::Exists,
            crate::native::ProjectionError::Linux(28) => RuntimePathError::NoSpace,
            crate::native::ProjectionError::Linux(39) => RuntimePathError::DirectoryNotEmpty,
            crate::native::ProjectionError::Linux(1) => RuntimePathError::OperationNotPermitted,
            crate::native::ProjectionError::Linux(30) => RuntimePathError::ReadOnly,
            crate::native::ProjectionError::Linux(20) => RuntimePathError::NotDirectory,
            crate::native::ProjectionError::Linux(36) => RuntimePathError::NameTooLong,
            crate::native::ProjectionError::Linux(40) => RuntimePathError::Loop,
            crate::native::ProjectionError::Linux(18) => RuntimePathError::CrossDevice,
            crate::native::ProjectionError::Linux(22) => RuntimePathError::Invalid,
            crate::native::ProjectionError::Linux(9) => RuntimePathError::BadDescriptor,
            crate::native::ProjectionError::Linux(_) | crate::native::ProjectionError::Session => RuntimePathError::Io,
        }
    }

    pub(super) fn object(error: crate::native::ProjectionError) -> ObjectError {
        match error {
            crate::native::ProjectionError::Linux(9) => ObjectError::BadDescriptor,
            crate::native::ProjectionError::Linux(11) => ObjectError::WouldBlock,
            crate::native::ProjectionError::Linux(4) => ObjectError::Interrupted,
            crate::native::ProjectionError::Linux(13 | 30) => ObjectError::PermissionDenied,
            crate::native::ProjectionError::Linux(16) => ObjectError::Busy,
            crate::native::ProjectionError::Linux(23 | 24) => ObjectError::ResourceLimit,
            crate::native::ProjectionError::Linux(28) => ObjectError::NoSpace,
            crate::native::ProjectionError::Linux(32) => ObjectError::BrokenPipe,
            crate::native::ProjectionError::Linux(125) => ObjectError::Canceled,
            crate::native::ProjectionError::Linux(22) => ObjectError::InvalidArgument,
            crate::native::ProjectionError::Linux(_) | crate::native::ProjectionError::Session => ObjectError::Io,
        }
    }
}
