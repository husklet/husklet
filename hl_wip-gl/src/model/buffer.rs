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

    /// `glBufferSubData(target, offset, data)` — overwrite a sub-range, bumping its generation.
    pub fn set_sub_data(&mut self, name: u32, offset: usize, data: &[u8]) {
        if let Some(b) = self.map.get_mut(&name) {
            if offset + data.len() > b.data.len() {
                b.data.resize(offset + data.len(), 0);
            }
            b.data[offset..offset + data.len()].copy_from_slice(data);
            b.gen += 1;
        }
    }

    pub fn get(&self, name: u32) -> Option<&GlBuffer> {
        self.map.get(&name)
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
