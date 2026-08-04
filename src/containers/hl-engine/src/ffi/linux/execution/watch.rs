use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex, Weak};

use hl_event::{
    EventResourceKey, InotifyMask, WatchBinding, WatchNodeIdentity, WatchPathIdentity, WatchRequest, WatchSource,
    WatchSourceError, WatchSourceEvent, WatchSourceObserver, WatchSourceSubscription,
};

const WATCH_LIMIT: usize = 8_192;

pub(super) struct Hub {
    root: PathBuf,
    sources: Mutex<Vec<Weak<Source>>>,
    next_source: AtomicU64,
    next_path: AtomicU64,
    next_cookie: AtomicU32,
    paths: Mutex<BTreeMap<WatchPathIdentity, PathBuf>>,
    dnotify: Mutex<Vec<Weak<DnotifyEntry>>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sources = self.sources.lock().unwrap_or_else(|error| error.into_inner());
        formatter.debug_struct("Hub").field("sources", &sources.len()).finish()
    }
}

impl Hub {
    pub(super) fn projected(root: &[u8]) -> Result<Arc<Self>, std::io::Error> {
        if !root.starts_with(b"/") || root.contains(&0) || root.len() > 4096 {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        Ok(Arc::new(Self {
            root: PathBuf::from(std::ffi::OsString::from_vec(root.to_vec())),
            sources: Mutex::new(Vec::new()),
            next_source: AtomicU64::new(1),
            next_path: AtomicU64::new(1),
            next_cookie: AtomicU32::new(1),
            paths: Mutex::new(BTreeMap::new()),
            dnotify: Mutex::new(Vec::new()),
        }))
    }

    pub(super) fn new(root: &[u8]) -> Result<Arc<Self>, std::io::Error> {
        let root = PathBuf::from(std::ffi::OsString::from_vec(root.to_vec())).canonicalize()?;
        Ok(Arc::new(Self {
            root,
            sources: Mutex::new(Vec::new()),
            next_source: AtomicU64::new(1),
            next_path: AtomicU64::new(1),
            next_cookie: AtomicU32::new(1),
            paths: Mutex::new(BTreeMap::new()),
            dnotify: Mutex::new(Vec::new()),
        }))
    }

    pub(super) fn publish(&self, path: &Path, mask: u32) {
        self.publish_dnotify(path, mask);
        let sources = {
            let mut state = self.sources.lock().unwrap_or_else(|error| error.into_inner());
            state.retain(|source| source.strong_count() != 0);
            state.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        for source in sources {
            source.publish(path, mask);
        }
    }

    pub(super) fn publish_child(&self, parent: &Path, name: &[u8], mask: u32) {
        self.publish_child_cookie(parent, name, mask, 0);
    }

    fn publish_child_cookie(&self, parent: &Path, name: &[u8], mask: u32, cookie: u32) {
        self.publish_dnotify(parent, mask);
        let sources = {
            let mut state = self.sources.lock().unwrap_or_else(|error| error.into_inner());
            state.retain(|source| source.strong_count() != 0);
            state.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        for source in sources {
            source.publish_child(parent, name, mask, cookie);
        }
    }

    pub(super) fn publish_move(&self, from: &Path, to: &Path) {
        let mut cookie = self.next_cookie.fetch_add(1, Ordering::Relaxed);
        if cookie == 0 {
            cookie = self.next_cookie.fetch_add(1, Ordering::Relaxed);
        }
        if let (Some(parent), Some(name)) = (from.parent(), from.file_name()) {
            self.publish_child_cookie(parent, name.as_encoded_bytes(), InotifyMask::MOVED_FROM, cookie);
        }
        if let (Some(parent), Some(name)) = (to.parent(), to.file_name()) {
            self.publish_child_cookie(parent, name.as_encoded_bytes(), InotifyMask::MOVED_TO, cookie);
        }
    }

    fn source(self: &Arc<Self>) -> Arc<Source> {
        let source = Arc::new(Source {
            hub: Arc::clone(self),
            resource: EventResourceKey::new(0x3000_0000_0000_0000 | self.next_source.fetch_add(1, Ordering::Relaxed))
                .expect("watch source identities are nonzero"),
            state: Mutex::new(SourceState::default()),
        });
        self.sources
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(Arc::downgrade(&source));
        source
    }

    pub(super) fn subscribe_dnotify(
        &self,
        path: PathBuf,
        mask: u32,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn hl_descriptor::ReadinessSubscription> {
        let entry = Arc::new(DnotifyEntry {
            path,
            mask,
            active: AtomicBool::new(true),
            callback,
        });
        self.dnotify
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(Arc::downgrade(&entry));
        Box::new(DnotifySubscription(entry))
    }

    fn publish_dnotify(&self, path: &Path, event: u32) {
        let entries = {
            let mut entries = self.dnotify.lock().unwrap_or_else(|error| error.into_inner());
            entries.retain(|entry| entry.strong_count() != 0);
            entries.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        for entry in entries {
            entry.publish(path, event);
        }
    }

    fn rooted(&self, bytes: &[u8]) -> Result<PathBuf, WatchSourceError> {
        if bytes.len() > 4_096 {
            return Err(WatchSourceError::NameTooLong);
        }
        let path = Path::new(std::ffi::OsStr::from_bytes(bytes));
        if path.components().any(|part| matches!(part, Component::ParentDir)) {
            return Err(WatchSourceError::PermissionDenied);
        }
        let relative = path.strip_prefix("/").unwrap_or(path);
        let resolved = self
            .root
            .join(relative)
            .canonicalize()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => WatchSourceError::NotFound,
                std::io::ErrorKind::PermissionDenied => WatchSourceError::PermissionDenied,
                _ => WatchSourceError::Failed,
            })?;
        if !resolved.starts_with(&self.root) {
            return Err(WatchSourceError::PermissionDenied);
        }
        Ok(resolved)
    }
}

struct DnotifyEntry {
    path: PathBuf,
    mask: u32,
    active: AtomicBool,
    callback: Arc<dyn Fn() + Send + Sync>,
}

impl DnotifyEntry {
    fn publish(&self, path: &Path, event: u32) {
        const MULTISHOT: u32 = 0x8000_0000;
        if self.path != path || self.mask & Self::event_mask(event) == 0 {
            return;
        }
        if self.mask & MULTISHOT == 0 {
            if !self.active.swap(false, Ordering::AcqRel) {
                return;
            }
        } else if !self.active.load(Ordering::Acquire) {
            return;
        }
        (self.callback)();
    }

    fn event_mask(event: u32) -> u32 {
        (if event & InotifyMask::ACCESS != 0 { 1 } else { 0 })
            | (if event & InotifyMask::MODIFY != 0 { 2 } else { 0 })
            | (if event & InotifyMask::CREATE != 0 { 4 } else { 0 })
            | (if event & (InotifyMask::DELETE | InotifyMask::DELETE_SELF) != 0 {
                8
            } else {
                0
            })
            | (if event & (InotifyMask::MOVED_FROM | InotifyMask::MOVED_TO | InotifyMask::MOVE_SELF) != 0 {
                16
            } else {
                0
            })
            | (if event & InotifyMask::ATTRIB != 0 { 32 } else { 0 })
    }
}

struct DnotifySubscription(Arc<DnotifyEntry>);

impl hl_descriptor::ReadinessSubscription for DnotifySubscription {
    fn quiesce(&self) {
        self.0.active.store(false, Ordering::Release);
    }
}

impl Drop for DnotifySubscription {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Release);
    }
}

struct Entry {
    path: PathBuf,
    mask: InotifyMask,
}

#[derive(Default)]
struct SourceState {
    watches: BTreeMap<u64, Entry>,
    observer: Option<(Arc<dyn WatchSourceObserver>, Arc<AtomicBool>)>,
}

struct Source {
    hub: Arc<Hub>,
    resource: EventResourceKey,
    state: Mutex<SourceState>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        formatter
            .debug_struct("Source")
            .field("watches", &state.watches.len())
            .finish()
    }
}

impl Source {
    fn publish(&self, path: &Path, mask: u32) {
        let (observer, active, events) = {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let Some((observer, active)) = &state.observer else {
                return;
            };
            let events = state
                .watches
                .iter()
                .filter_map(|(token, entry)| {
                    (entry.path == path && entry.mask.contains(mask)).then_some(WatchSourceEvent {
                        source_token: *token,
                        mask: InotifyMask::from_bits(mask),
                        cookie: 0,
                        name: Vec::new(),
                        unlinked_child: false,
                    })
                })
                .collect::<Vec<_>>();
            (Arc::clone(observer), Arc::clone(active), events)
        };
        if !active.load(Ordering::Acquire) {
            return;
        }
        events
            .into_iter()
            .take_while(|_| active.load(Ordering::Acquire))
            .for_each(|event| observer.watch_event(event));
    }

    fn publish_child(&self, parent: &Path, name: &[u8], mask: u32, cookie: u32) {
        let (observer, active, events) = {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let Some((observer, active)) = &state.observer else {
                return;
            };
            let events = state
                .watches
                .iter()
                .filter_map(|(token, entry)| {
                    (entry.path == parent && entry.mask.contains(mask)).then_some(WatchSourceEvent {
                        source_token: *token,
                        mask: InotifyMask::from_bits(mask),
                        cookie,
                        name: name.to_vec(),
                        unlinked_child: false,
                    })
                })
                .collect::<Vec<_>>();
            (Arc::clone(observer), Arc::clone(active), events)
        };
        if !active.load(Ordering::Acquire) {
            return;
        }
        events
            .into_iter()
            .take_while(|_| active.load(Ordering::Acquire))
            .for_each(|event| observer.watch_event(event));
    }
}

impl WatchSource for Source {
    fn resolve(&self, request: WatchRequest<'_>) -> Result<WatchBinding, WatchSourceError> {
        let path = self.hub.rooted(request.path)?;
        let metadata = std::fs::metadata(&path).map_err(|_| WatchSourceError::NotFound)?;
        if request.mask.contains(InotifyMask::ONLY_DIRECTORY) && !metadata.is_dir() {
            return Err(WatchSourceError::NotDirectory);
        }
        let identity = WatchPathIdentity(self.hub.next_path.fetch_add(1, Ordering::Relaxed));
        if identity.0 == 0 {
            return Err(WatchSourceError::ResourceLimit);
        }
        self.hub
            .paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(identity, path);
        Ok(WatchBinding {
            node: WatchNodeIdentity {
                device: metadata.dev(),
                object: metadata.ino(),
            },
            path: identity,
            is_directory: metadata.is_dir(),
        })
    }

    fn add(&self, binding: WatchBinding, token: u64, mask: InotifyMask) -> Result<(), WatchSourceError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.watches.len() >= WATCH_LIMIT {
            return Err(WatchSourceError::ResourceLimit);
        }
        let path = self
            .hub
            .paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&binding.path)
            .cloned()
            .ok_or(WatchSourceError::NotFound)?;
        state.watches.insert(token, Entry { path, mask });
        Ok(())
    }

    fn modify(&self, token: u64, mask: InotifyMask) -> Result<(), WatchSourceError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.watches.get_mut(&token).ok_or(WatchSourceError::NotFound)?.mask = mask;
        Ok(())
    }

    fn remove(&self, token: u64) -> Result<(), WatchSourceError> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .watches
            .remove(&token)
            .map(|_| ())
            .ok_or(WatchSourceError::NotFound)
    }

    fn subscribe(
        &self,
        observer: Arc<dyn WatchSourceObserver>,
    ) -> Result<Box<dyn WatchSourceSubscription>, WatchSourceError> {
        let active = Arc::new(AtomicBool::new(true));
        self.state.lock().unwrap_or_else(|error| error.into_inner()).observer = Some((observer, Arc::clone(&active)));
        Ok(Box::new(Subscription(active)))
    }

    fn checkpoint_clone(&self) -> Result<Arc<dyn WatchSource>, WatchSourceError> {
        Ok(self.hub.source())
    }
}

struct Subscription(Arc<AtomicBool>);

impl WatchSourceSubscription for Subscription {
    fn quiesce(&self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.quiesce();
    }
}

#[derive(Debug)]
pub(super) struct Provider(pub(super) Arc<Hub>);

impl hl_runtime::WatchEventSource for Provider {
    fn watches(&self) -> Result<(EventResourceKey, Arc<dyn WatchSource>), hl_runtime::EventSourceError> {
        let source = self.0.source();
        Ok((source.resource, source))
    }
}
