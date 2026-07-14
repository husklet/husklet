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

/// The per-context framebuffer table: FBO name → attached color-texture GL name (`0` = no attachment),
/// with a monotonic name counter. Name `0` is the reserved default framebuffer.
#[derive(Debug, Default)]
pub struct Framebuffers {
    /// FBO name → its color-attachment texture GL name.
    color: HashMap<u32, u32>,
    next_name: u32,
}

impl Framebuffers {
    pub fn new() -> Self {
        // FBO names start at 1; name 0 is the default framebuffer.
        Self { color: HashMap::new(), next_name: 1 }
    }

    /// `glGenFramebuffers` — mint one fresh FBO name (materialized lazily on the first attach).
    pub fn gen(&mut self) -> u32 {
        let name = self.next_name;
        self.next_name += 1;
        self.color.entry(name).or_insert(0);
        name
    }

    /// `glFramebufferTexture2D(GL_COLOR_ATTACHMENT0, tex)` — attach `tex` as `fbo`'s color target.
    pub fn attach_color(&mut self, fbo: u32, tex: u32) {
        if fbo != 0 {
            self.color.insert(fbo, tex);
        }
    }

    /// The GL texture attached as `fbo`'s color target (`0` = default framebuffer or no attachment).
    pub fn color_attachment(&self, fbo: u32) -> u32 {
        self.color.get(&fbo).copied().unwrap_or(0)
    }

    /// `glDeleteFramebuffers` — drop the object. Returns `false` for an unknown name.
    pub fn delete(&mut self, name: u32) -> bool {
        self.color.remove(&name).is_some()
    }

    pub fn len(&self) -> usize {
        self.color.len()
    }

    pub fn is_empty(&self) -> bool {
        self.color.is_empty()
    }
}
