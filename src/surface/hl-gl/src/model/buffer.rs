//! GL buffer objects + the buffer table: `glGenBuffers`/`glBufferData`/`glBufferSubData` tracking.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`Buffer`, the `MAXBUF` object array). A GL buffer holds the
//! app's raw bytes plus its declared target/usage; the frame builder ([`crate::service::frame`]) reads a
//! bound buffer's bytes at swap and lowers them to a `CreateBuffer` + `WriteBuffer`. The buffer *ids*
//! here are the GL-object names the guest allocates; the IR buffer ids are minted separately by
//! [`super::context::GlContext`] at swap (exactly as cuda mints its buffer ids in the context).

use std::{collections::HashMap, sync::Arc};

/// One live GL buffer object: the app-uploaded bytes + the target it was last bound/filled against.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct GlBuffer {
    /// The raw bytes the app uploaded (`glBufferData`/`glBufferSubData`).
    pub data: Arc<Vec<u8>>,
    /// The GL target the buffer was bound to when filled (`GL_ARRAY_BUFFER` / `GL_ELEMENT_ARRAY_BUFFER`).
    pub target: u32,
    /// The GL usage hint (`GL_STATIC_DRAW`, …) — carried for fidelity; the lowering ignores it.
    pub usage: u32,
    /// Bumped on every content mutation — a dirty key a residency-aware swap can use to skip re-uploads.
    pub gen: u64,
    /// The currently-mapped byte range `(offset, length)` (`glMapBufferRange`), or `None` if not mapped.
    /// The app writes through the pointer this range hands back; `glUnmapBuffer` clears it and flushes.
    pub mapped: Option<(usize, usize)>,
    /// Access flags validated by `glMapBufferRange`; explicit flush requires its corresponding bit.
    pub mapped_access: u32,
}

/// The per-context buffer table: GL name → [`GlBuffer`], with a monotonic name counter. Name `0` is the
/// reserved "no buffer" binding (never minted), matching GL.
#[derive(Debug, Default)]
pub struct Buffers {
    map: HashMap<u32, GlBuffer>,
    /// Storage of mapped objects deleted through GL. Deletion makes the name and bindings disappear
    /// immediately, but retaining the allocation until context teardown prevents an outstanding FFI map
    /// pointer from becoming a dangling Rust pointer.
    retired_mappings: Vec<Arc<Vec<u8>>>,
    next_name: u32,
}

impl Buffers {
    pub fn new() -> Self {
        // GL names start at 1; name 0 is the reserved unbound sentinel.
        Self {
            map: HashMap::new(),
            retired_mappings: Vec::new(),
            next_name: 1,
        }
    }

    /// `glGenBuffers` — mint one fresh GL buffer name (allocated lazily; the object materializes on the
    /// first `glBindBuffer`/`glBufferData`).
    pub fn gen(&mut self) -> u32 {
        let name = self.next_name;
        self.next_name += 1;
        self.map.entry(name).or_default();
        name
    }

    /// Materialize a non-zero name bound through `GL_CHROMIUM_bind_generates_resource`.
    pub fn ensure(&mut self, name: u32) {
        if name != 0 {
            self.map.entry(name).or_default();
            self.next_name = self.next_name.max(name.saturating_add(1));
        }
    }

    /// `glBufferData(target, data, usage)` — (re)fill the buffer's bytes, bumping its generation.
    pub fn set_data(&mut self, name: u32, target: u32, data: &[u8], usage: u32) {
        let b = self.map.entry(name).or_default();
        b.data = Arc::new(data.to_vec());
        b.target = target;
        b.usage = usage;
        b.gen += 1;
    }

    /// `glBufferSubData(target, offset, data)` — overwrite a sub-range, bumping its generation. Uses a
    /// checked add so an adversarial `offset` near `usize::MAX` can never overflow (debug panic) — an
    /// overflowing range is dropped. Callers (`record::buffer_sub_data`, `copy_buffer_sub_data`) validate
    /// the range fits first, so the grow branch only ever covers an exact-fit write.
    pub fn set_sub_data(&mut self, name: u32, offset: usize, data: &[u8]) {
        if let Some(b) = self.map.get_mut(&name) {
            let Some(end) = offset.checked_add(data.len()) else {
                return;
            };
            let storage = Arc::make_mut(&mut b.data);
            if end > storage.len() {
                storage.resize(end, 0);
            }
            storage[offset..end].copy_from_slice(data);
            b.gen += 1;
        }
    }

    pub fn get(&self, name: u32) -> Option<&GlBuffer> {
        self.map.get(&name)
    }

    pub fn get_mut(&mut self, name: u32) -> Option<&mut GlBuffer> {
        self.map.get_mut(&name)
    }

    pub fn is_mapped(&self, name: u32) -> bool {
        self.map
            .get(&name)
            .is_some_and(|buffer| buffer.mapped.is_some())
    }

    pub fn mapped_access(&self, name: u32) -> Option<u32> {
        self.map
            .get(&name)
            .filter(|buffer| buffer.mapped.is_some())
            .map(|buffer| buffer.mapped_access)
    }

    /// `glMapBufferRange` — grow the buffer's storage to cover `[offset, offset+length)`, record the
    /// mapped range, and return the byte offset the mapping starts at (so the C shim can hand back a
    /// pointer INTO the buffer's `data`). Returns `None` for an unknown/zero name.
    pub fn map_range(
        &mut self,
        name: u32,
        offset: usize,
        length: usize,
        access: u32,
    ) -> Option<usize> {
        let b = self.map.get_mut(&name)?;
        let need = offset.checked_add(length)?;
        let storage = Arc::make_mut(&mut b.data);
        if storage.len() < need {
            storage.resize(need, 0);
        }
        b.mapped = Some((offset, length));
        b.mapped_access = access;
        Some(offset)
    }

    /// Pointer into a live mapped range. [`Self::map_range`] detached shared snapshots before recording
    /// the mapping, so this cannot mutate bytes retained by an earlier draw.
    pub fn mapped_ptr(&mut self, name: u32, offset: usize) -> Option<*mut u8> {
        let buffer = self.map.get_mut(&name)?;
        let (mapped_offset, mapped_len) = buffer.mapped?;
        if offset < mapped_offset || offset > mapped_offset.checked_add(mapped_len)? {
            return None;
        }
        let storage = Arc::get_mut(&mut buffer.data)?;
        // SAFETY: `map_range` validated the mapped interval against `storage.len()`, and the bounds check
        // above restricts `offset` to that interval. The unique Arc keeps the allocation stable and exclusive.
        Some(unsafe { storage.as_mut_ptr().add(offset) })
    }

    /// `glUnmapBuffer` — clear the mapped flag and return the range with its access contract. Writable
    /// mappings become a new content generation; read-only mappings leave content unchanged.
    pub fn take_map(&mut self, name: u32) -> Option<(usize, Vec<u8>, u32)> {
        let b = self.map.get_mut(&name)?;
        let (off, len) = b.mapped.take()?;
        let access = std::mem::take(&mut b.mapped_access);
        if access & crate::model::glconst::GL_MAP_WRITE_BIT != 0 {
            b.gen += 1;
        }
        let end = (off + len).min(b.data.len());
        Some((off, b.data[off..end].to_vec(), access))
    }

    /// The bytes of the buffer's `[offset, offset+length)` sub-range (clamped to its storage) — the source
    /// of a `glFlushMappedBufferRange` explicit flush. `None` for an unknown name.
    pub fn range_bytes(&self, name: u32, offset: usize, length: usize) -> Option<Vec<u8>> {
        let b = self.map.get(&name)?;
        let end = offset.checked_add(length)?.min(b.data.len());
        if offset >= b.data.len() {
            return Some(Vec::new());
        }
        Some(b.data[offset..end].to_vec())
    }

    /// Mark writes made through a live mapped pointer as a new content generation. The bytes already live
    /// in `data`; this generation boundary keeps deferred draws on opposite sides of an explicit flush from
    /// sharing one resident IR buffer.
    pub fn mark_changed(&mut self, name: u32) {
        if let Some(buffer) = self.map.get_mut(&name) {
            buffer.gen += 1;
        }
    }

    /// Non-empty content is what the frame builder requires before it uploads a bound buffer.
    pub fn has_data(&self, name: u32) -> bool {
        self.map
            .get(&name)
            .map(|b| !b.data.is_empty())
            .unwrap_or(false)
    }

    /// `glDeleteBuffers` — drop the object. Returns `false` for an unknown name.
    pub fn delete(&mut self, name: u32) -> bool {
        let Some(buffer) = self.map.remove(&name) else {
            return false;
        };
        if buffer.mapped.is_some() {
            self.retired_mappings.push(buffer.data);
        }
        true
    }

    #[cfg(test)]
    pub fn retired_mapping_count(&self) -> usize {
        self.retired_mappings.len()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
