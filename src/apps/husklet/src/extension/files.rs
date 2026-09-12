//! Files beneath one workspace's storage directory.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use hl_extension::RelativePath;
use hl_extension::port::{
    DirectoryPage, Entry, FileChange, FileChangeKind, FileChangePage, FileInventory, FileRange, HostError,
    WorkspaceFiles,
};
use notify::{RecursiveMode, Watcher as _};

/// The workspace file port, rooted at one directory.
///
/// [`RelativePath`] already refuses traversal and absolute paths when it is
/// constructed, and `Authority::permit_path` confines a call to the declared
/// roots. Neither check can see a symbolic link, so every path this adapter is
/// about to touch is resolved on the real filesystem and re-checked against the
/// canonical root. Syntax is not containment.
pub struct WorkspaceDirectory {
    root: PathBuf,
    root_device: u64,
    root_inode: u64,
    mutations: Mutex<()>,
    journals: Mutex<BTreeMap<Vec<hl_extension::FilesystemSelector>, FileJournal>>,
    watcher: Mutex<WatcherState>,
}

struct WatcherState {
    _watcher: notify::RecommendedWatcher,
    events: Receiver<notify::Result<notify::Event>>,
    overflow: Arc<AtomicBool>,
}

struct FileJournal {
    identity: String,
    revision: u64,
    oldest: u64,
    entries: BTreeMap<RelativePath, Entry>,
    changes: VecDeque<FileChange>,
    initialized: bool,
    scan_truncated: bool,
    event_overflow: bool,
    pending_invalidations: std::collections::BTreeSet<RelativePath>,
}

impl Default for FileJournal {
    fn default() -> Self {
        Self {
            identity: uuid::Uuid::new_v4().simple().to_string(),
            revision: 0,
            oldest: 0,
            entries: BTreeMap::new(),
            changes: VecDeque::new(),
            initialized: false,
            scan_truncated: false,
            event_overflow: false,
            pending_invalidations: std::collections::BTreeSet::new(),
        }
    }
}

const PATH_DEPTH_LIMIT: usize = 128;
const READ_BYTES_LIMIT: usize = (1 << 20) - (8 << 10);
const LIST_ENTRIES_LIMIT: usize = 4096;
const LIST_PATH_BYTES_LIMIT: usize = 512 << 10;
const JOURNAL_ENTRIES_LIMIT: usize = 16_384;
const JOURNAL_HISTORY_LIMIT: usize = 4_096;

impl WorkspaceDirectory {
    /// Roots the port at `root`, creating it if it does not exist.
    ///
    /// The root is canonicalized once here so that later containment checks
    /// compare two resolved paths; a root reached through a symbolic link would
    /// otherwise make every legitimate path look like an escape.
    ///
    /// # Errors
    /// Returns the failure to create or resolve the root directory.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let metadata = std::fs::metadata(&root)?;
        let (sender, events) = mpsc::sync_channel(JOURNAL_HISTORY_LIMIT);
        let overflow = Arc::new(AtomicBool::new(false));
        let callback_overflow = Arc::clone(&overflow);
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if matches!(
                event.as_ref().map(|event| &event.kind),
                Ok(notify::EventKind::Access(_))
            ) {
                return;
            }
            if sender.try_send(event).is_err() {
                callback_overflow.store(true, Ordering::Release);
            }
        })
        .map_err(io::Error::other)?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(io::Error::other)?;
        Ok(Self {
            root,
            root_device: metadata.dev(),
            root_inode: metadata.ino(),
            mutations: Mutex::new(()),
            journals: Mutex::new(BTreeMap::new()),
            watcher: Mutex::new(WatcherState {
                _watcher: watcher,
                events,
                overflow,
            }),
        })
    }

    /// The directory this port is confined to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn pinned(&self, path: &RelativePath) -> Result<PinnedEntry, HostError> {
        let parts = path.parts();
        if parts.len() > PATH_DEPTH_LIMIT {
            return Err(HostError::Conflict(format!(
                "{path} exceeds the {PATH_DEPTH_LIMIT}-component limit"
            )));
        }
        let Some((name, parents)) = parts.split_last() else {
            return Err(HostError::Conflict("the workspace root cannot be mutated".to_owned()));
        };
        let mut directory = self.root_directory().map_err(|error| absence(path, &error))?;
        for part in parents {
            directory = open_at(
                &directory,
                part,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            )
            .map_err(|error| absence(path, &error))?;
        }
        let metadata = directory.metadata().map_err(|error| absence(path, &error))?;
        Ok(PinnedEntry {
            directory,
            metadata,
            name: c_name(Path::new(name)).map_err(|error| absence(path, &error))?,
        })
    }

    fn pinned_publication(&self, path: &RelativePath) -> Result<PinnedEntry, HostError> {
        let entry = self.pinned(path)?;
        if symlink_at(&entry.directory, &entry.name).map_err(|error| absence(path, &error))? {
            return Err(HostError::Conflict(format!("{path} is a symbolic link")));
        }
        Ok(entry)
    }

    fn root_directory(&self) -> io::Result<File> {
        let descriptor = rustix::fs::openat(
            rustix::fs::CWD,
            &self.root,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        let directory = File::from(descriptor);
        let metadata = directory.metadata()?;
        if (metadata.dev(), metadata.ino()) != (self.root_device, self.root_inode) {
            return Err(io::Error::other(
                "workspace root changed after authority was established",
            ));
        }
        Ok(directory)
    }

    fn directory(&self, path: &RelativePath) -> Result<File, HostError> {
        let parts = path.parts();
        if parts.len() > PATH_DEPTH_LIMIT {
            return Err(HostError::Conflict(format!(
                "{path} exceeds the {PATH_DEPTH_LIMIT}-component limit"
            )));
        }
        let mut directory = self.root_directory().map_err(|error| absence(path, &error))?;
        for part in parts {
            directory = open_at(
                &directory,
                part,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            )
            .map_err(|error| absence(path, &error))?;
        }
        Ok(directory)
    }

    fn opened(&self, path: &RelativePath) -> Result<File, HostError> {
        let parts = path.parts();
        if parts.len() > PATH_DEPTH_LIMIT {
            return Err(HostError::Conflict(format!(
                "{path} exceeds the {PATH_DEPTH_LIMIT}-component limit"
            )));
        }
        let Some((name, parents)) = parts.split_last() else {
            return Err(HostError::Conflict("the workspace root is not a file".into()));
        };
        let mut directory = self.root_directory().map_err(|error| absence(path, &error))?;
        for part in parents {
            directory = open_at(
                &directory,
                part,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            )
            .map_err(|error| absence(path, &error))?;
        }
        open_at(&directory, name, rustix::fs::OFlags::RDONLY).map_err(|error| absence(path, &error))
    }

    fn create_observed_with(
        &self,
        path: &RelativePath,
        contents: &[u8],
        before_publish: impl FnOnce() -> io::Result<()>,
    ) -> Result<String, HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let entry = self.pinned_publication(path)?;
        before_publish().map_err(|error| absence(path, &error))?;
        let current = self.pinned_publication(path)?;
        if (entry.metadata.dev(), entry.metadata.ino()) != (current.metadata.dev(), current.metadata.ino()) {
            return Err(HostError::Conflict(format!("{path} parent changed before publication")));
        }
        atomic_create(&entry, contents).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                HostError::Conflict(format!("{path} no longer matches the observed identity"))
            } else {
                absence(path, &error)
            }
        })
    }

    fn mkdir_with(
        &self,
        path: &RelativePath,
        before_create: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let entry = self.pinned(path)?;
        before_create().map_err(|error| absence(path, &error))?;
        rustix::fs::mkdirat(&entry.directory, &entry.name, rustix::fs::Mode::from_raw_mode(0o777))
            .map_err(io::Error::from)
            .and_then(|()| entry.directory.sync_all())
            .map_err(|error| absence(path, &error))
    }

    fn scan(
        &self,
        roots: &[hl_extension::FilesystemSelector],
    ) -> Result<(BTreeMap<RelativePath, Entry>, bool), HostError> {
        let mut pending = roots
            .iter()
            .map(|selector| match selector {
                hl_extension::FilesystemSelector::Exact { exact } => (exact.clone(), false),
                hl_extension::FilesystemSelector::Subtree { subtree } => (subtree.clone(), true),
            })
            .collect::<Vec<_>>();
        pending.reverse();
        let mut entries = BTreeMap::new();
        while let Some((path, descend)) = pending.pop() {
            if path.parts().is_empty() {
                if descend {
                    for child in self.list(&path)?.into_iter().rev() {
                        pending.push((child.path, true));
                    }
                }
                continue;
            }
            let entry = match self.stat(&path) {
                Ok(entry) => entry,
                Err(HostError::Absent(_)) => continue,
                Err(error) => return Err(error),
            };
            let directory = entry.directory;
            entries.insert(path.clone(), entry);
            if entries.len() >= JOURNAL_ENTRIES_LIMIT {
                return Ok((entries, true));
            }
            if directory && descend {
                let children = self.list(&path)?;
                for child in children.into_iter().rev() {
                    pending.push((child.path, true));
                }
            }
        }
        Ok((entries, false))
    }

    fn drain_watcher(&self) -> Result<(std::collections::BTreeSet<RelativePath>, bool), HostError> {
        let watcher = self
            .watcher
            .lock()
            .map_err(|_| HostError::Failed("filesystem watcher lock is poisoned".into()))?;
        let mut paths = std::collections::BTreeSet::new();
        let mut overflow = watcher.overflow.swap(false, Ordering::AcqRel);
        while let Ok(event) = watcher.events.try_recv() {
            match event {
                Ok(event) => {
                    for path in event.paths {
                        let Ok(relative) = path.strip_prefix(&self.root) else {
                            overflow = true;
                            continue;
                        };
                        let Some(relative) = relative.to_str() else {
                            overflow = true;
                            continue;
                        };
                        if relative.is_empty() {
                            continue;
                        }
                        match RelativePath::new(relative) {
                            Ok(path) => {
                                paths.insert(path);
                            }
                            Err(_) => overflow = true,
                        }
                    }
                }
                Err(_) => overflow = true,
            }
        }
        Ok((paths, overflow))
    }

    fn refresh_journal(&self, roots: &[hl_extension::FilesystemSelector]) -> Result<(), HostError> {
        let (invalidated, watcher_overflow) = self.drain_watcher()?;
        let (next, scan_truncated) = self.scan(roots)?;
        let mut journals = self
            .journals
            .lock()
            .map_err(|_| HostError::Failed("filesystem journal lock is poisoned".into()))?;
        let mut key = roots.to_vec();
        key.sort();
        key.dedup();
        for (selectors, journal) in journals.iter_mut() {
            journal.event_overflow |= watcher_overflow;
            journal.pending_invalidations.extend(
                invalidated
                    .iter()
                    .filter(|path| selectors.iter().any(|selector| selector.permits(path)))
                    .cloned(),
            );
        }
        let journal = journals.entry(key).or_default();
        if !journal.initialized {
            journal.entries = next;
            journal.initialized = true;
            journal.scan_truncated = scan_truncated;
            journal.event_overflow |= watcher_overflow;
            journal.pending_invalidations.clear();
            return Ok(());
        }
        let mut changes = BTreeMap::new();
        for (path, entry) in &next {
            match journal.entries.get(path) {
                None => {
                    changes.insert(path.clone(), (FileChangeKind::Create, Some(entry.clone())));
                }
                Some(before) if before != entry => {
                    changes.insert(path.clone(), (FileChangeKind::Modify, Some(entry.clone())));
                }
                Some(_) => {}
            }
        }
        for path in journal.entries.keys() {
            if !next.contains_key(path) {
                changes.insert(path.clone(), (FileChangeKind::Remove, None));
            }
        }
        journal.pending_invalidations.extend(
            invalidated
                .into_iter()
                .filter(|path| roots.iter().any(|selector| selector.permits(path))),
        );
        for path in std::mem::take(&mut journal.pending_invalidations) {
            changes.entry(path).or_insert((FileChangeKind::Invalidate, None));
        }
        for (path, (kind, entry)) in changes {
            journal.revision = journal.revision.saturating_add(1);
            let revision = journal.revision;
            journal.changes.push_back(FileChange {
                revision,
                kind,
                path,
                entry,
            });
            if journal.changes.len() > JOURNAL_HISTORY_LIMIT {
                if let Some(discarded) = journal.changes.pop_front() {
                    journal.oldest = discarded.revision;
                }
            }
        }
        journal.entries = next;
        journal.scan_truncated = scan_truncated;
        journal.event_overflow |= watcher_overflow;
        Ok(())
    }
}

impl WorkspaceFiles for WorkspaceDirectory {
    fn inventory(&self, roots: &[hl_extension::FilesystemSelector]) -> Result<FileInventory, HostError> {
        self.refresh_journal(roots)?;
        let (journal, revision) = {
            let journals = self
                .journals
                .lock()
                .map_err(|_| HostError::Failed("filesystem journal lock is poisoned".into()))?;
            let mut key = roots.to_vec();
            key.sort();
            key.dedup();
            let journal = journals.get(&key).expect("refresh creates the scoped journal");
            (journal.identity.clone(), journal.revision)
        };
        const LIMIT: usize = 256;
        const PATH_BYTES_LIMIT: usize = 256 * 1024;
        let mut pending = roots
            .iter()
            .map(|selector| match selector {
                hl_extension::FilesystemSelector::Exact { exact } => (exact.clone(), false),
                hl_extension::FilesystemSelector::Subtree { subtree } => (subtree.clone(), true),
            })
            .collect::<Vec<_>>();
        pending.reverse();
        let mut entries = std::collections::BTreeMap::new();
        let mut path_bytes = 0usize;
        let mut complete = true;
        while let Some((path, descend)) = pending.pop() {
            let directory = if path.parts().is_empty() {
                true
            } else {
                let entry = match self.stat(&path) {
                    Ok(entry) => entry,
                    Err(HostError::Absent(_)) => continue,
                    Err(error) => return Err(error),
                };
                let directory = entry.directory;
                let key = path.to_string();
                if !entries.contains_key(&key) {
                    if entries.len() == LIMIT || path_bytes.saturating_add(key.len()) > PATH_BYTES_LIMIT {
                        complete = false;
                        break;
                    }
                    path_bytes += key.len();
                    entries.insert(key, entry);
                }
                directory
            };
            if !directory || !descend {
                continue;
            }
            let mut children = self.list(&path)?;
            children.sort_by(|left, right| left.path.to_string().cmp(&right.path.to_string()));
            for child in children.into_iter().rev() {
                pending.push((child.path, true));
            }
        }
        let inventory = FileInventory {
            entries: entries.into_values().collect(),
            complete,
            coalesced: 0,
            journal,
            revision,
        };
        if inventory.complete {
            let mut journals = self
                .journals
                .lock()
                .map_err(|_| HostError::Failed("filesystem journal lock is poisoned".into()))?;
            let mut key = roots.to_vec();
            key.sort();
            key.dedup();
            if let Some(journal) = journals.get_mut(&key) {
                journal.event_overflow = false;
            }
        }
        Ok(inventory)
    }

    fn changes_since(
        &self,
        roots: &[hl_extension::FilesystemSelector],
        observed: &str,
        after: u64,
        limit: usize,
    ) -> Result<FileChangePage, HostError> {
        self.refresh_journal(roots)?;
        let journals = self
            .journals
            .lock()
            .map_err(|_| HostError::Failed("filesystem journal lock is poisoned".into()))?;
        let mut key = roots.to_vec();
        key.sort();
        key.dedup();
        let journal = journals.get(&key).expect("refresh creates the scoped journal");
        let truncated = observed != journal.identity
            || journal.scan_truncated
            || journal.event_overflow
            || after < journal.oldest
            || after > journal.revision;
        let changes = if truncated {
            Vec::new()
        } else {
            journal
                .changes
                .iter()
                .filter(|change| change.revision > after)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
        };
        let next = changes.last().map_or(journal.revision, |change| change.revision);
        let more = journal.changes.iter().any(|change| change.revision > next);
        let next = if more { next } else { journal.revision };
        Ok(FileChangePage {
            journal: journal.identity.clone(),
            more: !truncated && more,
            changes,
            next: if truncated { journal.revision } else { next },
            current: journal.revision,
            truncated,
        })
    }

    /// # Errors
    /// Returns `HostError::Absent` for a missing directory, `HostError::Conflict`
    /// for a path that resolves outside the root, and a failure otherwise.
    fn list(&self, path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        let directory = self.directory(path)?;
        let reading = rustix::fs::Dir::read_from(&directory)
            .map_err(io::Error::from)
            .map_err(|error| absence(path, &error))?;
        let mut entries = Vec::new();
        let mut path_bytes = 0usize;
        for entry in reading {
            let entry = entry.map_err(io::Error::from).map_err(|error| absence(path, &error))?;
            if matches!(entry.file_name().to_bytes(), b"." | b"..") {
                continue;
            }
            if entries.len() == LIST_ENTRIES_LIMIT {
                return Err(HostError::Failed(format!(
                    "{path}: directory exceeds the {LIST_ENTRIES_LIMIT}-entry limit"
                )));
            }
            let described = described_at(path, &directory, &entry)?;
            path_bytes = path_bytes.saturating_add(described.path.as_str().len());
            if path_bytes > LIST_PATH_BYTES_LIMIT {
                return Err(HostError::Failed(format!(
                    "{path}: directory paths exceed the {LIST_PATH_BYTES_LIMIT}-byte limit"
                )));
            }
            entries.push(described);
        }
        entries.sort_by(|first, second| first.path.cmp(&second.path));
        Ok(entries)
    }

    fn list_page(
        &self,
        path: &RelativePath,
        after: Option<&RelativePath>,
        observed: Option<&str>,
        limit: usize,
    ) -> Result<DirectoryPage, HostError> {
        let directory = self.directory(path)?;
        let identity = file_identity(&directory.metadata().map_err(|error| absence(path, &error))?);
        if observed.is_some_and(|value| value != identity) {
            return Err(HostError::Conflict(format!("{path}: directory changed between pages")));
        }
        let reading = rustix::fs::Dir::read_from(&directory)
            .map_err(io::Error::from)
            .map_err(|error| absence(path, &error))?;
        let mut entries = Vec::with_capacity(limit + 1);
        for entry in reading {
            let entry = entry.map_err(io::Error::from).map_err(|error| absence(path, &error))?;
            if matches!(entry.file_name().to_bytes(), b"." | b"..") {
                continue;
            }
            let described = described_at(path, &directory, &entry)?;
            if after.is_some_and(|cursor| described.path <= *cursor) {
                continue;
            }
            let at = entries.partition_point(|candidate: &Entry| candidate.path < described.path);
            entries.insert(at, described);
            if entries.len() > limit + 1 {
                entries.pop();
            }
        }
        let mut more = entries.len() > limit;
        entries.truncate(limit);
        let mut bytes = 0usize;
        let fits = entries
            .iter()
            .take_while(|entry| {
                bytes = bytes.saturating_add(entry.path.as_str().len());
                bytes <= LIST_PATH_BYTES_LIMIT
            })
            .count();
        if fits == 0 && !entries.is_empty() {
            return Err(HostError::Failed(format!(
                "{path}: first directory entry exceeds the page byte limit"
            )));
        }
        more |= fits < entries.len();
        entries.truncate(fits);
        let next = entries.last().map(|entry| entry.path.clone());
        let finished = file_identity(&directory.metadata().map_err(|error| absence(path, &error))?);
        if finished != identity {
            return Err(HostError::Conflict(format!("{path}: directory changed while listing")));
        }
        Ok(DirectoryPage {
            entries,
            identity,
            next,
            more,
        })
    }

    /// # Errors
    /// Returns `HostError::Absent` for a missing file, `HostError::Conflict` for
    /// a path that resolves outside the root, and a failure otherwise.
    fn read(&self, path: &RelativePath) -> Result<Vec<u8>, HostError> {
        let file = self.opened(path)?;
        read_bounded(file).map_err(|error| absence(path, &error))
    }

    fn read_link(&self, path: &RelativePath) -> Result<Vec<u8>, HostError> {
        let entry = self.pinned(path)?;
        let target = rustix::fs::readlinkat(&entry.directory, &entry.name, Vec::new())
            .map_err(io::Error::from)
            .map_err(|error| absence(path, &error))?;
        let bytes = target.as_bytes().to_vec();
        if bytes.len() > RelativePath::LIMIT {
            return Err(HostError::Failed(format!(
                "{path}: symbolic-link target exceeds the {}-byte limit",
                RelativePath::LIMIT
            )));
        }
        Ok(bytes)
    }

    fn read_range(
        &self,
        path: &RelativePath,
        offset: u64,
        limit: usize,
        observed: Option<&str>,
    ) -> Result<FileRange, HostError> {
        let mut file = self.opened(path)?;
        let before = file.metadata().map_err(|error| absence(path, &error))?;
        if !before.is_file() {
            return Err(HostError::Conflict(format!("{path} is not a regular file")));
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| absence(path, &error))?;
        let mut contents = Vec::with_capacity(limit);
        std::io::Read::by_ref(&mut file)
            .take(limit as u64)
            .read_to_end(&mut contents)
            .map_err(|error| absence(path, &error))?;
        let after = file.metadata().map_err(|error| absence(path, &error))?;
        let identity = file_identity(&before);
        if observed.is_some_and(|value| value != identity) {
            return Err(HostError::Conflict(format!(
                "{path} no longer matches the observed identity"
            )));
        }
        if identity != file_identity(&after) {
            return Err(HostError::Conflict(format!("{path} changed while it was read")));
        }
        let total = before.len();
        let eof = offset.saturating_add(contents.len() as u64) >= total;
        Ok(FileRange {
            path: path.clone(),
            identity,
            offset,
            total,
            eof,
            truncated: !eof,
            contents,
        })
    }

    fn stat(&self, path: &RelativePath) -> Result<Entry, HostError> {
        let entry = self.pinned(path)?;
        let status = rustix::fs::statat(
            &entry.directory,
            &entry.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)
        .map_err(|error| absence(path, &error))?;
        Ok(Entry {
            path: path.clone(),
            directory: rustix::fs::FileType::from_raw_mode(status.st_mode)
                == rustix::fs::FileType::Directory,
            size: status.st_size.try_into().unwrap_or(u64::MAX),
            identity: Some(status_identity(&status)),
        })
    }

    /// # Errors
    /// Returns `HostError::Absent` when the containing directory does not exist,
    /// `HostError::Conflict` for a path that resolves outside the root, and a
    /// failure otherwise.
    fn write(&self, path: &RelativePath, contents: &[u8]) -> Result<(), HostError> {
        let entry = self.stat(path)?;
        if entry.directory {
            return Err(HostError::Conflict(format!("{path} is a directory")));
        }
        let observed = entry
            .identity
            .ok_or_else(|| HostError::Conflict(format!("{path} has no stable identity")))?;
        self.write_observed(path, &observed, contents).map(|_| ())
    }

    fn write_observed(&self, path: &RelativePath, observed: &str, contents: &[u8]) -> Result<String, HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let entry = self.pinned(path)?;
        let opened = open_entry_identity(path, &entry, observed)?;
        if !opened.is_file() {
            return Err(HostError::Conflict(format!("{path} is not a regular file")));
        }
        atomic_replace_observed(&entry, &opened, contents, |_, _| Ok(()))
            .map_err(|error| absence(path, &error))?
            .ok_or_else(|| HostError::Conflict(format!("{path} no longer matches the observed identity")))
    }

    fn create_observed(&self, path: &RelativePath, contents: &[u8]) -> Result<String, HostError> {
        self.create_observed_with(path, contents, || Ok(()))
    }

    fn mkdir(&self, path: &RelativePath) -> Result<(), HostError> {
        self.mkdir_with(path, || Ok(()))
    }

    fn rename(&self, from: &RelativePath, to: &RelativePath) -> Result<(), HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let source = self.pinned(from)?;
        let destination = self.pinned(to)?;
        rename_noreplace(&source, &destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                HostError::Conflict(format!("{to} already exists"))
            } else {
                absence(from, &error)
            }
        })?;
        source.directory.sync_all().map_err(|error| absence(from, &error))?;
        if (source.metadata.dev(), source.metadata.ino()) != (destination.metadata.dev(), destination.metadata.ino()) {
            destination.directory.sync_all().map_err(|error| absence(to, &error))?;
        }
        Ok(())
    }

    fn rename_observed(&self, from: &RelativePath, to: &RelativePath, observed: &str) -> Result<String, HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let source = self.pinned(from)?;
        let opened = open_entry_identity(from, &source, observed)?;
        let destination = self.pinned(to)?;
        rename_noreplace(&source, &destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                HostError::Conflict(format!("{to} already exists"))
            } else {
                absence(from, &error)
            }
        })?;
        let status = rustix::fs::statat(
            &destination.directory,
            &destination.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)
        .map_err(|error| absence(to, &error))?;
        if (status.st_dev, status.st_ino) != (opened.dev(), opened.ino()) {
            rename_noreplace(&destination, &source).map_err(|error| absence(from, &error))?;
            return Err(HostError::Conflict(format!(
                "{from} no longer matches the observed identity"
            )));
        }
        source.directory.sync_all().map_err(|error| absence(from, &error))?;
        if (source.metadata.dev(), source.metadata.ino()) != (destination.metadata.dev(), destination.metadata.ino()) {
            destination.directory.sync_all().map_err(|error| absence(to, &error))?;
        }
        let status = rustix::fs::statat(
            &destination.directory,
            &destination.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)
        .map_err(|error| absence(to, &error))?;
        Ok(status_identity(&status))
    }

    fn remove(&self, path: &RelativePath) -> Result<(), HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let entry = self.pinned(path)?;
        remove_entry(&entry)
            .and_then(|()| entry.directory.sync_all())
            .map_err(|error| absence(path, &error))
    }

    fn remove_observed(&self, path: &RelativePath, observed: &str) -> Result<(), HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let entry = self.pinned(path)?;
        let opened = open_entry_identity(path, &entry, observed)?;
        for _ in 0..128 {
            let quarantine = CString::new(format!(
                ".husklet-remove-{}-{}",
                std::process::id(),
                TEMPORARY.fetch_add(1, Ordering::Relaxed)
            ))
            .expect("fixed prefix has no NUL");
            let captured = PinnedEntry {
                directory: entry.directory.try_clone().map_err(|error| absence(path, &error))?,
                metadata: entry.directory.metadata().map_err(|error| absence(path, &error))?,
                name: quarantine,
            };
            match rename_noreplace(&entry, &captured) {
                Ok(()) => {
                    let status = rustix::fs::statat(
                        &captured.directory,
                        &captured.name,
                        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                    )
                    .map_err(io::Error::from)
                    .map_err(|error| absence(path, &error))?;
                    if (status.st_dev, status.st_ino) != (opened.dev(), opened.ino()) {
                        rename_noreplace(&captured, &entry).map_err(|error| absence(path, &error))?;
                        return Err(HostError::Conflict(format!(
                            "{path} no longer matches the observed identity"
                        )));
                    }
                    return remove_entry(&captured)
                        .and_then(|()| entry.directory.sync_all())
                        .map_err(|error| absence(path, &error));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(absence(path, &error)),
            }
        }
        Err(HostError::Failed(
            "could not reserve a unique removal quarantine".into(),
        ))
    }
}

struct PinnedEntry {
    directory: File,
    metadata: std::fs::Metadata,
    name: CString,
}

#[cfg(test)]
fn pin_entry_with(target: &Path, before_open: impl FnOnce() -> io::Result<()>) -> io::Result<PinnedEntry> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("entry has no parent directory"))?;
    let expected = std::fs::metadata(parent)?;
    before_open()?;
    let directory = File::open(parent)?;
    let actual = directory.metadata()?;
    if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
        return Err(io::Error::other("containing directory changed before operation"));
    }
    Ok(PinnedEntry {
        directory,
        metadata: actual,
        name: c_name(target)?,
    })
}

fn rename_noreplace(source: &PinnedEntry, destination: &PinnedEntry) -> io::Result<()> {
    rustix::fs::renameat_with(
        &source.directory,
        &source.name,
        &destination.directory,
        &destination.name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}

fn remove_entry(entry: &PinnedEntry) -> io::Result<()> {
    let status = rustix::fs::statat(&entry.directory, &entry.name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
    let flags = if rustix::fs::FileType::from_raw_mode(status.st_mode) == rustix::fs::FileType::Directory {
        rustix::fs::AtFlags::REMOVEDIR
    } else {
        rustix::fs::AtFlags::empty()
    };
    rustix::fs::unlinkat(&entry.directory, &entry.name, flags).map_err(Into::into)
}

fn status_identity(status: &rustix::fs::Stat) -> String {
    format!(
        "v1:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
        status.st_dev,
        status.st_ino,
        status.st_size,
        status.st_mtime,
        status.st_mtime_nsec,
        status.st_ctime,
        status.st_ctime_nsec
    )
}

fn open_entry_identity(
    path: &RelativePath,
    entry: &PinnedEntry,
    observed: &str,
) -> Result<std::fs::Metadata, HostError> {
    let descriptor = rustix::fs::openat(
        &entry.directory,
        &entry.name,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)
    .map_err(|error| absence(path, &error))?;
    let metadata = File::from(descriptor)
        .metadata()
        .map_err(|error| absence(path, &error))?;
    if file_identity(&metadata) != observed {
        return Err(HostError::Conflict(format!(
            "{path} no longer matches the observed identity"
        )));
    }
    Ok(metadata)
}

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

struct Temporary {
    directory: File,
    name: CString,
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = rustix::fs::unlinkat(&self.directory, &self.name, rustix::fs::AtFlags::empty());
    }
}

fn atomic_create(target: &PinnedEntry, contents: &[u8]) -> io::Result<String> {
    atomic_create_with(target, contents, || Ok(()))
}

/// Exchanges the staged file with the live name in one namespace operation.
/// `None` means another writer replaced the observed inode; the exchange was
/// reversed before returning, so the foreign value remains untouched.
fn atomic_replace_observed(
    target: &PinnedEntry,
    observed: &std::fs::Metadata,
    contents: &[u8],
    after_exchange: impl FnOnce(&File, &CStr) -> io::Result<()>,
) -> io::Result<Option<String>> {
    let mut published = None;
    let mut conflict = false;
    atomic_write_at(target, contents, |directory, temporary, file| {
        rustix::fs::renameat_with(
            directory,
            temporary,
            directory,
            &target.name,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .map_err(io::Error::from)?;
        let inspected = (|| {
            after_exchange(directory, temporary)?;
            let displaced = rustix::fs::statat(
                directory,
                temporary,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(io::Error::from)?;
            if (displaced.st_dev, displaced.st_ino) != (observed.dev(), observed.ino()) {
                return Ok(None);
            }
            let identity = file_identity(&file.metadata()?);
            directory.sync_all()?;
            Ok(Some(identity))
        })();
        let Some(identity) = inspected.map_err(|error| match rollback_exchange(directory, temporary, &target.name) {
            Ok(()) => error,
            Err(rollback) => io::Error::other(format!(
                "publication failed ({error}) and rollback failed ({rollback})"
            )),
        })?
        else {
            rustix::fs::renameat_with(
                directory,
                temporary,
                directory,
                &target.name,
                rustix::fs::RenameFlags::EXCHANGE,
            )
            .map_err(io::Error::from)?;
            directory.sync_all()?;
            conflict = true;
            return Ok(false);
        };
        published = Some(identity);
        Ok(true)
    })?;
    Ok((!conflict).then_some(published).flatten())
}

fn rollback_exchange(directory: &File, temporary: &CStr, target: &CStr) -> io::Result<()> {
    rustix::fs::renameat_with(
        directory,
        temporary,
        directory,
        target,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(io::Error::from)?;
    directory.sync_all()
}

fn atomic_create_with(
    target: &PinnedEntry,
    contents: &[u8],
    after_publish: impl FnOnce() -> io::Result<()>,
) -> io::Result<String> {
    let mut identity = None;
    atomic_write_at(target, contents, |directory, temporary, file| {
        rustix::fs::renameat_with(
            directory,
            temporary,
            directory,
            &target.name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)?;
        after_publish()?;
        identity = Some(file_identity(&file.metadata()?));
        directory.sync_all()?;
        Ok(true)
    })?;
    identity.ok_or_else(|| io::Error::other("atomic creation did not publish"))
}

fn atomic_write_at(
    target: &PinnedEntry,
    contents: &[u8],
    before_publish: impl FnOnce(&File, &CStr, &File) -> io::Result<bool>,
) -> io::Result<()> {
    if symlink_at(&target.directory, &target.name)? {
        return Err(io::Error::other("publication target is a symbolic link"));
    }
    let mut opened = None;
    for _ in 0..128 {
        let sequence = TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!(".husklet-write-{}-{sequence}.tmp", std::process::id()))
            .expect("generated temporary names contain no NUL");
        match open_exclusive_at(&target.directory, &name) {
            Ok(file) => {
                opened = Some((
                    Temporary {
                        directory: target.directory.try_clone()?,
                        name,
                    },
                    file,
                ));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let (temporary, mut file) = opened
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "could not reserve atomic write temporary"))?;
    file.write_all(contents)?;
    file.sync_all()?;
    if before_publish(&target.directory, &temporary.name, &file)? {
        return Ok(());
    }
    target.directory.sync_all()
}

fn file_identity(metadata: &std::fs::Metadata) -> String {
    format!(
        "v1:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

#[cfg(test)]
fn atomic_write_before_publish(
    target: &Path,
    contents: &[u8],
    before_publish: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))?
        .to_owned();
    atomic_write_with(
        target,
        contents,
        || Ok(()),
        |_, temporary, _| before_publish(&parent.join(temporary.to_string_lossy().as_ref())).map(|()| false),
    )
}

#[cfg(test)]
fn atomic_write_with(
    target: &Path,
    contents: &[u8],
    before_open: impl FnOnce() -> io::Result<()>,
    before_publish: impl FnOnce(&File, &CStr, &File) -> io::Result<bool>,
) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))?;
    let expected = std::fs::metadata(parent)?;
    before_open()?;
    let directory = File::open(parent)?;
    let actual = directory.metadata()?;
    if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
        return Err(io::Error::other("containing directory changed before publication"));
    }
    let target_name = c_name(target)?;
    if symlink_at(&directory, &target_name)? {
        return Err(io::Error::other("publication target is a symbolic link"));
    }
    let mut opened = None;
    for _ in 0..128 {
        let sequence = TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!(".husklet-write-{}-{sequence}.tmp", std::process::id()))
            .expect("generated temporary names contain no NUL");
        match open_exclusive_at(&directory, &name) {
            Ok(file) => {
                opened = Some((
                    Temporary {
                        directory: directory.try_clone()?,
                        name,
                    },
                    file,
                ));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let (temporary, mut file) = opened
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "could not reserve atomic write temporary"))?;
    file.write_all(contents)?;
    file.sync_all()?;
    if before_publish(&directory, &temporary.name, &file)? {
        return Ok(());
    }
    rename_at(&directory, &temporary.name, &target_name)?;
    directory.sync_all()?;
    Ok(())
}

fn c_name(path: &Path) -> io::Result<CString> {
    let name = path.file_name().ok_or_else(|| io::Error::other("file has no name"))?;
    CString::new(name.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name contains NUL"))
}

fn open_exclusive_at(directory: &File, name: &CStr) -> io::Result<File> {
    let descriptor = rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )?;
    Ok(File::from(descriptor))
}

fn symlink_at(directory: &File, name: &CStr) -> io::Result<bool> {
    match rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(status) => Ok(rustix::fs::FileType::from_raw_mode(status.st_mode) == rustix::fs::FileType::Symlink),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn rename_at(directory: &File, from: &CStr, to: &CStr) -> io::Result<()> {
    // Both names are interpreted relative to the same pinned directory.
    // rename replaces, rather than follows, `to`.
    rustix::fs::renameat(directory, from, directory, to).map_err(Into::into)
}

fn open_at(directory: &File, name: &str, flags: rustix::fs::OFlags) -> io::Result<File> {
    let descriptor = rustix::fs::openat(
        directory,
        name,
        flags | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    Ok(File::from(descriptor))
}

fn read_bounded(file: File) -> io::Result<Vec<u8>> {
    let mut contents = Vec::new();
    file.take((READ_BYTES_LIMIT + 1) as u64).read_to_end(&mut contents)?;
    if contents.len() > READ_BYTES_LIMIT {
        return Err(io::Error::other(format!(
            "file exceeds the {READ_BYTES_LIMIT}-byte read limit"
        )));
    }
    Ok(contents)
}

/// A missing file is an absence; anything else is a host failure.
fn absence(path: &RelativePath, error: &io::Error) -> HostError {
    if error.kind() == io::ErrorKind::NotFound {
        return HostError::Absent(path.to_string());
    }
    if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
        return HostError::Conflict(format!("{path} contains a symbolic link or non-directory component"));
    }
    HostError::Failed(format!("{path}: {error}"))
}

/// Describes one directory entry relative to the listed path.
fn described_at(parent: &RelativePath, directory: &File, entry: &rustix::fs::DirEntry) -> Result<Entry, HostError> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let joined = if parent.parts().is_empty() {
        name
    } else {
        format!("{parent}/{name}")
    };
    let path = RelativePath::new(joined).map_err(|refusal| HostError::Failed(refusal.to_string()))?;
    let metadata = rustix::fs::statat(directory, entry.file_name(), rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)
        .map_err(|error| HostError::Failed(format!("{path}: {error}")))?;
    if metadata.st_ino != entry.ino() {
        return Err(HostError::Conflict(format!(
            "{path} changed while its directory was listed"
        )));
    }
    Ok(Entry {
        path,
        directory: rustix::fs::FileType::from_raw_mode(metadata.st_mode) == rustix::fs::FileType::Directory,
        size: metadata.st_size.try_into().unwrap_or(u64::MAX),
        identity: Some(format!(
            "v1:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_size,
            metadata.st_mtime,
            metadata.st_mtime_nsec,
            metadata.st_ctime,
            metadata.st_ctime_nsec
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::{WorkspaceDirectory, atomic_replace_observed, open_entry_identity};
    use hl_extension::port::{HostError, WorkspaceFiles};
    use hl_extension::{FilesystemSelector, RelativePath};
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    fn path(value: &str) -> RelativePath {
        RelativePath::new(value).expect("path")
    }

    fn subtree(value: &str) -> FilesystemSelector {
        FilesystemSelector::Subtree { subtree: path(value) }
    }

    #[test]
    fn directory_creation_cannot_cross_its_selector_during_an_authorized_rename() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("allowed")).expect("allowed directory");
        let files = Arc::new(WorkspaceDirectory::new(&root).expect("workspace"));
        let (pinned, pinned_receiver) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        let creating = Arc::clone(&files);
        let creation = std::thread::spawn(move || {
            creating.mkdir_with(&path("allowed/new"), || {
                pinned.send(()).expect("announce pinned parent");
                release_receiver.recv().expect("release creation");
                Ok(())
            })
        });
        pinned_receiver.recv().expect("creation pinned its authorized parent");

        let (renamed, renamed_receiver) = mpsc::channel();
        let renaming = Arc::clone(&files);
        let rename = std::thread::spawn(move || {
            let result = renaming.rename(&path("allowed"), &path("restricted"));
            renamed.send(()).expect("announce rename completion");
            result
        });
        assert!(
            renamed_receiver.recv_timeout(Duration::from_millis(100)).is_err(),
            "rename crossed an in-flight exact-path creation"
        );

        release.send(()).expect("release creation");
        creation.join().expect("creation thread").expect("directory creation");
        rename.join().expect("rename thread").expect("authorized rename");
        assert!(root.join("restricted/new").is_dir());
    }

    #[test]
    fn watcher_overflow_is_explicit_until_a_complete_inventory_rebases_the_scope() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let files = WorkspaceDirectory::new(&root).unwrap();
        let roots = [subtree("docs")];
        let inventory = files.inventory(&roots).unwrap();
        let baseline = files.changes_since(&roots, &inventory.journal, inventory.revision, 32).unwrap();
        files.watcher.lock().unwrap().overflow.store(true, Ordering::Release);
        let overflow = files.changes_since(&roots, &baseline.journal, baseline.next, 32).unwrap();
        assert!(overflow.truncated);
        assert!(overflow.changes.is_empty());
        let rebased = files.inventory(&roots).unwrap();
        assert!(rebased.complete);
        assert!(!files.changes_since(&roots, &rebased.journal, rebased.revision, 32).unwrap().truncated);
    }

    #[test]
    fn complete_inventory_cursor_bridges_without_losing_a_later_change() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/readme.md"), b"before").unwrap();
        let files = WorkspaceDirectory::new(&root).unwrap();
        let roots = [subtree("docs")];

        let inventory = files.inventory(&roots).unwrap();
        assert!(inventory.complete);
        std::fs::write(root.join("docs/readme.md"), b"after").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));

        let page = files.changes_since(&roots, &inventory.journal, inventory.revision, 32).unwrap();
        assert!(!page.truncated);
        assert!(
            page.changes
                .iter()
                .any(|change| { change.revision > inventory.revision && change.path.as_str() == "docs/readme.md" })
        );
    }

    #[test]
    fn recreated_workspace_directory_invalidates_a_cursor_from_the_prior_journal() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/readme.md"), b"before").unwrap();
        let roots = [subtree("docs")];

        let old = {
            let files = WorkspaceDirectory::new(&root).unwrap();
            files.inventory(&roots).unwrap()
        };
        std::fs::write(root.join("docs/readme.md"), b"changed while stopped").unwrap();
        let files = WorkspaceDirectory::new(&root).unwrap();
        let invalidated = files
            .changes_since(&roots, &old.journal, old.revision, 32)
            .unwrap();

        assert_ne!(invalidated.journal, old.journal);
        assert!(invalidated.truncated);
        assert!(invalidated.changes.is_empty());
        assert_eq!(invalidated.next, invalidated.current);
        let fresh = files.inventory(&roots).unwrap();
        assert_eq!(fresh.journal, invalidated.journal);
        assert!(!files
            .changes_since(&roots, &fresh.journal, fresh.revision, 32)
            .unwrap()
            .truncated);
    }

    #[test]
    fn one_scoped_reader_cannot_drain_another_readers_watcher_invalidations() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        let files = WorkspaceDirectory::new(&root).unwrap();
        let alpha = [subtree("alpha")];
        let beta = [subtree("beta")];
        let alpha_inventory = files.inventory(&alpha).unwrap();
        let beta_inventory = files.inventory(&beta).unwrap();
        assert_ne!(alpha_inventory.journal, beta_inventory.journal);
        std::fs::write(root.join("alpha/transient"), b"x").unwrap();
        std::fs::remove_file(root.join("alpha/transient")).unwrap();
        std::fs::write(root.join("beta/transient"), b"x").unwrap();
        std::fs::remove_file(root.join("beta/transient")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));
        let alpha_page = files.changes_since(&alpha, &alpha_inventory.journal, 0, 32).unwrap();
        let beta_page = files.changes_since(&beta, &beta_inventory.journal, 0, 32).unwrap();
        assert!(
            alpha_page
                .changes
                .iter()
                .any(|change| change.path.as_str() == "alpha/transient")
        );
        assert!(
            beta_page
                .changes
                .iter()
                .any(|change| change.path.as_str() == "beta/transient")
        );
        assert!(
            alpha_page
                .changes
                .iter()
                .all(|change| !change.path.as_str().starts_with("beta/"))
        );
        assert!(
            beta_page
                .changes
                .iter()
                .all(|change| !change.path.as_str().starts_with("alpha/"))
        );
    }

    #[test]
    fn an_ordinary_nested_path_is_accepted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("logs")).expect("directory");
        std::fs::write(root.join("logs/app.log"), b"hello").expect("file");
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert_eq!(files.read(&path("logs/app.log")).expect("contents"), b"hello");
        let listed = files.list(&path("logs")).expect("listing");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path.as_str(), "logs/app.log");
        assert_eq!(listed[0].size, 5);
        assert!(!listed[0].directory);
        assert_eq!(files.stat(&path("logs/app.log")).expect("metadata"), listed[0]);

        files
            .create_observed(&path("logs/written.txt"), b"new")
            .expect("create");
        assert_eq!(std::fs::read(root.join("logs/written.txt")).expect("file"), b"new");
        files.mkdir(&path("logs/nested")).expect("mkdir");
        files
            .rename(&path("logs/written.txt"), &path("logs/renamed.txt"))
            .expect("rename");
        files.remove(&path("logs/renamed.txt")).expect("remove file");
        files.remove(&path("logs/nested")).expect("remove empty directory");
        assert!(!root.join("logs/renamed.txt").exists());
    }

    #[test]
    fn a_symbolic_link_out_of_the_root_is_refused() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        let secret = temporary.path().join("secret.txt");
        std::fs::write(&secret, b"private").expect("secret");
        let local = root.join("local.txt");
        std::fs::write(&local, b"local").expect("local target");
        std::os::unix::fs::symlink(&secret, root.join("escape.txt")).expect("link");
        std::os::unix::fs::symlink("local.txt", root.join("local-link.txt")).expect("local link");
        std::os::unix::fs::symlink(temporary.path(), root.join("outside")).expect("link");
        let files = WorkspaceDirectory::new(&root).expect("root");

        // The name is syntactically valid and inside a declared root, so nothing
        // upstream of this adapter can refuse it.
        let escape = path("escape.txt");
        assert!(matches!(files.read(&escape), Err(HostError::Conflict(_))));
        let link = files.stat(&escape).expect("link metadata");
        assert!(!link.directory);
        assert_eq!(link.size, secret.as_os_str().as_encoded_bytes().len() as u64);
        assert_eq!(
            files.read_link(&escape).expect("link text"),
            secret.as_os_str().as_encoded_bytes()
        );
        let inventory = files
            .inventory(&[hl_extension::FilesystemSelector::Subtree {
                subtree: path("escape.txt"),
            }])
            .expect("a link does not break scoped inventory");
        assert_eq!(inventory.entries, vec![link]);
        assert!(matches!(files.write(&escape, b"owned"), Err(HostError::Conflict(_))));
        assert!(matches!(
            files.write(&path("local-link.txt"), b"owned"),
            Err(HostError::Conflict(_))
        ));
        assert!(matches!(files.list(&path("outside")), Err(HostError::Conflict(_))));
        assert!(
            matches!(files.read(&path("outside/secret.txt")), Err(HostError::Conflict(_))),
            "a linked directory does not become a root"
        );
        assert_eq!(
            std::fs::read(&secret).expect("secret"),
            b"private",
            "the refused write must not have reached the target"
        );
        files.remove(&path("escape.txt")).expect("remove link itself");
        assert_eq!(std::fs::read(&secret).expect("secret"), b"private");
        assert_eq!(std::fs::read(&local).expect("local target"), b"local");
    }

    #[test]
    fn concurrent_atomic_writes_publish_one_complete_value_and_leave_no_temporaries() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = std::sync::Arc::new(WorkspaceDirectory::new(&root).expect("root"));
        std::fs::write(root.join("state.bin"), b"").expect("initial file");
        let first = vec![b'a'; 64 * 1024];
        let second = vec![b'b'; 64 * 1024];
        let threads = [first.clone(), second.clone()].map(|contents| {
            let files = std::sync::Arc::clone(&files);
            std::thread::spawn(move || files.write(&path("state.bin"), &contents))
        });
        let mut published_writes = 0;
        for thread in threads {
            published_writes += usize::from(thread.join().expect("writer").is_ok());
        }
        assert!(published_writes >= 1, "at least one raced write publishes");
        let published = std::fs::read(root.join("state.bin")).expect("published");
        assert!(
            published == first || published == second,
            "a reader sees one whole publication"
        );
        assert!(std::fs::read_dir(&root).expect("root listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-")
        }));
    }

    #[test]
    fn observed_ranges_reject_replacements_and_observed_creation_never_overwrites() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let target = path("state.bin");

        let created = files
            .create_observed(&target, b"first-page/second-page")
            .expect("create against observed absence");
        let first = files.read_range(&target, 0, 10, None).expect("first page");
        assert_eq!(first.identity, created);
        assert_eq!(first.contents, b"first-page");
        assert!(!first.eof);

        files.write(&target, b"replacement").expect("concurrent replacement");
        assert!(matches!(
            files.read_range(&target, 10, 10, Some(&first.identity)),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(
            std::fs::read(root.join("state.bin")).expect("replacement"),
            b"replacement"
        );
        assert!(matches!(
            files.create_observed(&target, b"second create"),
            Err(HostError::Conflict(_))
        ));
    }

    #[test]
    fn observed_write_replaces_only_the_inspected_file_and_returns_the_new_identity() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let target = path("settings.json");
        let first = files.create_observed(&target, b"one").expect("create");

        let second = files
            .write_observed(&target, &first, b"two")
            .expect("compare-and-swap write");
        assert_ne!(second, first);
        assert_eq!(
            files.stat(&target).expect("stat").identity.as_deref(),
            Some(second.as_str())
        );
        assert_eq!(files.read(&target).expect("read"), b"two");

        assert!(matches!(
            files.write_observed(&target, &first, b"stale"),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(files.read(&target).expect("read"), b"two");
        assert!(std::fs::read_dir(&root).expect("root listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-old-")
        }));
    }

    #[test]
    fn observed_write_exchange_never_removes_the_live_name() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let target = path("settings.json");
        let first = files.create_observed(&target, b"old").expect("create");
        let entry = files.pinned(&target).expect("pinned target");
        let observed = open_entry_identity(&target, &entry, &first).expect("observed target");

        let second = atomic_replace_observed(&entry, &observed, b"new", |directory, displaced| {
            let live = rustix::fs::openat(
                directory,
                &entry.name,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::empty(),
            )?;
            let old = rustix::fs::openat(
                directory,
                displaced,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::empty(),
            )?;
            let mut live = std::fs::File::from(live);
            let mut old = std::fs::File::from(old);
            let mut live_bytes = Vec::new();
            let mut old_bytes = Vec::new();
            std::io::Read::read_to_end(&mut live, &mut live_bytes)?;
            std::io::Read::read_to_end(&mut old, &mut old_bytes)?;
            assert_eq!(live_bytes, b"new", "the authorized name is continuously present");
            assert_eq!(old_bytes, b"old", "the observed inode is retained until validation");
            Ok(())
        })
        .expect("atomic exchange")
        .expect("matching identity publishes");

        assert_ne!(second, first);
        assert_eq!(std::fs::read(root.join("settings.json")).expect("live file"), b"new");
    }

    #[test]
    fn observed_write_exchange_restores_a_raced_replacement_without_overwriting_it() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let target = path("settings.json");
        let first = files.create_observed(&target, b"observed").expect("create");
        let entry = files.pinned(&target).expect("pinned target");
        let observed = open_entry_identity(&target, &entry, &first).expect("observed target");
        std::fs::rename(root.join("settings.json"), root.join("observed-away")).expect("move observed inode");
        std::fs::write(root.join("settings.json"), b"foreign").expect("raced replacement");

        let result = atomic_replace_observed(&entry, &observed, b"ours", |_, _| Ok(())).expect("exchange rollback");
        assert_eq!(result, None, "the raced inode cannot satisfy the observation");
        assert_eq!(
            std::fs::read(root.join("settings.json")).expect("foreign value"),
            b"foreign",
            "rollback preserves the raced replacement"
        );
        assert_eq!(
            std::fs::read(root.join("observed-away")).expect("observed value"),
            b"observed"
        );
    }

    #[test]
    fn separate_observed_writes_cannot_claim_transactional_multi_file_rollback() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let first = path("first.txt");
        let second = path("second.txt");
        let first_identity = files.create_observed(&first, b"one").expect("first");
        let stale_second = files.create_observed(&second, b"two").expect("second");
        let current_second = files
            .write_observed(&second, &stale_second, b"changed elsewhere")
            .expect("concurrent second write");

        files
            .write_observed(&first, &first_identity, b"applied")
            .expect("first patch member");
        assert!(matches!(
            files.write_observed(&second, &stale_second, b"would apply"),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(files.read(&first).expect("first contents"), b"applied");
        assert_eq!(files.read(&second).expect("second contents"), b"changed elsewhere");
        assert_eq!(
            files.stat(&second).expect("second stat").identity.as_deref(),
            Some(current_second.as_str())
        );
    }

    #[test]
    fn inventory_recurses_only_declared_roots_and_reports_its_bound() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("source/nested")).expect("source");
        std::fs::create_dir_all(root.join("private")).expect("private");
        std::fs::write(root.join("source/main.ts"), b"main").expect("source file");
        std::fs::write(root.join("source/nested/lib.ts"), b"lib").expect("nested file");
        std::fs::write(root.join("private/key"), b"secret").expect("private file");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let inventory = files.inventory(&[subtree("source")]).expect("inventory");
        assert!(inventory.complete);
        assert_eq!(inventory.coalesced, 0);
        assert_eq!(
            inventory
                .entries
                .iter()
                .map(|entry| entry.path.to_string())
                .collect::<Vec<_>>(),
            ["source", "source/main.ts", "source/nested", "source/nested/lib.ts"]
        );
        assert!(inventory.entries.iter().all(|entry| entry.identity.is_some()));
        let whole = files
            .inventory(&[subtree("source"), subtree("private")])
            .expect("declared-root inventory");
        assert!(
            whole
                .entries
                .iter()
                .any(|entry| entry.path.to_string() == "private/key")
        );
        for index in 0..260 {
            std::fs::write(root.join("source").join(format!("extra-{index}")), b"x").expect("extra file");
        }
        let bounded = files.inventory(&[subtree("source")]).expect("bounded inventory");
        assert_eq!(bounded.entries.len(), 256);
        assert!(!bounded.complete);
    }

    #[test]
    fn observed_rename_and_remove_capture_then_validate_without_touching_replacements() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let source = path("source");
        files.create_observed(&source, b"original").expect("original");
        let observed = files.stat(&source).expect("stat").identity.expect("identity");

        files.write(&source, b"replacement").expect("replacement");
        assert!(matches!(
            files.rename_observed(&source, &path("destination"), &observed),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(
            std::fs::read(root.join("source")).expect("restored replacement"),
            b"replacement"
        );
        assert!(
            !root.join("destination").exists(),
            "stale rename rolls its captured entry back"
        );
        assert!(matches!(
            files.remove_observed(&source, &observed),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(
            std::fs::read(root.join("source")).expect("preserved replacement"),
            b"replacement"
        );
        assert!(std::fs::read_dir(&root).expect("listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-remove-")
        }));

        let current = files.stat(&source).expect("current stat").identity.expect("identity");
        let renamed = files
            .rename_observed(&source, &path("destination"), &current)
            .expect("observed rename");
        assert_eq!(
            renamed,
            files
                .stat(&path("destination"))
                .expect("destination stat")
                .identity
                .expect("identity")
        );
        files
            .remove_observed(&path("destination"), &renamed)
            .expect("observed remove");
        assert!(!root.join("destination").exists());
    }

    #[test]
    fn failed_publication_preserves_the_old_entry_and_cleans_its_temporary() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("target")).expect("old directory");
        std::fs::write(root.join("target/held"), b"old").expect("old contents");
        let files = WorkspaceDirectory::new(&root).expect("root");
        assert!(files.write(&path("target"), b"replacement").is_err());
        assert_eq!(std::fs::read(root.join("target/held")).expect("old contents"), b"old");
        assert!(std::fs::read_dir(&root).expect("root listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-")
        }));
    }

    #[test]
    fn failure_before_publication_preserves_prior_file_bytes_and_cleans_temporary() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("state");
        std::fs::write(&target, b"old bytes").expect("old contents");

        let result = super::atomic_write_before_publish(&target, b"new bytes", |_| {
            Err(std::io::Error::other("injected before publication"))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).expect("old contents"), b"old bytes");
        assert!(
            std::fs::read_dir(temporary.path())
                .expect("directory listing")
                .all(|entry| {
                    !entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".husklet-write-")
                })
        );
    }

    #[test]
    fn replaced_parent_is_rejected_before_any_raced_path_is_written() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let parent = temporary.path().join("parent");
        let displaced = temporary.path().join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&parent).expect("parent");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(parent.join("state"), b"old bytes").expect("old contents");
        std::fs::write(outside.join("state"), b"outside bytes").expect("outside contents");
        let target = parent.join("state");

        let result = super::atomic_write_with(
            &target,
            b"new bytes",
            || {
                std::fs::rename(&parent, &displaced)?;
                std::os::unix::fs::symlink(&outside, &parent)?;
                Ok(())
            },
            |_, _, _| Ok(false),
        );

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(displaced.join("state")).expect("old contents"),
            b"old bytes"
        );
        assert_eq!(
            std::fs::read(outside.join("state")).expect("outside contents"),
            b"outside bytes"
        );
        assert!(std::fs::read_dir(&outside).expect("outside listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-")
        }));
    }

    #[test]
    fn ancestor_replaced_after_confinement_cannot_redirect_creation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let parent = root.join("parent");
        let displaced = root.join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&parent).expect("parent");
        std::fs::create_dir_all(&outside).expect("outside");
        let files = WorkspaceDirectory::new(&root).expect("workspace");
        let path = path("parent/new.txt");
        let result = files.create_observed_with(&path, b"workspace bytes", || {
            std::fs::rename(&parent, &displaced)?;
            std::os::unix::fs::symlink(&outside, &parent)?;
            Ok(())
        });

        assert!(result.is_err(), "an ancestor replacement must fail publication");
        assert!(!outside.join("new.txt").exists(), "creation escaped the workspace");
        assert!(!displaced.join("new.txt").exists(), "failed creation was not published");
    }

    #[test]
    fn created_file_identity_uses_the_published_file_after_ancestor_replacement() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let parent = root.join("parent");
        let displaced = root.join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&parent).expect("parent");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("new.txt"), b"outside bytes").expect("outside file");
        let files = WorkspaceDirectory::new(&root).expect("workspace");
        let entry = files
            .pinned_publication(&path("parent/new.txt"))
            .expect("pinned parent");

        let identity = super::atomic_create_with(&entry, b"workspace bytes", || {
            std::fs::rename(&parent, &displaced)?;
            std::os::unix::fs::symlink(&outside, &parent)?;
            Ok(())
        })
        .expect("creation remains bound to the opened parent");

        assert_eq!(
            std::fs::read(displaced.join("new.txt")).expect("created file"),
            b"workspace bytes"
        );
        assert_eq!(
            std::fs::read(outside.join("new.txt")).expect("outside file"),
            b"outside bytes"
        );
        assert_eq!(
            identity,
            super::file_identity(&std::fs::metadata(displaced.join("new.txt")).expect("created metadata"))
        );
    }

    #[test]
    fn final_symlink_raced_in_before_rename_is_replaced_not_followed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("state");
        let outside = temporary.path().join("outside");
        std::fs::write(&target, b"old bytes").expect("old contents");
        std::fs::write(&outside, b"outside bytes").expect("outside contents");

        super::atomic_write_before_publish(&target, b"new bytes", |_| {
            std::fs::remove_file(&target)?;
            std::os::unix::fs::symlink(&outside, &target)?;
            Ok(())
        })
        .expect("atomic publication");

        assert_eq!(std::fs::read(&target).expect("published contents"), b"new bytes");
        assert!(
            !std::fs::symlink_metadata(&target)
                .expect("published metadata")
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&outside).expect("outside contents"), b"outside bytes");
    }

    #[test]
    fn unrelated_torn_temporary_is_never_published_or_removed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        let torn = root.join(".husklet-write-old.tmp");
        std::fs::write(&torn, b"torn").expect("torn temporary");
        std::fs::write(root.join("state"), b"old").expect("initial state");
        let files = WorkspaceDirectory::new(&root).expect("root");
        files.write(&path("state"), b"new").expect("write");
        assert_eq!(std::fs::read(root.join("state")).expect("state"), b"new");
        assert_eq!(std::fs::read(torn).expect("unowned temporary"), b"torn");
    }

    #[test]
    fn mutation_parent_replacement_is_rejected_before_opening_authority() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let parent = temporary.path().join("parent");
        let displaced = temporary.path().join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&parent).expect("parent");
        std::fs::create_dir_all(&outside).expect("outside");
        let target = parent.join("entry");

        let result = super::pin_entry_with(&target, || {
            std::fs::rename(&parent, &displaced)?;
            std::os::unix::fs::symlink(&outside, &parent)?;
            Ok(())
        });

        assert!(result.is_err());
        assert!(std::fs::read_dir(&outside).expect("outside listing").next().is_none());
    }

    #[test]
    fn final_symlinks_are_never_followed_by_mutations() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(&outside, b"outside").expect("outside");
        std::os::unix::fs::symlink(&outside, root.join("remove-link")).expect("remove link");
        std::os::unix::fs::symlink(&outside, root.join("rename-link")).expect("rename link");
        std::os::unix::fs::symlink(&outside, root.join("mkdir-link")).expect("mkdir link");
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert!(files.mkdir(&path("mkdir-link")).is_err());
        files.remove(&path("remove-link")).expect("remove link itself");
        files
            .rename(&path("rename-link"), &path("renamed-link"))
            .expect("rename link itself");

        assert_eq!(std::fs::read(&outside).expect("outside"), b"outside");
        assert!(!root.join("remove-link").exists());
        assert!(
            std::fs::symlink_metadata(root.join("renamed-link"))
                .expect("renamed link")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn concurrent_renames_never_overwrite_the_winner() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("first"), b"first").expect("first");
        std::fs::write(root.join("second"), b"second").expect("second");
        let files = std::sync::Arc::new(WorkspaceDirectory::new(&root).expect("root"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let threads = ["first", "second"].map(|source| {
            let files = std::sync::Arc::clone(&files);
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                files.rename(&path(source), &path("winner"))
            })
        });
        barrier.wait();
        let results = threads.map(|thread| thread.join().expect("renamer"));

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(HostError::Conflict(_))))
                .count(),
            1
        );
        let winner = std::fs::read(root.join("winner")).expect("winner");
        assert!(winner == b"first" || winner == b"second");
        assert_eq!(
            [root.join("first"), root.join("second")]
                .into_iter()
                .filter(|source| source.exists())
                .count(),
            1
        );
    }

    #[test]
    fn opened_file_authority_survives_a_final_symlink_swap() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        let target = root.join("state");
        let outside = temporary.path().join("outside");
        std::fs::write(&target, b"original").expect("original");
        std::fs::write(&outside, b"secret material").expect("outside");
        let files = WorkspaceDirectory::new(&root).expect("root");

        let opened = files.opened(&path("state")).expect("open authority");
        let metadata = opened.metadata().expect("opened metadata");
        std::fs::remove_file(&target).expect("remove name");
        std::os::unix::fs::symlink(&outside, &target).expect("raced link");

        assert_eq!(super::read_bounded(opened).expect("opened bytes"), b"original");
        assert_eq!(metadata.len(), 8);
        assert_ne!(metadata.len(), std::fs::metadata(&target).expect("later path").len());
    }

    #[test]
    fn opened_directory_authority_survives_an_ancestor_swap() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let listing = root.join("listing");
        let displaced = root.join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&listing).expect("listing");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(listing.join("public"), b"public").expect("public");
        std::fs::write(outside.join("secret"), b"secret").expect("secret");
        let files = WorkspaceDirectory::new(&root).expect("root");

        let directory = files.directory(&path("listing")).expect("directory authority");
        std::fs::rename(&listing, &displaced).expect("displace");
        std::os::unix::fs::symlink(&outside, &listing).expect("raced ancestor");
        let names = rustix::fs::Dir::read_from(&directory)
            .expect("pinned listing")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "public"));
        assert!(!names.iter().any(|name| name == "secret"));
    }

    #[test]
    fn replaced_workspace_root_never_redirects_read_authority() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let displaced = temporary.path().join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(root.join("state"), b"public").expect("public");
        std::fs::write(outside.join("state"), b"secret").expect("secret");
        let files = WorkspaceDirectory::new(&root).expect("root");
        std::fs::rename(&root, &displaced).expect("displace root");
        std::os::unix::fs::symlink(&outside, &root).expect("replace root");

        assert!(files.read(&path("state")).is_err());
        assert_eq!(std::fs::read(outside.join("state")).expect("outside"), b"secret");
    }

    #[test]
    fn reads_and_component_walks_are_bounded_before_reply_encoding() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("large"), vec![0; super::READ_BYTES_LIMIT + 1]).expect("large file");
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert!(matches!(files.read(&path("large")), Err(HostError::Failed(_))));
        let deep = std::iter::repeat_n("x", super::PATH_DEPTH_LIMIT + 1)
            .collect::<Vec<_>>()
            .join("/");
        assert!(matches!(files.read(&path(&deep)), Err(HostError::Conflict(_))));
    }

    #[test]
    fn directory_listing_has_a_hard_entry_bound() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let listing = root.join("listing");
        std::fs::create_dir_all(&listing).expect("listing");
        for index in 0..=super::LIST_ENTRIES_LIMIT {
            std::fs::write(listing.join(index.to_string()), []).expect("entry");
        }
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert!(matches!(files.list(&path("listing")), Err(HostError::Failed(_))));
        let first = files
            .list_page(&path("listing"), None, None, 3)
            .expect("first bounded page");
        assert_eq!(first.entries.len(), 3);
        assert!(first.more);
        std::fs::write(listing.join("inserted-after-first-page"), []).expect("concurrent insert");
        assert!(matches!(
            files.list_page(&path("listing"), first.next.as_ref(), Some(&first.identity), 3),
            Err(HostError::Conflict(_))
        ));
        let refreshed = files
            .list_page(&path("listing"), None, None, 3)
            .expect("restart after mutation");
        let second = files
            .list_page(&path("listing"), refreshed.next.as_ref(), Some(&refreshed.identity), 3)
            .expect("following bounded page");
        assert_eq!(second.entries.len(), 3);
        assert!(second.entries[0].path > refreshed.entries[2].path);
    }

    #[test]
    fn a_missing_file_is_absent_rather_than_a_failure() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let files = WorkspaceDirectory::new(temporary.path().join("workspace")).expect("root");
        assert!(matches!(files.read(&path("nothing.txt")), Err(HostError::Absent(_))));
        assert!(matches!(files.list(&path("nowhere")), Err(HostError::Absent(_))));
    }

    #[test]
    fn a_root_reached_through_a_link_still_accepts_its_own_files() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let real = temporary.path().join("real");
        std::fs::create_dir_all(&real).expect("directory");
        std::fs::write(real.join("app.log"), b"hello").expect("file");
        let linked = temporary.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).expect("link");

        let files = WorkspaceDirectory::new(&linked).expect("root");
        assert_eq!(files.read(&path("app.log")).expect("contents"), b"hello");
    }
}
