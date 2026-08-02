//! GL renderbuffer objects + the renderbuffer table: `glGenRenderbuffers`/`glBindRenderbuffer`/
//! `glRenderbufferStorage`/`glFramebufferRenderbuffer` tracking, for offscreen render targets.
//!
//! This model has no separate renderbuffer storage class: a renderbuffer is modeled as a
//! **texture-backed attachment**. `glRenderbufferStorage` allocates a backing entry in the context's
//! texture table (sized `w`×`h` in the chosen neutral format); `glFramebufferRenderbuffer` then attaches
//! that backing texture as the FBO's color target, reusing the exact same offscreen render-target path a
//! `glFramebufferTexture2D` texture attachment takes ([`crate::service::frame::resolve_target`]). The
//! table itself only tracks name existence + the backing texture name; the storage lives in
//! [`crate::model::texture::Textures`]. Name `0` is the reserved "no renderbuffer" binding.

use std::collections::HashMap;

/// One live GL renderbuffer object. In this model it is backed by a texture-table entry (`tex`, `0` until
/// `glRenderbufferStorage` allocates it) that carries the actual pixel extent/format.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Renderbuffer {
    /// The backing texture GL name in [`crate::model::texture::Textures`] (`0` = no storage allocated yet).
    pub tex: u32,
    pub width: i32,
    pub height: i32,
    pub internal_format: u32,
    pub samples: i32,
}

/// The per-context renderbuffer table: RBO name → [`Renderbuffer`], with a monotonic name counter. Name
/// `0` is the reserved default (never minted).
#[derive(Debug, Default)]
pub struct Renderbuffers {
    map: HashMap<u32, Renderbuffer>,
    next_name: u32,
}

impl Renderbuffers {
    pub fn new() -> Self {
        // RBO names start at 1; name 0 is the reserved "no renderbuffer" binding.
        Self {
            map: HashMap::new(),
            next_name: 1,
        }
    }

    /// `glGenRenderbuffers` — mint one fresh RBO name (storage allocated lazily on `glRenderbufferStorage`).
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

    /// `glIsRenderbuffer` — true once `name` names a generated (non-default) renderbuffer object.
    pub fn contains(&self, name: u32) -> bool {
        name != 0 && self.map.contains_key(&name)
    }

    /// The backing texture GL name of RBO `name` (`0` = unknown RBO or storage not yet allocated).
    pub fn backing_tex(&self, name: u32) -> u32 {
        self.map.get(&name).map(|r| r.tex).unwrap_or(0)
    }

    /// The `(width, height)` extent recorded by `glRenderbufferStorage` for RBO `name`, or `None` if the
    /// RBO is unknown (`glGetRenderbufferParameteriv`).
    pub fn dims(&self, name: u32) -> Option<(i32, i32)> {
        self.map.get(&name).map(|r| (r.width, r.height))
    }

    pub fn get(&self, name: u32) -> Option<&Renderbuffer> {
        self.map.get(&name)
    }

    /// Record `glRenderbufferStorage`: bind `tex` (a texture-table name) as the RBO's backing storage at
    /// the given extent. Creates the RBO entry on demand (matching GL's first-bind-creates behavior).
    pub fn set_storage(
        &mut self,
        name: u32,
        tex: u32,
        width: i32,
        height: i32,
        internal_format: u32,
        samples: i32,
    ) {
        if name != 0 {
            self.map.insert(
                name,
                Renderbuffer {
                    tex,
                    width,
                    height,
                    internal_format,
                    samples,
                },
            );
        }
    }

    /// `glDeleteRenderbuffers` — drop the object. Returns `false` for an unknown / zero name. The backing
    /// texture (an internal allocation the guest never named) is left in the texture table; the caller
    /// detaches it from any framebuffer color slot.
    pub fn delete(&mut self, name: u32) -> Option<Renderbuffer> {
        self.map.remove(&name)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
