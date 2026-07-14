//! [`MacPresenter`]: the concrete macOS [`Presenter`] the neutral scene hands finished frames to.
//!
//! It composites the currently-attached content for each surface (a `wl_shm` BGRA buffer or a zero-copy
//! host `IOSurface`) through the Metal composite pipeline into a persistent offscreen target, then — in
//! windowed mode — blits that into a `CAMetalLayer` drawable. Headless (offscreen) mode keeps only the
//! backing target, which [`MacPresenter::last_rgba`] reads back to prove the present on a real GPU
//! without a visible window / GUI session.
//!
//! The neutral [`PresentableImage`] carries geometry only, so pixels are supplied out-of-band via
//! [`MacPresenter::attach_bgra`] / [`MacPresenter::attach_iosurface`] before `present()` runs — the same
//! commit-then-compose split a Wayland compositor uses.

use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::MainThreadMarker;
use objc2_metal::{MTLRenderPipelineState, MTLTexture};

use crate::scene::model::{OutputId, PresentableImage, Rect, SurfaceId, Visibility};
use crate::scene::port::{PresentOutcome, PresentTiming, PresentationFeedback, Presenter};

use super::iosurface::{cfrelease, dimensions, lookup};
use super::metal::{bgra_to_rgba, MetalCtx};
use super::window::{ensure_app, MetalWindow};

/// The pixel source a surface last attached — resolved to an `MTLTexture` at present time.
enum Content {
    /// A `wl_shm` BGRA buffer (tight `w*4` rows), uploaded to a texture each present.
    Bgra { bgra: Vec<u8>, w: u32, h: u32 },
    /// A host `IOSurface` global id, wrapped zero-copy each present.
    IoSurfaceId(u32),
}

/// Per-surface presenter state: the attached content, the persistent composite target, and (windowed
/// mode) the native window.
struct SurfState {
    content: Option<Content>,
    /// The last composited frame `(w, h, texture)` in device pixels — readable back for verification.
    composite: Option<(u32, u32, Retained<ProtocolObject<dyn MTLTexture>>)>,
    window: Option<MetalWindow>,
    title: String,
}

impl SurfState {
    fn new() -> SurfState {
        SurfState {
            content: None,
            composite: None,
            window: None,
            title: String::new(),
        }
    }
}

/// A [`Presenter`] that draws each composed frame to a real macOS window (Cocoa `NSWindow` +
/// `CAMetalLayer` + Metal), or headless into an offscreen `MTLTexture` it can read back.
pub struct MacPresenter {
    ctx: MetalCtx,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    surfaces: HashMap<SurfaceId, SurfState>,
    /// `Some` ⇒ windowed mode (open real `NSWindow`s); `None` ⇒ headless offscreen mode.
    mtm: Option<MainThreadMarker>,
    /// Monotonic per-frame pacing serial for delivered frames.
    serial: u64,
    /// Total frames presented.
    pub frames: u32,
}

impl MacPresenter {
    /// Headless: composite into offscreen `MTLTexture`s only (no windows). Provable on any Metal GPU with
    /// no GUI login session. `None` if Metal is unavailable / the composite pipeline fails to compile.
    pub fn new_offscreen() -> Option<MacPresenter> {
        let ctx = MetalCtx::new()?;
        let pipeline = ctx.make_composite_pipeline()?;
        Some(MacPresenter {
            ctx,
            pipeline,
            surfaces: HashMap::new(),
            mtm: None,
            serial: 0,
            frames: 0,
        })
    }

    /// Windowed: open a real `NSWindow` + `CAMetalLayer` per surface. Must be constructed on the AppKit
    /// main thread; a visible window additionally needs a GUI login session. `None` if Metal is
    /// unavailable / the pipeline fails.
    pub fn new_windowed(mtm: MainThreadMarker) -> Option<MacPresenter> {
        ensure_app(mtm);
        let ctx = MetalCtx::new()?;
        let pipeline = ctx.make_composite_pipeline()?;
        Some(MacPresenter {
            ctx,
            pipeline,
            surfaces: HashMap::new(),
            mtm: Some(mtm),
            serial: 0,
            frames: 0,
        })
    }

    /// The Metal device (GPU/adapter) name the present runs on, e.g. "Apple M-series".
    pub fn device_name(&self) -> String {
        self.ctx.device_name()
    }

    /// Attach a `wl_shm` BGRA buffer (tight `w*4` rows, little-endian ARGB8888 memory order B,G,R,A) as
    /// surface `sid`'s content for the next present.
    pub fn attach_bgra(&mut self, sid: SurfaceId, bgra: Vec<u8>, w: u32, h: u32) {
        self.surfaces
            .entry(sid)
            .or_insert_with(SurfState::new)
            .content = Some(Content::Bgra { bgra, w, h });
    }

    /// Attach a host `IOSurface` global id as surface `sid`'s content — wrapped zero-copy at present.
    pub fn attach_iosurface(&mut self, sid: SurfaceId, id: u32) {
        self.surfaces
            .entry(sid)
            .or_insert_with(SurfState::new)
            .content = Some(Content::IoSurfaceId(id));
    }

    /// Set the title used for surface `sid`'s native window.
    pub fn set_title(&mut self, sid: SurfaceId, title: impl Into<String>) {
        self.surfaces
            .entry(sid)
            .or_insert_with(SurfState::new)
            .title = title.into();
    }

    /// Read surface `sid`'s last composited frame back as `(w, h, RGBA)` — the GUI-session-free proof of
    /// what the presenter put on (or would put on) screen.
    pub fn last_rgba(&self, sid: SurfaceId) -> Option<(u32, u32, Vec<u8>)> {
        let (w, h, tex) = self.surfaces.get(&sid)?.composite.as_ref()?;
        let bgra = self.ctx.readback_bgra(tex, *w, *h);
        Some((*w, *h, bgra_to_rgba(&bgra)))
    }

    /// Compose surface `sid`'s attached content into its persistent target. Returns the composite
    /// `(w, h)` on success, or an error string on device failure (unresolvable IOSurface, missing
    /// content). Does not touch the window.
    fn compose(&mut self, sid: SurfaceId) -> Result<(u32, u32), String> {
        let st = self.surfaces.get(&sid).ok_or("no state for surface")?;
        let content = st.content.as_ref().ok_or("no content attached")?;

        // Resolve the source texture (and its device-pixel size) from the attached content.
        let (src, w, h, io) = match content {
            Content::Bgra { bgra, w, h } => {
                let tex = self.ctx.upload_bgra(bgra, *w, *h);
                (tex, *w, *h, std::ptr::null_mut())
            }
            Content::IoSurfaceId(id) => {
                let surface = unsafe { lookup(*id) };
                if surface.is_null() {
                    return Err(format!("IOSurface id {id} not found"));
                }
                let (sw, sh, _) = unsafe { dimensions(surface) };
                let tex = self.ctx.texture_from_iosurface(surface, sw as u32, sh as u32);
                (tex, sw as u32, sh as u32, surface)
            }
        };

        // Reuse a persistent composite target sized to the source (zero per-frame realloc at steady size).
        let need_new = match &self.surfaces.get(&sid).unwrap().composite {
            Some((cw, ch, _)) => *cw != w || *ch != h,
            None => true,
        };
        if need_new {
            let tex = self.ctx.new_bgra_texture(w, h);
            self.surfaces.get_mut(&sid).unwrap().composite = Some((w, h, tex));
        }
        let dst = self.surfaces.get(&sid).unwrap().composite.as_ref().unwrap().2.clone();
        self.ctx
            .compose_into(&self.pipeline, &src, &dst, [0.0, 0.0, 1.0, 1.0]);
        if !io.is_null() {
            unsafe { cfrelease(io) };
        }
        Ok((w, h))
    }
}

impl Presenter for MacPresenter {
    fn present(
        &mut self,
        _output: OutputId,
        image: &PresentableImage,
        _damage: &[Rect],
        _timing: PresentTiming,
    ) -> PresentationFeedback {
        let sid = image.surface;
        let (w, h) = match self.compose(sid) {
            Ok(dims) => dims,
            Err(err) => {
                eprintln!("[macos-surface] present sid={sid:?}: {err}");
                // No content / unresolvable device resource: retryable — the compositor keeps pacing so a
                // re-attached buffer next cycle can succeed.
                return PresentationFeedback {
                    outcome: PresentOutcome::RetryableFailure,
                };
            }
        };
        self.frames += 1;

        // Windowed mode: open the window lazily (sized to the image's logical points), size its drawable
        // to the composite's device pixels, and blit. If no window/session is up the frame stays offscreen.
        let shown = if let Some(mtm) = self.mtm {
            let st = self.surfaces.get_mut(&sid).unwrap();
            if st.window.is_none() {
                let title = if st.title.is_empty() {
                    format!("hl surface {}", sid.0)
                } else {
                    st.title.clone()
                };
                st.window = Some(MetalWindow::new(
                    mtm,
                    &self.ctx,
                    image.width.max(1) as u32,
                    image.height.max(1) as u32,
                    &title,
                ));
            }
            let win = st.window.as_ref().unwrap();
            win.set_drawable_size(w, h);
            let composite = st.composite.as_ref().unwrap().2.clone();
            win.present(&self.ctx, &composite)
        } else {
            false
        };

        if shown {
            self.serial += 1;
            PresentationFeedback::delivered(self.serial, None)
        } else {
            // Composited into the backing target but not visibly shown (headless, or window not yet on
            // screen). Honest `Offscreen` so the schedule service does not advance pacing as if it shipped.
            PresentationFeedback::offscreen()
        }
    }

    fn set_visibility(&mut self, _surface: SurfaceId, _visibility: Visibility) {
        // A future step maps this onto NSWindow miniaturize/order; the present path already withholds
        // delivery for a non-visible window (see `MetalWindow::is_visible`).
    }
}
