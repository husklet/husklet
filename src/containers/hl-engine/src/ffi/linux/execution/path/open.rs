use std::collections::BTreeMap;
use std::ffi::CString;
use std::fmt;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptionIdentity, OpenFileDescription};
use hl_runtime::{GuestPath, OpenIntent, PreparedPathOpen, RuntimePathError};

use super::{FileTransferRegistry, HostError, NativeFile, directory, filesystem, lease, pin};

pub(super) struct PendingOpen {
    object: Arc<NativeFile>,
    file: Arc<NativeFile>,
    path: PathBuf,
    intent: OpenIntent,
    mode: u32,
    root: PathBuf,
    guest: Option<GuestPath>,
    paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
    terminals: Arc<hl_runtime::TerminalCatalog>,
    parent: OwnedFd,
    name: CString,
    transfers: Arc<FileTransferRegistry>,
}

impl PendingOpen {
    pub(super) fn new(
        file: Arc<NativeFile>,
        path: PathBuf,
        intent: OpenIntent,
        mode: u32,
        root: PathBuf,
        paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
        terminals: Arc<hl_runtime::TerminalCatalog>,
        parent: OwnedFd,
        name: CString,
        transfers: Arc<FileTransferRegistry>,
    ) -> Self {
        Self {
            object: Arc::clone(&file),
            file,
            path,
            intent,
            mode,
            root,
            guest: None,
            paths,
            terminals,
            parent,
            name,
            transfers,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn projected(
        file: Arc<NativeFile>,
        path: PathBuf,
        guest: GuestPath,
        intent: OpenIntent,
        mode: u32,
        paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
        terminals: Arc<hl_runtime::TerminalCatalog>,
        parent: OwnedFd,
        name: CString,
        transfers: Arc<FileTransferRegistry>,
    ) -> Self {
        Self {
            object: Arc::clone(&file),
            file,
            path,
            intent,
            mode,
            root: PathBuf::new(),
            guest: Some(guest),
            paths,
            terminals,
            parent,
            name,
            transfers,
        }
    }

    fn exists(&self) -> Result<bool, RuntimePathError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the owned parent and terminated name remain live, status is
        // writable, and fstatat retains no pointer or descriptor.
        let result = unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(false)
        } else {
            Err(HostError::map(error))
        }
    }

    fn discard(&self) {
        // SAFETY: this removes only the final name created by this pending
        // open after quota admission failed; arguments are retained nowhere.
        unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) };
    }

    fn discard_created(&self, created: bool, temporary: bool) {
        if created && !temporary {
            self.discard();
        }
    }

    fn admit_quota(&self, opened: &std::fs::File, created: bool, temporary: bool) -> Result<(), RuntimePathError> {
        let Some(budget) = self.file.shm_budget.as_ref() else {
            return Ok(());
        };
        let lease = match budget.open(opened) {
            Ok(lease) => lease,
            Err(hl_descriptor::ObjectError::NoSpace) => {
                self.discard_created(created, temporary);
                return Err(RuntimePathError::NoSpace);
            }
            Err(_) => return Err(RuntimePathError::Io),
        };
        *self.file.shm_lease.lock().unwrap_or_else(|error| error.into_inner()) = Some(lease);
        Ok(())
    }
}

impl fmt::Debug for PendingOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PendingOpen")
    }
}

impl PreparedPathOpen for PendingOpen {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        self.object.clone()
    }
    fn bind(&mut self, identity: DescriptionIdentity) -> Result<(), RuntimePathError> {
        self.transfers.bind(identity, self.file.clone())
    }
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        let bits = self.intent.bits();
        let temporary = bits & OpenIntent::TEMPORARY != 0;
        let created = temporary || bits & OpenIntent::CREATE != 0 && !self.exists()?;
        let opened = pin::Host::open(&self.parent, &self.name, self.intent, self.mode)?;
        let metadata = opened.metadata().map_err(HostError::map)?;
        if metadata.file_type().is_fifo() {
            // The final name changed after the prepare-time kind check. The
            // nonblocking host open proves this without sleeping; retry lets
            // scheduler admission route the now-observed FIFO to a waiter.
            return Err(RuntimePathError::WouldBlock);
        }
        self.admit_quota(&opened, created, temporary)?;
        self.file
            .path_only
            .store(bits & OpenIntent::PATH_ONLY != 0, Ordering::Release);
        self.file
            .anonymous_exclusive
            .store(temporary && bits & OpenIntent::EXCLUSIVE != 0, Ordering::Release);
        let retained_link =
            if bits & (OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW) == OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW {
                std::fs::read_link(&self.path)
                    .ok()
                    .map(|target| target.as_os_str().as_encoded_bytes().to_vec())
            } else {
                None
            };
        self.file.retain_link(retained_link.clone());
        if bits & OpenIntent::WRITE != 0 && bits & OpenIntent::PATH_ONLY == 0 {
            let identity = (metadata.dev(), metadata.ino());
            let writes = Arc::clone(&self.file.writes);
            *self.file.write_lease.lock().unwrap_or_else(|error| error.into_inner()) =
                Some(lease::WriteLease::acquire(identity, writes));
        }
        if bits & OpenIntent::TEMPORARY != 0 && bits & OpenIntent::PATH_ONLY == 0 {
            *self.file.file.lock().unwrap_or_else(|error| error.into_inner()) = Some(opened);
            return Ok(());
        }
        let guest = if let Some(guest) = &self.guest {
            guest.clone()
        } else {
            let relative = self
                .path
                .strip_prefix(&self.root)
                .map_err(|_| RuntimePathError::Access)?;
            let text = relative.to_str().ok_or(RuntimePathError::Invalid)?;
            GuestPath::new(&format!("/{text}")).map_err(|_| RuntimePathError::Invalid)?
        };
        let filesystem_path = if retained_link.is_some() {
            self.path.parent().unwrap_or(&self.path)
        } else {
            &self.path
        };
        let filesystem = filesystem::HostFilesystem::read(filesystem_path, &guest)?;
        let identity = (metadata.dev(), metadata.ino());
        let opened_path = lease::LeaseEntry {
            guest: guest.clone(),
            filesystem,
            file: Arc::downgrade(&self.file),
        };
        let mut paths = self.paths.lock().unwrap_or_else(|error| error.into_inner());
        match paths.entry(identity) {
            std::collections::btree_map::Entry::Occupied(mut entry) if entry.get().file.upgrade().is_none() => {
                entry.insert(opened_path);
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(opened_path);
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
        drop(paths);
        if metadata.is_dir() {
            let mut directory = directory::State::new(self.path.clone());
            if guest.as_str() == "/dev/pts" {
                directory = directory.with_terminals(Arc::clone(&self.terminals));
            }
            *self.file.directory.lock().unwrap_or_else(|error| error.into_inner()) = Some(directory);
        }
        *self.file.file.lock().unwrap_or_else(|error| error.into_inner()) = Some(opened);
        if created
            && !temporary
            && let (Some(parent), Some(name)) = (self.path.parent(), self.path.file_name())
        {
            self.file
                .watches
                .publish_child(parent, name.as_encoded_bytes(), hl_event::InotifyMask::CREATE);
        }
        Ok(())
    }
    fn rollback(self: Box<Self>) {}
}
