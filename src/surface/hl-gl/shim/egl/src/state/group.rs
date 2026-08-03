use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};

use hl_gl::model::context::{ContextState, GlContext, IrAllocator};
use hl_gl::model::texture::SharedPixels;

use super::ImportedImage;
use crate::image::{Image, StorageKey};

#[derive(Clone)]
struct LinearWrite {
    image: Arc<Image>,
    shared: Arc<SharedPixels>,
    version: u64,
}

pub(crate) struct GroupData {
    pub(crate) gl: GlContext,
    pub(crate) images: HashMap<u32, ImportedImage>,
    linear_storages: HashMap<StorageKey, std::sync::Weak<SharedPixels>>,
    pending_linear: HashMap<StorageKey, LinearWrite>,
    contexts: HashMap<usize, ContextState>,
    active: usize,
}

impl GroupData {
    pub(super) fn new(allocator: Arc<IrAllocator>) -> Self {
        Self {
            gl: GlContext::with_allocator(allocator),
            images: HashMap::new(),
            linear_storages: HashMap::new(),
            pending_linear: HashMap::new(),
            contexts: HashMap::new(),
            active: 0,
        }
    }

    pub(super) fn add(&mut self, token: usize, state: ContextState) -> bool {
        if self.contexts.contains_key(&token) {
            return false;
        }
        self.contexts.insert(token, state);
        true
    }

    pub(super) fn activate(&mut self, token: usize) -> bool {
        if self.active == token {
            return token == 0 || self.contexts.contains_key(&token);
        }
        if token != 0 && !self.contexts.contains_key(&token) {
            return false;
        }
        if self.active != 0 {
            let state = self
                .contexts
                .get_mut(&self.active)
                .expect("active context belongs to its group");
            self.gl.switch_state(state);
        }
        self.active = 0;
        if token != 0 {
            let state = self
                .contexts
                .get_mut(&token)
                .expect("validated context belongs to its group");
            self.gl.switch_state(state);
            self.active = token;
        }
        true
    }

    pub(super) fn remove(&mut self, token: usize) -> bool {
        if self.active == token {
            self.activate(0);
        }
        let Some(mut state) = self.contexts.remove(&token) else {
            return false;
        };
        self.gl.retire_state(&mut state);
        true
    }

    pub(super) fn references_buffer(&self, name: u32) -> bool {
        self.gl.references_buffer(name)
            || self
                .contexts
                .values()
                .any(|context| context.references_buffer(name))
    }

    pub(super) fn references_texture(&self, name: u32) -> bool {
        self.gl.references_texture(name)
            || self
                .contexts
                .values()
                .any(|context| context.references_texture(name))
    }

    pub(super) fn references_sampler(&self, name: u32) -> bool {
        self.gl.references_sampler(name)
            || self
                .contexts
                .values()
                .any(|context| context.references_sampler(name))
    }

    pub(super) fn references_program(&self, name: u32) -> bool {
        self.gl.references_program(name)
            || self
                .contexts
                .values()
                .any(|context| context.references_program(name))
    }

    pub(super) fn retire(&mut self) {
        self.activate(0);
        for (_, mut state) in self.contexts.drain() {
            self.gl.retire_state(&mut state);
        }
        self.gl.retire_all();
        self.images.clear();
        self.linear_storages.clear();
        self.pending_linear.clear();
    }

    pub(crate) fn linear_storage(
        &mut self,
        image: &Arc<Image>,
        pixels: Arc<Vec<u8>>,
    ) -> hl_gpu::Result<Arc<SharedPixels>> {
        if image.external_token().is_some() {
            return Err(hl_gpu::GpuError::Invalid(
                "external images do not use CPU shared storage",
            ));
        }
        let key = image
            .storage_key()
            .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
        self.linear_storages
            .retain(|_, storage| storage.strong_count() > 0);
        if let Some(storage) = self
            .linear_storages
            .get(&key)
            .and_then(std::sync::Weak::upgrade)
        {
            if storage.load().as_slice() != pixels.as_slice() {
                storage.store(pixels);
            }
            return Ok(storage);
        }
        let storage = Arc::new(SharedPixels::new(pixels));
        self.linear_storages.insert(key, Arc::downgrade(&storage));
        Ok(storage)
    }

    pub(crate) fn flush_dirty_images(
        &mut self,
        captured: &std::collections::HashSet<u32>,
    ) -> hl_gpu::Result<usize> {
        let captured_storage = captured
            .iter()
            .filter_map(|name| self.images.get(name))
            .filter_map(|binding| binding.image.storage_key().ok())
            .collect::<std::collections::HashSet<_>>();
        let keys = self.pending_linear.keys().copied().collect::<Vec<_>>();
        let mut flushed = 0;
        let mut first_error = None;
        for key in keys {
            if captured_storage.contains(&key) {
                continue;
            }
            let Some(pending) = self.pending_linear.get(&key).cloned() else {
                continue;
            };
            let snapshot = pending.shared.snapshot();
            match pending.image.write_native_bgra(&snapshot.data) {
                Ok(()) => {
                    if self
                        .pending_linear
                        .get(&key)
                        .is_some_and(|current| current.version <= snapshot.version)
                    {
                        self.pending_linear.remove(&key);
                    }
                    flushed += 1;
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| hl_gpu::GpuError::Decode(error.to_string()));
                }
            }
        }
        first_error.map_or(Ok(flushed), Err)
    }

    pub(crate) fn mark_linear_dirty(&mut self, name: u32) -> bool {
        let Some(texture) = self.gl.textures.get(name) else {
            return false;
        };
        let Some(binding) = self.images.get_mut(&name) else {
            return false;
        };
        let Some(shared) = binding.shared.clone() else {
            return false;
        };
        if texture.ir_format != hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm {
            return false;
        }
        binding.generation = texture.gen;
        let Ok(key) = binding.image.storage_key() else {
            return false;
        };
        let version = shared.version();
        self.pending_linear
            .entry(key)
            .and_modify(|pending| {
                if version >= pending.version {
                    *pending = LinearWrite {
                        image: Arc::clone(&binding.image),
                        shared: Arc::clone(&shared),
                        version,
                    };
                }
            })
            .or_insert_with(|| LinearWrite {
                image: Arc::clone(&binding.image),
                shared,
                version,
            });
        true
    }

    pub(crate) fn redefine_texture(&mut self, define: impl FnOnce(&mut GlContext)) {
        let texture = self.gl.bound_texture();
        let generation = self.gl.textures.get(texture).map(|texture| texture.gen);
        define(&mut self.gl);
        if generation != self.gl.textures.get(texture).map(|texture| texture.gen) {
            if let Some(generation) = generation {
                self.gl.retire_texture_generation(texture, generation);
            }
            self.images.remove(&texture);
        }
    }

    pub(crate) fn redefine_renderbuffer(&mut self, define: impl FnOnce(&mut GlContext)) {
        let renderbuffer = self.gl.bound_renderbuffer();
        let texture = self.gl.renderbuffers.backing_tex(renderbuffer);
        let generation = self.gl.textures.get(texture).map(|texture| texture.gen);
        define(&mut self.gl);
        let texture = self.gl.renderbuffers.backing_tex(renderbuffer);
        if generation != self.gl.textures.get(texture).map(|texture| texture.gen) {
            if let Some(generation) = generation {
                self.gl.retire_texture_generation(texture, generation);
            }
            self.images.remove(&texture);
        }
    }

    pub(crate) fn delete_texture(&mut self, texture: u32) {
        self.gl.delete_texture_later(texture);
    }

    pub(crate) fn delete_buffer(&mut self, buffer: u32) {
        self.gl.delete_buffer_later(buffer);
    }

    pub(crate) fn delete_sampler(&mut self, sampler: u32) {
        self.gl.delete_sampler_later(sampler);
    }

    pub(crate) fn delete_program(&mut self, program: u32) {
        self.gl.delete_program_later(program);
    }

    pub(crate) fn delete_renderbuffer(&mut self, renderbuffer: u32) {
        let texture = self.gl.renderbuffers.backing_tex(renderbuffer);
        if self.gl.delete_renderbuffer(renderbuffer) {
            self.images.remove(&texture);
        }
    }

    pub(super) fn collect_deleted_objects(&mut self) {
        let textures = self.gl.deleted_textures().collect::<Vec<_>>();
        for name in textures {
            if !self.references_texture(name) && self.gl.reclaim_texture(name) {
                self.images.remove(&name);
            }
        }
        let buffers = self.gl.deleted_buffers().collect::<Vec<_>>();
        for name in buffers {
            if !self.references_buffer(name) {
                self.gl.reclaim_buffer(name);
            }
        }
        let samplers = self.gl.deleted_samplers().collect::<Vec<_>>();
        for name in samplers {
            if !self.references_sampler(name) {
                self.gl.reclaim_sampler(name);
            }
        }
        let programs = self.gl.deleted_programs().collect::<Vec<_>>();
        for name in programs {
            if !self.references_program(name) {
                self.gl.reclaim_program(name);
            }
        }
    }
}

enum Status {
    Ready(GroupData),
    Busy,
    Lost(Arc<str>),
}

struct Slot {
    generation: u64,
    status: Status,
    /// Whether the one `GL_CONTEXT_LOST` this loss owes the application has been handed out yet.
    lost_reported: bool,
}

pub(super) struct GroupSlot {
    state: Mutex<Slot>,
    changed: Condvar,
}

impl GroupSlot {
    pub(super) fn new(allocator: Arc<IrAllocator>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Slot {
                generation: 1,
                status: Status::Ready(GroupData::new(allocator)),
                lost_reported: false,
            }),
            changed: Condvar::new(),
        })
    }

    pub(super) fn acquire(self: &Arc<Self>) -> Result<GroupLease, GroupError> {
        let mut slot = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            let status = std::mem::replace(&mut slot.status, Status::Busy);
            match status {
                Status::Ready(data) => {
                    return Ok(GroupLease {
                        slot: Arc::clone(self),
                        generation: slot.generation,
                        data: Some(data),
                    });
                }
                Status::Busy => {
                    slot.status = Status::Busy;
                    slot = self
                        .changed
                        .wait(slot)
                        .unwrap_or_else(|error| error.into_inner());
                }
                Status::Lost(error) => {
                    slot.status = Status::Lost(Arc::clone(&error));
                    return Err(GroupError(error));
                }
            }
        }
    }

    pub(super) fn generation(&self) -> Result<u64, GroupError> {
        let slot = self.state.lock().unwrap_or_else(|error| error.into_inner());
        match &slot.status {
            Status::Lost(error) => Err(GroupError(Arc::clone(error))),
            Status::Ready(_) | Status::Busy => Ok(slot.generation),
        }
    }

    /// Terminate this share group permanently. Every GL object it owns is gone and every context in it is
    /// unusable from here on, so this is reported at error level: it is the last moment at which the
    /// ORIGINAL failure is still named. Afterwards every entry point can say only that the group is lost,
    /// and a driver that dies without a diagnostic leaves the application's later symptoms unattributable —
    /// a `glGenTextures` that never ran, an upload into the reserved name, a null dereference several
    /// calls downstream. Reported once because the first loss is the real one and `lose` is idempotent.
    /// Whether this group has been terminated. Unlike [`Self::take_lost_report`] this does not consume
    /// anything: `glGetGraphicsResetStatus` reports a persistent condition, not a one-shot event.
    pub(super) fn is_lost(&self) -> bool {
        let slot = self.state.lock().unwrap_or_else(|error| error.into_inner());
        matches!(slot.status, Status::Lost(_))
    }

    /// Take the single `GL_CONTEXT_LOST` this loss owes the application, if it has not been taken yet.
    ///
    /// `GL_KHR_robustness` specifies the error is reported ONCE and the queue then reads empty until the
    /// context is recreated. Reporting it on every call would be worse than the silence it replaces:
    /// draining the error queue in a `while (glGetError() != GL_NO_ERROR)` loop is an ordinary idiom, and
    /// dEQP does exactly that, so a sticky error would spin forever.
    pub(super) fn take_lost_report(&self) -> bool {
        let mut slot = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if !matches!(slot.status, Status::Lost(_)) || slot.lost_reported {
            return false;
        }
        slot.lost_reported = true;
        true
    }

    pub(super) fn lose(&self, reason: impl Into<Arc<str>>) {
        let mut slot = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if matches!(slot.status, Status::Lost(_)) {
            return;
        }
        slot.lost_reported = false;
        let Some(generation) = slot.generation.checked_add(1) else {
            slot.status = Status::Lost(Arc::from("share-group generation exhausted"));
            hl_log::hl_error!(
                hl_log::tag::GL,
                "share group lost: generation counter exhausted. Every later GL call in this group \
                 does nothing."
            );
            self.changed.notify_all();
            return;
        };
        let reason = reason.into();
        hl_log::hl_error!(
            hl_log::tag::GL,
            "share group lost: {reason}. Every later GL call in this group does nothing."
        );
        slot.generation = generation;
        slot.status = Status::Lost(reason);
        self.changed.notify_all();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GroupError(Arc<str>);

impl fmt::Display for GroupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for GroupError {}

pub(super) struct GroupLease {
    slot: Arc<GroupSlot>,
    generation: u64,
    data: Option<GroupData>,
}

impl GroupLease {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    #[cfg(test)]
    pub(super) fn data(&self) -> &GroupData {
        self.data.as_ref().expect("live lease owns group data")
    }

    pub(super) fn data_mut(&mut self) -> &mut GroupData {
        self.data.as_mut().expect("live lease owns group data")
    }

    pub(super) fn lose(mut self, reason: impl Into<Arc<str>>) {
        self.data.take();
        self.slot.lose(reason);
    }
}

impl Drop for GroupLease {
    fn drop(&mut self) {
        let Some(data) = self.data.take() else {
            return;
        };
        let mut slot = self
            .slot
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if slot.generation == self.generation && matches!(slot.status, Status::Busy) {
            slot.status = Status::Ready(data);
            self.slot.changed.notify_one();
        }
    }
}

#[cfg(test)]
#[path = "group/tests.rs"]
mod tests;
