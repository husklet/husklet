//! GL buffer objects + the buffer table: `glGenBuffers`/`glBufferData`/`glBufferSubData` tracking.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`Buffer`, the `MAXBUF` object array). A GL buffer holds the
//! app's raw bytes plus its declared target/usage; the frame builder ([`crate::service::frame`]) reads a
//! bound buffer's bytes at swap and lowers them to a `CreateBuffer` + `WriteBuffer`. The buffer *ids*
//! here are the GL-object names the guest allocates; the IR buffer ids are minted separately by
//! [`super::context::GlContext`] at swap (exactly as cuda mints its buffer ids in the context).

use std::collections::HashMap;

/// One live GL buffer object: the app-uploaded bytes + the target it was last bound/filled against.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct GlBuffer {
    /// The raw bytes the app uploaded (`glBufferData`/`glBufferSubData`).
    pub data: Vec<u8>,
    /// The GL target the buffer was bound to when filled (`GL_ARRAY_BUFFER` / `GL_ELEMENT_ARRAY_BUFFER`).
    pub target: u32,
    /// The GL usage hint (`GL_STATIC_DRAW`, …) — carried for fidelity; the lowering ignores it.
    pub usage: u32,
    /// Bumped on every content mutation — a dirty key a residency-aware swap can use to skip re-uploads.
    pub gen: u64,
    /// The currently-mapped byte range `(offset, length)` (`glMapBufferRange`), or `None` if not mapped.
    /// The app writes through the pointer this range hands back; `glUnmapBuffer` clears it and flushes.
    pub mapped: Option<(usize, usize)>,
}

/// The per-context buffer table: GL name → [`GlBuffer`], with a monotonic name counter. Name `0` is the
/// reserved "no buffer" binding (never minted), matching GL.
#[derive(Debug, Default)]
pub struct Buffers {
    map: HashMap<u32, GlBuffer>,
    next_name: u32,
}

impl Buffers {
    pub fn new() -> Self {
        // GL names start at 1; name 0 is the reserved unbound sentinel.
        Self { map: HashMap::new(), next_name: 1 }
    }

    /// `glGenBuffers` — mint one fresh GL buffer name (allocated lazily; the object materializes on the
    /// first `glBindBuffer`/`glBufferData`).
    pub fn gen(&mut self) -> u32 {
        let name = self.next_name;
        self.next_name += 1;
        self.map.entry(name).or_default();
        name
    }

    /// `glBufferData(target, data, usage)` — (re)fill the buffer's bytes, bumping its generation.
    pub fn set_data(&mut self, name: u32, target: u32, data: &[u8], usage: u32) {
        let b = self.map.entry(name).or_default();
        b.data = data.to_vec();
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
            let Some(end) = offset.checked_add(data.len()) else { return };
            if end > b.data.len() {
                b.data.resize(end, 0);
            }
            b.data[offset..end].copy_from_slice(data);
            b.gen += 1;
        }
    }

    pub fn get(&self, name: u32) -> Option<&GlBuffer> {
        self.map.get(&name)
    }

    pub fn get_mut(&mut self, name: u32) -> Option<&mut GlBuffer> {
        self.map.get_mut(&name)
    }

    /// `glMapBufferRange` — grow the buffer's storage to cover `[offset, offset+length)`, record the
    /// mapped range, and return the byte offset the mapping starts at (so the C shim can hand back a
    /// pointer INTO the buffer's `data`). Returns `None` for an unknown/zero name.
    pub fn map_range(&mut self, name: u32, offset: usize, length: usize) -> Option<usize> {
        let b = self.map.get_mut(&name)?;
        let need = offset.checked_add(length)?;
        if b.data.len() < need {
            b.data.resize(need, 0);
        }
        b.mapped = Some((offset, length));
        Some(offset)
    }

    /// `glUnmapBuffer` — clear the mapped flag, bump the generation (the app may have written through the
    /// pointer), and return the mapped range's `(offset, bytes)` so the caller can flush it as a
    /// `WriteBuffer`. Returns `None` if the buffer was not mapped / is unknown.
    pub fn take_map(&mut self, name: u32) -> Option<(usize, Vec<u8>)> {
        let b = self.map.get_mut(&name)?;
        let (off, len) = b.mapped.take()?;
        b.gen += 1;
        let end = (off + len).min(b.data.len());
        Some((off, b.data[off..end].to_vec()))
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

    /// Non-empty content is what the frame builder requires before it uploads a bound buffer.
    pub fn has_data(&self, name: u32) -> bool {
        self.map.get(&name).map(|b| !b.data.is_empty()).unwrap_or(false)
    }

    /// `glDeleteBuffers` — drop the object. Returns `false` for an unknown name.
    pub fn delete(&mut self, name: u32) -> bool {
        self.map.remove(&name).is_some()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
