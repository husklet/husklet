use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::File;
use std::io::{Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hl_descriptor::ObjectError;
use hl_runtime::{MountSourceId, RuntimePathError};

use super::HostError;

const POSIX_SHM_SOURCE: u64 = 1;
const POSIX_SHM_BYTES: u64 = 64 * 1024 * 1024;
const POSIX_SHM_INODES: usize = 4096;

#[derive(Clone, Copy)]
struct Limits {
    bytes: u64,
    inodes: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Key(u64, u64);

impl Key {
    pub(super) const fn new(device: u64, inode: u64) -> Self {
        Self(device, inode)
    }
}

struct Entry {
    object: u64,
    size: u64,
    opens: usize,
    seals: u8,
}

struct State {
    bytes: u64,
    next_object: u64,
    entries: BTreeMap<Key, Entry>,
}

pub(super) struct Budget {
    limits: Limits,
    state: Mutex<State>,
}

impl std::fmt::Debug for Budget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ShmBudget")
    }
}

pub(super) struct Lease {
    budget: Arc<Budget>,
    key: Key,
}

/// Capability-rooted native backing for one engine's POSIX shared-memory mount.
pub(super) struct PosixShm {
    source: MountSourceId,
    path: PathBuf,
    root: Arc<File>,
    budget: Arc<Budget>,
}

impl PosixShm {
    pub(super) fn create(root_path: &Path, root: &File) -> Result<Self, RuntimePathError> {
        let device = Self::directory(root.as_raw_fd(), c"dev", 0o755)?;
        let shared = Self::directory(device.as_raw_fd(), c"shm", 0o1777)?;
        // mkdirat observes umask. The mounted namespace contract is exactly 01777.
        // SAFETY: shared is a live owned directory descriptor and fchmod retains no state.
        if unsafe { libc::fchmod(shared.as_raw_fd(), 0o1777) } != 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        let source = MountSourceId::new(POSIX_SHM_SOURCE).map_err(|_| RuntimePathError::Invalid)?;
        Ok(Self {
            source,
            path: root_path.join("dev/shm"),
            root: Arc::new(shared),
            budget: Arc::new(Budget::new(Limits {
                bytes: POSIX_SHM_BYTES,
                inodes: POSIX_SHM_INODES,
            })),
        })
    }

    pub(super) const fn source(&self) -> MountSourceId {
        self.source
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn root(&self) -> Arc<File> {
        Arc::clone(&self.root)
    }

    pub(super) fn budget(&self) -> Arc<Budget> {
        Arc::clone(&self.budget)
    }

    fn directory(parent: RawFd, name: &CStr, mode: libc::mode_t) -> Result<File, RuntimePathError> {
        // SAFETY: parent and name remain live for mkdirat, which retains neither.
        let created = unsafe { libc::mkdirat(parent, name.as_ptr(), mode) };
        if created != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(HostError::map(error));
            }
        }
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: parent and name remain live for openat; success creates one
        // uniquely owned descriptor and no pointer is retained.
        let descriptor = unsafe { libc::openat(parent, name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful openat returned one unowned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

impl Budget {
    const fn new(limits: Limits) -> Self {
        Self {
            limits,
            state: Mutex::new(State {
                bytes: 0,
                next_object: 1,
                entries: BTreeMap::new(),
            }),
        }
    }

    #[cfg(test)]
    pub(super) fn testing(bytes: u64, inodes: usize) -> Arc<Self> {
        Arc::new(Self::new(Limits { bytes, inodes }))
    }

    pub(super) fn ordinary() -> Arc<Self> {
        Arc::new(Self::new(Limits {
            bytes: POSIX_SHM_BYTES,
            inodes: POSIX_SHM_INODES,
        }))
    }

    pub(super) fn open(self: &Arc<Self>, file: &File) -> Result<Lease, ObjectError> {
        let metadata = file.metadata().map_err(Self::object)?;
        let key = Key(metadata.dev(), metadata.ino());
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = state.entries.get_mut(&key) {
            let previous = entry.size;
            let opens = entry.opens.checked_add(1).ok_or(ObjectError::NoSpace)?;
            let total = state.bytes - previous;
            if !state.fits(previous, metadata.size(), self.limits.bytes) {
                return Err(ObjectError::NoSpace);
            }
            state.bytes = total + metadata.size();
            let entry = state.entries.get_mut(&key).ok_or(ObjectError::Io)?;
            entry.size = metadata.size();
            entry.opens = opens;
            return Ok(Lease {
                budget: Arc::clone(self),
                key,
            });
        }
        let size = metadata.size();
        if state.entries.len() == self.limits.inodes || !state.fits(0, size, self.limits.bytes) {
            return Err(ObjectError::NoSpace);
        }
        let object = state.next_object;
        state.next_object = state.next_object.checked_add(1).ok_or(ObjectError::NoSpace)?;
        state.bytes += size;
        state.entries.insert(
            key,
            Entry {
                object,
                size,
                opens: 1,
                seals: 1,
            },
        );
        Ok(Lease {
            budget: Arc::clone(self),
            key,
        })
    }

    pub(super) fn unlink(&self, key: Key, last_link: bool) {
        if !last_link {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let removable = state.entries.get(&key).is_some_and(|entry| entry.opens == 0);
        if removable {
            state.remove(key);
        }
    }

    pub(super) fn created(&self, key: Key, size: u64) -> Result<(), ObjectError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.entries.contains_key(&key) {
            return Ok(());
        }
        if state.entries.len() == self.limits.inodes || !state.fits(0, size, self.limits.bytes) {
            return Err(ObjectError::NoSpace);
        }
        let object = state.next_object;
        state.next_object = state.next_object.checked_add(1).ok_or(ObjectError::NoSpace)?;
        state.bytes += size;
        state.entries.insert(
            key,
            Entry {
                object,
                size,
                opens: 0,
                seals: 1,
            },
        );
        Ok(())
    }

    fn object(error: std::io::Error) -> ObjectError {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => ObjectError::PermissionDenied,
            std::io::ErrorKind::WouldBlock => ObjectError::WouldBlock,
            std::io::ErrorKind::Interrupted => ObjectError::Interrupted,
            _ => ObjectError::Io,
        }
    }
}

impl Lease {
    pub(super) fn seals(&self) -> Result<u8, ObjectError> {
        self.budget
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .get(&self.key)
            .map(|entry| entry.seals)
            .ok_or(ObjectError::Io)
    }

    pub(super) fn add_seals(&self, seals: u8) -> Result<u8, ObjectError> {
        let mut state = self.budget.state.lock().unwrap_or_else(|error| error.into_inner());
        let entry = state.entries.get_mut(&self.key).ok_or(ObjectError::Io)?;
        if entry.seals & 1 != 0 {
            return Err(ObjectError::PermissionDenied);
        }
        entry.seals |= seals;
        Ok(entry.seals)
    }

    pub(super) fn write(&self, file: &File, input: &[u8]) -> Result<usize, ObjectError> {
        let mut cursor = file;
        let offset = cursor.stream_position().map_err(Budget::object)?;
        self.mutate(file, offset.saturating_add(input.len() as u64), |file| {
            let mut writer = file;
            writer.write(input).map_err(Budget::object)
        })
    }

    pub(super) fn write_at(&self, file: &File, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.mutate(file, offset.saturating_add(input.len() as u64), |file| {
            file.write_at(input, offset).map_err(Budget::object)
        })
    }

    pub(super) fn truncate(&self, file: &File, size: u64) -> Result<(), ObjectError> {
        self.mutate(file, size, |file| file.set_len(size).map_err(Budget::object))
    }

    /// Holds the tmpfs extent reservation across one external native write.
    /// The terminal error is kept distinct from quota/accounting failures.
    pub(super) fn external<T, E>(
        &self,
        file: &File,
        target: u64,
        operation: impl FnOnce(&File) -> Result<T, E>,
    ) -> Result<Result<T, E>, ObjectError> {
        let mut state = self.budget.state.lock().unwrap_or_else(|error| error.into_inner());
        let current = state.entries.get(&self.key).ok_or(ObjectError::Io)?.size;
        let projected = current.max(target);
        let total = state.bytes - current;
        if total
            .checked_add(projected)
            .is_none_or(|bytes| bytes > self.budget.limits.bytes)
        {
            return Err(ObjectError::NoSpace);
        }
        let result = operation(file);
        let actual = file.metadata().map_err(Budget::object)?.size();
        state.bytes = total.checked_add(actual).ok_or(ObjectError::NoSpace)?;
        state.entries.get_mut(&self.key).ok_or(ObjectError::Io)?.size = actual;
        Ok(result)
    }

    fn mutate<T>(
        &self,
        file: &File,
        target: u64,
        operation: impl FnOnce(&File) -> Result<T, ObjectError>,
    ) -> Result<T, ObjectError> {
        let mut state = self.budget.state.lock().unwrap_or_else(|error| error.into_inner());
        let entry = state.entries.get(&self.key).ok_or(ObjectError::Io)?;
        let current = entry.size;
        if entry.seals & (8 | 16) != 0 {
            return Err(ObjectError::PermissionDenied);
        }
        if target < current && entry.seals & 2 != 0 || target > current && entry.seals & 4 != 0 {
            return Err(ObjectError::PermissionDenied);
        }
        let projected = current.max(target);
        let total = state.bytes - current;
        if total
            .checked_add(projected)
            .is_none_or(|bytes| bytes > self.budget.limits.bytes)
        {
            return Err(ObjectError::NoSpace);
        }
        let result = operation(file);
        let actual = file.metadata().map_err(Budget::object)?.size();
        state.bytes = total.checked_add(actual).ok_or(ObjectError::NoSpace)?;
        state.entries.get_mut(&self.key).ok_or(ObjectError::Io)?.size = actual;
        result
    }

    pub(super) fn close(self, links: u64) {
        let mut state = self.budget.state.lock().unwrap_or_else(|error| error.into_inner());
        let removable = if let Some(entry) = state.entries.get_mut(&self.key) {
            entry.opens = entry.opens.saturating_sub(1);
            entry.opens == 0 && links == 0
        } else {
            false
        };
        if removable {
            state.remove(self.key);
        }
    }
}

impl State {
    fn fits(&self, previous: u64, size: u64, limit: u64) -> bool {
        self.bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(size))
            .is_some_and(|bytes| bytes <= limit)
    }

    fn remove(&mut self, key: Key) {
        if let Some(entry) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(entry.size);
            let _ = entry.object;
        }
    }
}
