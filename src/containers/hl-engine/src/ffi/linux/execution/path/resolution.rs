use hl_linux::ResolveFlags;
use hl_runtime::{
    GuestPathBytes, MountNamespace, OpenIntent, ReadOnlyPaths, ResolveError, ResolveHostError, RuntimePathError,
};

pub(super) struct Policy(ResolveFlags);

impl From<ResolveFlags> for Policy {
    fn from(flags: ResolveFlags) -> Self {
        Self(flags)
    }
}

impl Policy {
    pub(super) fn admit(&self) -> Result<(), RuntimePathError> {
        if self.0.cached {
            // This adapter does not own a complete dentry cache. Linux requires
            // RESOLVE_CACHED to return EAGAIN when lookup would require I/O.
            return Err(RuntimePathError::WouldBlock);
        }
        Ok(())
    }

    pub(super) fn runtime_error(error: ResolveError) -> RuntimePathError {
        match error {
            ResolveError::Path(_) => RuntimePathError::Invalid,
            ResolveError::RelativeBase => RuntimePathError::Invalid,
            ResolveError::PathTooLong | ResolveError::TooManyComponents => RuntimePathError::NameTooLong,
            ResolveError::ComponentTooLong => RuntimePathError::NameTooLong,
            ResolveError::SymlinkLoop | ResolveError::SymlinkForbidden | ResolveError::MagicLinkForbidden => {
                RuntimePathError::Loop
            }
            ResolveError::CrossDevice | ResolveError::Escape => RuntimePathError::CrossDevice,
            ResolveError::NotDirectory => RuntimePathError::NotDirectory,
            ResolveError::UnsupportedMountKind => RuntimePathError::Unsupported,
            ResolveError::Host(error) => Self::host_error(error),
        }
    }

    pub(super) fn writable(
        namespace: &MountNamespace,
        read_only: &ReadOnlyPaths,
        root_read_only: bool,
        path: &GuestPathBytes,
        intent: OpenIntent,
    ) -> Result<(), RuntimePathError> {
        let writing =
            OpenIntent::WRITE | OpenIntent::CREATE | OpenIntent::TRUNCATE | OpenIntent::APPEND | OpenIntent::TEMPORARY;
        if intent.bits() & OpenIntent::PATH_ONLY == 0
            && intent.bits() & writing != 0
            && namespace.denies_write_bytes(path, root_read_only, read_only)
        {
            Err(RuntimePathError::ReadOnly)
        } else {
            Ok(())
        }
    }

    fn host_error(error: ResolveHostError) -> RuntimePathError {
        match error {
            ResolveHostError::NotFound => RuntimePathError::NotFound,
            ResolveHostError::NotDirectory => RuntimePathError::NotDirectory,
            ResolveHostError::PermissionDenied => RuntimePathError::Access,
            ResolveHostError::ResourceLimit => RuntimePathError::TooLarge,
            ResolveHostError::Io => RuntimePathError::Io,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Policy, ResolveFlags, RuntimePathError};

    #[test]
    fn cached_retries() {
        let flags = ResolveFlags {
            cached: true,
            ..ResolveFlags::default()
        };
        assert_eq!(Policy::from(flags).admit(), Err(RuntimePathError::WouldBlock));
    }

    #[test]
    fn constrained_admitted() {
        for flags in [
            ResolveFlags {
                no_cross_device: true,
                ..ResolveFlags::default()
            },
            ResolveFlags {
                no_magic_links: true,
                ..ResolveFlags::default()
            },
            ResolveFlags {
                no_symlinks: true,
                ..ResolveFlags::default()
            },
            ResolveFlags {
                beneath: true,
                ..ResolveFlags::default()
            },
            ResolveFlags {
                in_root: true,
                ..ResolveFlags::default()
            },
        ] {
            assert_eq!(Policy::from(flags).admit(), Ok(()));
        }
    }
}
