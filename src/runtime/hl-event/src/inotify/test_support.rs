use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    Inotify, InotifyLimits, InotifyMask, WatchBinding, WatchNodeIdentity, WatchPathIdentity, WatchRequest, WatchSource,
    WatchSourceError, WatchSourceEvent, WatchSourceObserver, WatchSourceSubscription,
};

#[derive(Clone)]
struct Node {
    binding: WatchBinding,
}

#[derive(Default)]
struct State {
    nodes: BTreeMap<Vec<u8>, Node>,
    masks: BTreeMap<u64, InotifyMask>,
    observer: Option<(Arc<dyn WatchSourceObserver>, Arc<AtomicBool>)>,
    removes: Vec<u64>,
}

#[derive(Default)]
pub(crate) struct Source {
    state: Mutex<State>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        formatter
            .debug_struct("Source")
            .field("nodes", &state.nodes.len())
            .field("watches", &state.masks.len())
            .finish()
    }
}

impl Source {
    fn add_node(&self, path: &[u8], object: u64, is_directory: bool) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .nodes
            .insert(
                path.to_vec(),
                Node {
                    binding: WatchBinding {
                        node: WatchNodeIdentity { device: 1, object },
                        path: WatchPathIdentity(object + 100),
                        is_directory,
                    },
                },
            );
    }

    pub(crate) fn emit(&self, token: u64, mask: u32, cookie: u32, name: &[u8], unlinked_child: bool) {
        let observer = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .observer
            .clone();
        if let Some((observer, active)) = observer
            && active.load(Ordering::SeqCst)
        {
            observer.watch_event(WatchSourceEvent {
                source_token: token,
                mask: InotifyMask::from_bits(mask),
                cookie,
                name: name.to_vec(),
                unlinked_child,
            });
        }
    }

    pub(crate) fn token(&self) -> u64 {
        *self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .masks
            .keys()
            .next()
            .unwrap()
    }

    pub(crate) fn mask(&self, token: u64) -> InotifyMask {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).masks[&token]
    }

    pub(crate) fn removes(&self) -> Vec<u64> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .removes
            .clone()
    }
}

struct Subscription {
    active: Arc<AtomicBool>,
}

impl WatchSourceSubscription for Subscription {
    fn quiesce(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

impl WatchSource for Source {
    fn resolve(&self, request: WatchRequest<'_>) -> Result<WatchBinding, WatchSourceError> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let node = state.nodes.get(request.path).ok_or(WatchSourceError::NotFound)?;
        if request.mask.contains(InotifyMask::ONLY_DIRECTORY) && !node.binding.is_directory {
            return Err(WatchSourceError::NotDirectory);
        }
        Ok(node.binding)
    }

    fn add(&self, _binding: WatchBinding, token: u64, mask: InotifyMask) -> Result<(), WatchSourceError> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .masks
            .insert(token, mask);
        Ok(())
    }

    fn modify(&self, token: u64, mask: InotifyMask) -> Result<(), WatchSourceError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        *state.masks.get_mut(&token).ok_or(WatchSourceError::NotFound)? = mask;
        Ok(())
    }

    fn remove(&self, token: u64) -> Result<(), WatchSourceError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.removes.push(token);
        state.masks.remove(&token).map(|_| ()).ok_or(WatchSourceError::NotFound)
    }

    fn subscribe(
        &self,
        observer: Arc<dyn WatchSourceObserver>,
    ) -> Result<Box<dyn WatchSourceSubscription>, WatchSourceError> {
        let active = Arc::new(AtomicBool::new(true));
        self.state.lock().unwrap_or_else(|error| error.into_inner()).observer = Some((observer, active.clone()));
        Ok(Box::new(Subscription { active }))
    }
}

pub(crate) struct Fixture {
    pub(crate) source: Arc<Source>,
    pub(crate) inotify: Arc<Inotify>,
}

impl Fixture {
    pub(crate) fn new(nonblocking: bool) -> Self {
        Self::with_limits(nonblocking, InotifyLimits::default())
    }

    pub(crate) fn with_limits(nonblocking: bool, limits: InotifyLimits) -> Self {
        let source = Arc::new(Source::default());
        source.add_node(b"/file", 1, false);
        source.add_node(b"/alias", 1, false);
        source.add_node(b"/dir", 2, true);
        let inotify = Arc::new(Inotify::new(nonblocking, limits, source.clone()).unwrap());
        Self { source, inotify }
    }

    pub(crate) fn watch(&self, path: &[u8], bits: u32) -> i32 {
        self.inotify.add_watch(path, InotifyMask::from_bits(bits)).unwrap()
    }

    pub(crate) fn emit(&self, mask: u32, name: &[u8]) {
        self.source.emit(self.source.token(), mask, 0, name, false);
    }

    pub(crate) fn read_all(&self) -> Vec<u8> {
        let mut output = vec![0_u8; 1_024];
        let size = self.inotify.read(&mut output).unwrap();
        output.truncate(size);
        output
    }

    pub(crate) fn i32_at(bytes: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    pub(crate) fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    pub(crate) fn record_size(bytes: &[u8], offset: usize) -> usize {
        16 + usize::try_from(Self::u32_at(bytes, offset + 12)).unwrap()
    }
}
