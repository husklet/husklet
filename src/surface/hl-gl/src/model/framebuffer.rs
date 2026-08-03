//! GL framebuffer objects + the framebuffer table: `glGenFramebuffers`/`glBindFramebuffer`/
//! `glFramebufferTexture2D` tracking, for offscreen (non-default) render targets.
//!
//! A framebuffer object (FBO) names an off-screen render target: `glFramebufferTexture2D` attaches a GL
//! texture as its color attachment, and a draw recorded while the FBO is bound renders into that texture
//! rather than the default window surface. The model tracks only the FBO name → attached color-texture
//! name mapping; the frame builder ([`crate::service::frame`]) resolves the attachment to a
//! `CreateTexture(RENDER_TARGET)` at swap and renders the frame's geometry into it. Ported in spirit from
//! `hl-shim-gl`'s framebuffer bookkeeping. Name `0` is the reserved default framebuffer (never minted).

use std::collections::HashMap;

/// One texture object retained by an FBO attachment. `name` is the GL-visible spelling reported by
/// attachment queries; `object` is the stable identity used internally after that name is deleted/reused.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextureAttachment {
    pub name: u32,
    pub object: u64,
}

/// The per-context framebuffer table: FBO name → attached color-texture GL name (`0` = no attachment),
/// with a monotonic name counter. Name `0` is the reserved default framebuffer.
#[derive(Debug, Default)]
pub struct Framebuffers {
    /// FBO name → its `GL_COLOR_ATTACHMENT0` texture GL name (the single-target fast path, unchanged).
    color: HashMap<u32, TextureAttachment>,
    /// FBO name → its extra `GL_COLOR_ATTACHMENT{i}` (i ≥ 1) texture GL names, for multiple render targets
    /// (`glDrawBuffers` MRT). Keyed `(fbo, attachment_index)`; attachment 0 stays in `color` above so a
    /// single-attachment FBO is byte-identical to before. `0` = no attachment at that index.
    color_extra: HashMap<(u32, u32), TextureAttachment>,
    /// FBO name → the object attached at `GL_DEPTH_ATTACHMENT` / `GL_STENCIL_ATTACHMENT` (`0` = none).
    ///
    /// This model has no depth or stencil PLANE of its own — the frame builder mints one whenever a draw
    /// is depth- or stencil-tested — so the attachment is tracked for its GL semantics rather than for its
    /// storage: ES 3.0 §4.1.5/§4.1.6 make the depth test always pass and write nothing when the draw
    /// framebuffer has no depth attachment, and likewise for stencil. Without this an app that leaves
    /// `GL_DEPTH_TEST` enabled while rendering to a depth-less FBO loses every draw after the first.
    depth: HashMap<u32, (u32, bool)>,
    stencil: HashMap<u32, (u32, bool)>,
    /// What the application actually attached at each colour slot: `(GL object name, is renderbuffer)`.
    ///
    /// A renderbuffer attachment RESOLVES to its backing texture for rendering, so the colour tables above
    /// hold a texture name either way and cannot answer
    /// `GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE`/`_OBJECT_NAME` — both reported `GL_TEXTURE` and the backing
    /// texture's name for a renderbuffer, which is the one thing the query exists to distinguish.
    color_source: HashMap<(u32, u32), (u32, bool)>,
    next_name: u32,
}

impl Framebuffers {
    pub fn sample_count(&self, _fbo: u32) -> u32 {
        1
    }

    pub fn new() -> Self {
        // FBO names start at 1; name 0 is the default framebuffer.
        Self {
            color: HashMap::new(),
            color_extra: HashMap::new(),
            depth: HashMap::new(),
            stencil: HashMap::new(),
            color_source: HashMap::new(),
            next_name: 1,
        }
    }

    /// `glGenFramebuffers` — mint one fresh FBO name (materialized lazily on the first attach).
    pub fn gen(&mut self) -> u32 {
        let name = self.next_name;
        self.next_name += 1;
        self.color.entry(name).or_default();
        name
    }

    /// Materialize a non-zero name bound through `GL_CHROMIUM_bind_generates_resource`.
    pub fn ensure(&mut self, name: u32) {
        if name != 0 {
            self.color.entry(name).or_default();
            self.next_name = self.next_name.max(name.saturating_add(1));
        }
    }

    /// `glFramebufferTexture2D(GL_COLOR_ATTACHMENT0, tex)` — attach `tex` as `fbo`'s color target.
    pub fn attach_color(&mut self, fbo: u32, tex: u32) {
        self.attach_color_object(fbo, 0, tex, 0);
    }

    pub fn attach_color_object(&mut self, fbo: u32, index: u32, name: u32, object: u64) {
        if fbo != 0 {
            self.set_color_source(fbo, index, name, false);
            let attachment = TextureAttachment { name, object };
            if index == 0 {
                self.color.insert(fbo, attachment);
            } else {
                self.color_extra.insert((fbo, index), attachment);
            }
        }
    }

    /// `glFramebufferTexture2D(GL_COLOR_ATTACHMENT{index}, tex)` — attach `tex` at color-attachment `index`.
    /// Index 0 routes to [`attach_color`] (the single-target slot); index ≥ 1 is an MRT extra attachment.
    pub fn attach_color_index(&mut self, fbo: u32, index: u32, tex: u32) {
        if fbo == 0 {
            return;
        }
        self.set_color_source(fbo, index, tex, false);
        if index == 0 {
            self.color.insert(
                fbo,
                TextureAttachment {
                    name: tex,
                    object: 0,
                },
            );
        } else {
            self.color_extra.insert(
                (fbo, index),
                TextureAttachment {
                    name: tex,
                    object: 0,
                },
            );
        }
    }

    /// Record what was attached at a colour slot, so the attachment queries can tell a renderbuffer from
    /// a texture. `name` `0` clears the slot.
    pub fn set_color_source(&mut self, fbo: u32, index: u32, name: u32, renderbuffer: bool) {
        if fbo == 0 {
            return;
        }
        if name == 0 {
            self.color_source.remove(&(fbo, index));
        } else {
            self.color_source.insert((fbo, index), (name, renderbuffer));
        }
    }

    /// What is attached at `fbo`'s colour slot `index`, as `(GL object name, is renderbuffer)`. `None`
    /// when the slot is empty.
    pub fn color_source(&self, fbo: u32, index: u32) -> Option<(u32, bool)> {
        self.color_source.get(&(fbo, index)).copied()
    }

    /// `glFramebufferRenderbuffer`/`glFramebufferTexture2D` at `GL_DEPTH_ATTACHMENT` — `name` `0` detaches.
    pub fn attach_depth(&mut self, fbo: u32, name: u32, renderbuffer: bool) {
        if fbo != 0 {
            self.depth.insert(fbo, (name, renderbuffer));
        }
    }

    /// `glFramebufferRenderbuffer`/`glFramebufferTexture2D` at `GL_STENCIL_ATTACHMENT` — `0` detaches.
    pub fn attach_stencil(&mut self, fbo: u32, name: u32, renderbuffer: bool) {
        if fbo != 0 {
            self.stencil.insert(fbo, (name, renderbuffer));
        }
    }

    pub fn depth_source(&self, fbo: u32) -> Option<(u32, bool)> {
        self.depth.get(&fbo).copied().filter(|(name, _)| *name != 0)
    }

    pub fn stencil_source(&self, fbo: u32) -> Option<(u32, bool)> {
        self.stencil
            .get(&fbo)
            .copied()
            .filter(|(name, _)| *name != 0)
    }

    /// Whether `fbo` carries a depth attachment. The default framebuffer's answer belongs to its
    /// `EGLConfig`, not to this table, so callers pass `fbo != 0` here only.
    pub fn has_depth(&self, fbo: u32) -> bool {
        self.depth_source(fbo).is_some()
    }

    /// Whether `fbo` carries a stencil attachment (see [`Self::has_depth`]).
    pub fn has_stencil(&self, fbo: u32) -> bool {
        self.stencil_source(fbo).is_some()
    }

    /// The GL texture attached as `fbo`'s color target (`0` = default framebuffer or no attachment).
    pub fn color_attachment(&self, fbo: u32) -> u32 {
        self.color.get(&fbo).map_or(0, |attachment| attachment.name)
    }

    pub fn color_attachment_object(&self, fbo: u32, index: u32) -> Option<TextureAttachment> {
        let attachment = if index == 0 {
            self.color.get(&fbo)
        } else {
            self.color_extra.get(&(fbo, index))
        }?;
        (attachment.name != 0).then_some(*attachment)
    }

    /// The GL texture attached at `fbo`'s `GL_COLOR_ATTACHMENT{index}` (`0` = none). Index 0 is the
    /// single-target slot; index ≥ 1 an MRT extra attachment.
    pub fn color_attachment_index(&self, fbo: u32, index: u32) -> u32 {
        if index == 0 {
            self.color.get(&fbo).map_or(0, |attachment| attachment.name)
        } else {
            self.color_extra
                .get(&(fbo, index))
                .map_or(0, |attachment| attachment.name)
        }
    }

    /// The count of contiguous materialized color attachments on `fbo` starting at index 0 (so an FBO with
    /// attachments 0 and 1 returns 2). Stops at the first missing index — the MRT frame path renders this
    /// many targets.
    pub fn color_attachment_count(&self, fbo: u32) -> u32 {
        if fbo == 0 || self.color.get(&fbo).map_or(0, |attachment| attachment.name) == 0 {
            return 0;
        }
        let mut n = 1;
        while self
            .color_extra
            .get(&(fbo, n))
            .map_or(0, |attachment| attachment.name)
            != 0
        {
            n += 1;
        }
        n
    }

    /// True once `name` names a generated (non-default) FBO object (`glIsFramebuffer`, and the
    /// completeness check's "does this framebuffer exist" gate). Name `0` (the default framebuffer) is
    /// never a generated object.
    pub fn exists(&self, name: u32) -> bool {
        name != 0 && self.color.contains_key(&name)
    }

    /// Detach texture `tex` from every FBO's color slot (used when the underlying object — a texture or a
    /// renderbuffer's backing texture — is deleted, so a stale attachment can't leak into a later frame).
    pub fn detach_color_texture(&mut self, tex: u32) {
        if tex == 0 {
            return;
        }
        for attachment in self.color.values_mut() {
            if attachment.name == tex {
                *attachment = TextureAttachment::default();
            }
        }
        for attachment in self.color_extra.values_mut() {
            if attachment.name == tex {
                *attachment = TextureAttachment::default();
            }
        }
        // A texture attachment's source name IS the texture, so deleting it empties the slot. A
        // renderbuffer's source is the renderbuffer and survives its backing texture being reclaimed.
        self.color_source
            .retain(|_, (name, renderbuffer)| *renderbuffer || *name != tex);
    }

    pub(crate) fn references_texture(&self, texture: u32) -> bool {
        texture != 0
            && (self.color.values().any(|value| value.name == texture)
                || self.color_extra.values().any(|value| value.name == texture))
    }

    pub(crate) fn references_object(&self, object: u64) -> bool {
        object != 0
            && (self.color.values().any(|value| value.object == object)
                || self
                    .color_extra
                    .values()
                    .any(|value| value.object == object))
    }

    pub(crate) fn attached_objects(&self, fbo: u32) -> Vec<u64> {
        let mut objects = Vec::new();
        if let Some(attachment) = self.color.get(&fbo) {
            if attachment.object != 0 {
                objects.push(attachment.object);
            }
        }
        objects.extend(
            self.color_extra
                .iter()
                .filter_map(|(&(owner, _), attachment)| {
                    (owner == fbo && attachment.object != 0).then_some(attachment.object)
                }),
        );
        objects.sort_unstable();
        objects.dedup();
        objects
    }

    /// Detach `texture` only from `fbo`. GL deletion affects attachment points of the currently bound FBO;
    /// an unbound FBO retains its object reference even though the public texture name becomes reusable.
    pub fn detach_color_texture_from(&mut self, fbo: u32, texture: u32) {
        if let Some(attachment) = self.color.get_mut(&fbo) {
            if attachment.name == texture {
                *attachment = TextureAttachment::default();
            }
        }
        for (&(owner, _), attachment) in &mut self.color_extra {
            if owner == fbo && attachment.name == texture {
                *attachment = TextureAttachment::default();
            }
        }
        self.color_source
            .retain(|&(owner, _), (name, renderbuffer)| {
                owner != fbo || *renderbuffer || *name != texture
            });
    }

    pub fn detach_color_renderbuffer_from(&mut self, fbo: u32, renderbuffer: u32) {
        let indexes = self
            .color_source
            .iter()
            .filter_map(|(&(owner, index), &(name, is_renderbuffer))| {
                (owner == fbo && is_renderbuffer && name == renderbuffer).then_some(index)
            })
            .collect::<Vec<_>>();
        for index in indexes {
            if index == 0 {
                self.color.insert(fbo, TextureAttachment::default());
            } else {
                self.color_extra
                    .insert((fbo, index), TextureAttachment::default());
            }
            self.color_source.remove(&(fbo, index));
        }
    }

    /// `glDeleteFramebuffers` — drop the object. Returns `false` for an unknown name.
    pub fn delete(&mut self, name: u32) -> bool {
        self.color_extra.retain(|&(fbo, _), _| fbo != name);
        self.color_source.retain(|&(fbo, _), _| fbo != name);
        self.depth.remove(&name);
        self.stencil.remove(&name);
        self.color.remove(&name).is_some()
    }

    pub fn len(&self) -> usize {
        self.color.len()
    }

    pub fn is_empty(&self) -> bool {
        self.color.is_empty()
    }
}
