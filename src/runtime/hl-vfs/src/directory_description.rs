use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    DirectoryBatch, DirectoryBatchToken, ObjectError, ObjectKind, OfdDirectoryEntry, OpenFileDescription, Readiness,
    ReadinessObserver, ReadinessSubscription, StatusFlags,
};

use crate::{GuestPathBytes, Kind, OverlayNodeKind, VfsFileToken};

const ENTRY_MAXIMUM: usize = 4096;
const NAME_MAXIMUM: usize = 255;

/// Value-only directory entry. Guest dirent encoding belongs to hl-linux.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VfsDirectoryEntry {
    pub inode: u64,
    pub kind: Kind,
    pub name: Vec<u8>,
}

impl VfsDirectoryEntry {
    pub fn new(inode: u64, kind: Kind, name: impl AsRef<[u8]>) -> Result<Self, ObjectError> {
        let name = name.as_ref().to_vec();
        if name.is_empty() || name.len() > NAME_MAXIMUM || name.contains(&b'/') || name.contains(&0) {
            return Err(ObjectError::InvalidArgument);
        }
        Ok(Self { inode, kind, name })
    }

    pub fn from_overlay(inode: u64, kind: OverlayNodeKind, name: impl AsRef<[u8]>) -> Result<Self, ObjectError> {
        let kind = match kind {
            OverlayNodeKind::Directory => Kind::Directory,
            OverlayNodeKind::Regular => Kind::Regular,
            OverlayNodeKind::Symlink => Kind::Symlink,
            OverlayNodeKind::Other => Kind::Socket,
        };
        Self::new(inode, kind, name)
    }
}

/// Host operations needed for a directory open description.
pub trait VfsDirectoryHost: Send + Sync + 'static {
    /// Returns one ordered snapshot. Overlay adapters supply the result of the
    /// existing precedence/whiteout merge, rather than individual layers.
    fn snapshot(&self, directory: VfsFileToken) -> Result<Vec<VfsDirectoryEntry>, ObjectError>;

    fn readiness(&self, directory: VfsFileToken, interests: Readiness) -> Readiness;

    fn subscribe(
        &self,
        directory: VfsFileToken,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError>;

    fn cancel(&self, directory: VfsFileToken);

    fn close(&self, directory: VfsFileToken);
}

struct DirectoryState {
    generation: u64,
    status: StatusFlags,
    position: usize,
    snapshot: Option<Arc<[VfsDirectoryEntry]>>,
}

/// Directory object with an OFD-owned cookie and bounded stable snapshot.
pub struct VfsDirectoryDescription<H: VfsDirectoryHost> {
    host: H,
    token: VfsFileToken,
    path: GuestPathBytes,
    state: Mutex<DirectoryState>,
    retired: AtomicBool,
    closed: AtomicBool,
}

impl<H: VfsDirectoryHost> fmt::Debug for VfsDirectoryDescription<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VfsDirectoryDescription")
            .field("token", &self.token)
            .field("path", &self.path)
            .field("cookie", &self.cookie())
            .finish_non_exhaustive()
    }
}

impl<H: VfsDirectoryHost> VfsDirectoryDescription<H> {
    #[must_use]
    pub const fn new(host: H, token: VfsFileToken, path: GuestPathBytes, status: StatusFlags) -> Self {
        Self {
            host,
            token,
            path,
            state: Mutex::new(DirectoryState {
                generation: 1,
                status,
                position: 0,
                snapshot: None,
            }),
            retired: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub fn path(&self) -> &GuestPathBytes {
        &self.path
    }

    #[must_use]
    pub fn cookie(&self) -> u64 {
        self.lock_state().position as u64
    }

    /// Returns whole entries only and advances the shared OFD cookie.
    pub fn read_entries(&self, maximum: usize) -> Result<Vec<VfsDirectoryEntry>, ObjectError> {
        self.ensure_live()?;
        let mut state = self.lock_state();
        self.ensure_snapshot(&mut state)?;
        let snapshot = state.snapshot.as_ref().expect("snapshot established").clone();
        if maximum == 0 && state.position < snapshot.len() {
            return Err(ObjectError::InvalidArgument);
        }
        let end = state.position.saturating_add(maximum).min(snapshot.len());
        let output = snapshot[state.position..end].to_vec();
        state.position = end;
        Ok(output)
    }

    /// Valid cookies select that entry; zero or out-of-range rewinds.
    pub fn seek_cookie(&self, cookie: i64) -> Result<u64, ObjectError> {
        self.ensure_live()?;
        let mut state = self.lock_state();
        self.ensure_snapshot(&mut state)?;
        let length = state.snapshot.as_ref().expect("snapshot established").len();
        state.position = usize::try_from(cookie)
            .ok()
            .filter(|position| *position <= length)
            .unwrap_or(0);
        Ok(state.position as u64)
    }

    /// Drops the stable view so the next read observes later mutations.
    pub fn refresh(&self) -> Result<(), ObjectError> {
        self.ensure_live()?;
        let mut state = self.lock_state();
        state.snapshot = None;
        state.position = 0;
        state.generation = state.generation.wrapping_add(1).max(1);
        Ok(())
    }

    fn ensure_snapshot(&self, state: &mut DirectoryState) -> Result<(), ObjectError> {
        if state.snapshot.is_some() {
            return Ok(());
        }
        let entries = self.host.snapshot(self.token)?;
        if entries.len() > ENTRY_MAXIMUM {
            return Err(ObjectError::ResourceLimit);
        }
        for entry in &entries {
            VfsDirectoryEntry::new(entry.inode, entry.kind, entry.name.clone())?;
        }
        state.snapshot = Some(entries.into());
        Ok(())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, DirectoryState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn ensure_live(&self) -> Result<(), ObjectError> {
        if self.retired.load(Ordering::Acquire) {
            Err(ObjectError::Retired)
        } else {
            Ok(())
        }
    }
}

impl<H: VfsDirectoryHost> OpenFileDescription for VfsDirectoryDescription<H> {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Directory
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.ensure_live()?;
        self.lock_state().status = flags;
        Ok(())
    }

    fn read_directory(&self, maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        self.ensure_live()?;
        let mut state = self.lock_state();
        self.ensure_snapshot(&mut state)?;
        let start = state.position;
        let snapshot = state.snapshot.as_ref().expect("snapshot established");
        let entries = snapshot
            .iter()
            .skip(start)
            .take(maximum)
            .cloned()
            .enumerate()
            .map(|(index, entry)| OfdDirectoryEntry {
                inode: entry.inode,
                cookie: (start + index + 1) as i64,
                file_type: match entry.kind {
                    Kind::Fifo => 1,
                    Kind::Character => 2,
                    Kind::Directory => 4,
                    Kind::Block => 6,
                    Kind::Regular => 8,
                    Kind::Symlink => 10,
                    Kind::Socket => 12,
                },
                name: entry.name,
            })
            .collect();
        Ok(DirectoryBatch {
            token: DirectoryBatchToken {
                generation: state.generation,
                cookie: start as i64,
            },
            entries,
        })
    }

    fn commit_directory(&self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        self.ensure_live()?;
        let mut state = self.lock_state();
        if token.generation != state.generation || token.cookie < 0 || state.position != token.cookie as usize {
            return Err(ObjectError::InvalidArgument);
        }
        let length = state.snapshot.as_ref().map_or(0, |entries| entries.len());
        state.position = state
            .position
            .checked_add(count)
            .filter(|position| *position <= length)
            .ok_or(ObjectError::InvalidArgument)?;
        Ok(())
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        if self.retired.load(Ordering::Acquire) {
            return Readiness::from_bits(Readiness::ERROR | Readiness::HANGUP);
        }
        self.host.readiness(self.token, interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.ensure_live()?;
        self.host.subscribe(self.token, observer)
    }

    fn retire(&self) {
        if !self.retired.swap(true, Ordering::AcqRel) {
            self.host.cancel(self.token);
        }
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.host.close(self.token);
        }
    }
}
