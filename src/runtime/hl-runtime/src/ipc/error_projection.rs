use hl_ipc::{MessageError, SemaphoreError, SharedMemoryError};
use hl_linux::Errno;

pub(super) struct ErrorProjection;

impl ErrorProjection {
    pub(super) const fn shared_get(error: SharedMemoryError) -> Errno {
        match error {
            SharedMemoryError::NotFound => Errno::ENOENT,
            SharedMemoryError::Exists => Errno::EEXIST,
            SharedMemoryError::Permission => Errno::EACCES,
            SharedMemoryError::ResourceLimit => Errno::ENOSPC,
            SharedMemoryError::Shared(_) => Errno::ENOMEM,
            _ => Errno::EINVAL,
        }
    }

    pub(super) const fn message_get(error: MessageError) -> Errno {
        match error {
            MessageError::NotFound => Errno::ENOENT,
            MessageError::Exists => Errno::EEXIST,
            MessageError::Permission => Errno::EACCES,
            MessageError::ResourceLimit => Errno::ENOSPC,
            _ => Errno::EINVAL,
        }
    }

    pub(super) const fn message(error: MessageError) -> Errno {
        match error {
            MessageError::Permission => Errno::EACCES,
            MessageError::Again => Errno::EAGAIN,
            MessageError::NoMessage => Errno::from_raw(42),
            MessageError::TooBig => Errno::E2BIG,
            MessageError::ResourceLimit => Errno::ENOSPC,
            MessageError::Interrupted => Errno::EINTR,
            MessageError::TimedOut => Errno::EAGAIN,
            MessageError::Clock => Errno::EIO,
            _ => Errno::EINVAL,
        }
    }

    pub(super) const fn semaphore_get(error: SemaphoreError) -> Errno {
        match error {
            SemaphoreError::NotFound => Errno::ENOENT,
            SemaphoreError::Exists => Errno::EEXIST,
            SemaphoreError::Permission => Errno::EACCES,
            SemaphoreError::ResourceLimit => Errno::ENOSPC,
            _ => Errno::EINVAL,
        }
    }

    pub(super) const fn semaphore(error: SemaphoreError) -> Errno {
        match error {
            SemaphoreError::Permission => Errno::EACCES,
            SemaphoreError::ResourceLimit => Errno::ENOSPC,
            SemaphoreError::Range => Errno::from_raw(34),
            SemaphoreError::Again => Errno::EAGAIN,
            SemaphoreError::Interrupted => Errno::EINTR,
            SemaphoreError::TimedOut => Errno::EAGAIN,
            SemaphoreError::Clock => Errno::EIO,
            _ => Errno::EINVAL,
        }
    }
}
