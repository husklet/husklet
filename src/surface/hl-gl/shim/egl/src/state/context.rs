use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hl_gl::model::context::{ContextState, IrAllocator};

use super::group::GroupSlot;
use super::{ContextAttributes, EglSync};
use hl_gl::service::config;

/// One depth/stencil size of an advertised config, falling back to the primary config for a handle this
/// driver never issued (`EGL_NO_CONFIG_KHR`).
fn config_attrib(id: i32, attribute: i32) -> i32 {
    config::Config::attrib_of(id, attribute)
        .or_else(|_| config::Config::attrib_of(config::CONFIG_ID, attribute))
        .unwrap_or(0)
}

struct Context {
    attributes: ContextAttributes,
    group: Arc<GroupSlot>,
}

pub(super) struct Prepared {
    pub(super) group: Arc<GroupSlot>,
    pub(super) state: ContextState,
}

pub(super) struct Retire {
    pub(super) group: Arc<GroupSlot>,
    pub(super) token: usize,
    pub(super) final_context: bool,
}

pub(super) struct Contexts {
    contexts: HashMap<usize, Context>,
    current: HashMap<usize, usize>,
    pending_destroy: HashSet<usize>,
    syncs: HashMap<usize, EglSync>,
    allocator: Arc<IrAllocator>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BindError {
    Missing,
    Current,
}

impl Contexts {
    pub(super) fn new(allocator: Arc<IrAllocator>) -> Self {
        Self {
            contexts: HashMap::new(),
            current: HashMap::new(),
            pending_destroy: HashSet::new(),
            syncs: HashMap::new(),
            allocator,
        }
    }

    pub(super) fn prepare(
        &self,
        attributes: ContextAttributes,
        share: Option<usize>,
    ) -> Option<Prepared> {
        let group = match share {
            Some(token) => Arc::clone(&self.contexts.get(&token)?.group),
            None => GroupSlot::new(Arc::clone(&self.allocator)),
        };
        Some(Prepared {
            group,
            state: ContextState::with_version(
                attributes.client_version,
                attributes.minor_version,
                attributes.no_error,
            )
            .with_debug(attributes.debug)
            .on_config(
                config_attrib(attributes.config_id, config::EGL_DEPTH_SIZE),
                config_attrib(attributes.config_id, config::EGL_STENCIL_SIZE),
            ),
        })
    }

    pub(super) fn commit(
        &mut self,
        token: usize,
        attributes: ContextAttributes,
        group: Arc<GroupSlot>,
    ) -> bool {
        if self.contexts.contains_key(&token) {
            return false;
        }
        self.contexts.insert(token, Context { attributes, group });
        true
    }

    pub(super) fn attributes(&self, token: usize) -> Option<ContextAttributes> {
        self.contexts.get(&token).map(|context| context.attributes)
    }

    pub(super) fn group(&self, token: usize) -> Option<Arc<GroupSlot>> {
        self.contexts
            .get(&token)
            .map(|context| Arc::clone(&context.group))
    }

    pub(super) fn groups(&self) -> Vec<Arc<GroupSlot>> {
        let mut groups = Vec::new();
        for context in self.contexts.values() {
            if groups
                .iter()
                .any(|group| Arc::ptr_eq(group, &context.group))
            {
                continue;
            }
            groups.push(Arc::clone(&context.group));
        }
        groups
    }

    pub(super) fn shares(&self, token: usize, group: &Arc<GroupSlot>) -> bool {
        self.contexts
            .get(&token)
            .is_some_and(|context| Arc::ptr_eq(&context.group, group))
    }

    pub(super) fn contains(&self, token: usize) -> bool {
        self.contexts.contains_key(&token) && !self.pending_destroy.contains(&token)
    }

    pub(super) fn is_current(&self, token: usize) -> bool {
        self.current.get(&token).copied().unwrap_or(0) != 0
    }

    pub(super) fn bind(
        &mut self,
        previous: usize,
        next: usize,
    ) -> Result<Option<Retire>, BindError> {
        if previous == next {
            return Ok(None);
        }
        if next != 0 {
            if !self.contains(next) {
                return Err(BindError::Missing);
            }
            if self.is_current(next) {
                return Err(BindError::Current);
            }
        }
        let mut retire = None;
        if previous != 0 {
            if let Some(count) = self.current.get_mut(&previous) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.current.remove(&previous);
                    if self.pending_destroy.remove(&previous) {
                        retire = self.remove(previous);
                    }
                }
            }
        }
        if next != 0 {
            *self.current.entry(next).or_default() += 1;
        }
        Ok(retire)
    }

    pub(super) fn create_sync(&mut self, token: usize, sync: EglSync) {
        self.syncs.insert(token, sync);
    }

    pub(super) fn sync(&self, token: usize) -> Option<EglSync> {
        self.syncs.get(&token).copied()
    }

    pub(super) fn destroy_sync(&mut self, token: usize) -> Option<EglSync> {
        self.syncs.remove(&token)
    }

    pub(super) fn destroy(&mut self, token: usize) -> Option<Retire> {
        if self.current.get(&token).copied().unwrap_or(0) != 0 {
            self.pending_destroy.insert(token);
            return None;
        }
        self.remove(token)
    }

    fn remove(&mut self, token: usize) -> Option<Retire> {
        let context = self.contexts.remove(&token)?;
        self.syncs.retain(|_, sync| sync.context != token);
        let final_context = !self
            .contexts
            .values()
            .any(|other| Arc::ptr_eq(&other.group, &context.group));
        Some(Retire {
            group: context.group,
            token,
            final_context,
        })
    }

    pub(super) fn live(&self) -> u32 {
        self.contexts.len().try_into().unwrap_or(u32::MAX)
    }
}
