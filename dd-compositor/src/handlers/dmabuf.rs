//! `zwp_linux_dmabuf_v1` — the GPU present path for the Smithay compositor.
//!
//! ## Why dmabuf means "resolve a dd IOSurface" on this boundary
//! dd runs Linux guests on a macOS host. A guest GPU client (glmark2, es2tri, a GPU-composited
//! browser) renders through dd's GPU stack, which executes the guest's GL/GLES/Vulkan commands on
//! the host Metal device INTO a host `IOSurface`. It never makes a real Linux PRIME/`dma-buf`
//! export — there is no kernel dma-buf to import. What the guest hands the compositor as a
//! "dmabuf" is really a *reference to that host IOSurface*, carried in the dmabuf's DRM format
//! MODIFIER: `modifier_lo` is the IOSurface id and `modifier_hi`'s low 16 bits are a dd magic tag
//! (bit 16 = "the host GPU should also RENDER into this surface" — rung 3). This is the exact
//! contract the legacy hand-written `dd-display/src/server.rs` uses; this module is its Smithay
//! equivalent, so a guest built for the legacy path presents unchanged on `DD_DISPLAY_SMITHAY=1`.
//!
//! ## The bridge
//! 1. We advertise `zwp_linux_dmabuf_v1` (Smithay's [`DmabufState`], v4/v5 with feedback) exporting
//!    ARGB8888/XRGB8888 with the dd IOSurface modifier, and the synthetic render-node
//!    `main_device` (226:128) so a client's ozone/EGL GPU probe resolves the same node the engine
//!    synthesizes. Chrome needs the feedback path; glmark2/es2tri use the plain v3 modifier list
//!    Smithay derives from the same formats.
//! 2. On import ([`DmabufHandler::dmabuf_imported`]) we decode the IOSurface id from the modifier
//!    and accept the buffer; a buffer with no dd tag (a real LINEAR allocation we cannot back on
//!    macOS) is rejected so the client falls back to `wl_shm`.
//! 3. On commit, [`DdState::dmabuf_surface_buffer`] turns the committed dmabuf `wl_buffer` into a
//!    [`SurfaceBuffer`] carrying only `iosurface_id` (no CPU pixels). The reused `Presenter`
//!    (`MetalPresenter`/`CocoaPresenter`) resolves that id to the host IOSurface via the GPU mach
//!    bridge and composites it zero-copy — identical to the legacy GL path.
//!
//! ## Buffer release / fencing
//! The commit path takes Smithay's buffer assignment and creates an explicit generation-tagged use,
//! preventing Smithay's generic release-on-next-attach policy from recycling an IOSurface after a
//! failed present. Accepted delivery completes the active use exactly once; failed/offscreen uses are
//! retained. The Presenter ABI still lacks a true GPU-completion fence, so scheduled Metal delivery is
//! documented as a residual rather than pretending `present()` return proves command-buffer completion.

use dd_display::present::SurfaceBuffer;

use smithay::{
    backend::allocator::{dmabuf::Dmabuf, Buffer as _, Format, Fourcc, Modifier},
    reexports::wayland_server::{protocol::wl_buffer::WlBuffer, DisplayHandle},
    utils::{Logical, Size},
    wayland::dmabuf::{
        get_dmabuf, DmabufDeviceId, DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal,
        DmabufHandler, DmabufState, ImportNotifier,
    },
};

use crate::{handlers::compositor::logical_size_and_uv, DdState};

/// dd-private dmabuf modifier layout (shared with `dd-display/src/server.rs`): `modifier_lo` = the
/// host IOSurface id; `modifier_hi & 0xffff` = this magic tag identifying a dd IOSurface-backed
/// buffer; `modifier_hi & DD_DMABUF_RENDER_BIT` = the guest asked the host GPU to render into it.
pub(crate) const DD_DMABUF_MOD_MAGIC: u32 = 0x6464;
pub(crate) const DD_DMABUF_RENDER_BIT: u32 = 0x1_0000;

/// Decode a DRM format modifier into `(iosurface_id, gpu_render)` when it carries the dd IOSurface
/// tag, else `None` (a modifier we cannot back — e.g. a genuine `LINEAR` allocation).
pub(crate) fn dd_iosurface_from_modifier(modifier: u64) -> Option<(u32, bool)> {
    let hi = (modifier >> 32) as u32;
    let lo = modifier as u32;
    if hi & 0xffff == DD_DMABUF_MOD_MAGIC {
        Some((lo, hi & DD_DMABUF_RENDER_BIT != 0))
    } else {
        None
    }
}

impl DmabufHandler for DdState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    /// A client finished a `zwp_linux_buffer_params_v1` create/create_immed. Accept the buffer iff
    /// its modifier carries a dd IOSurface id (which we resolve zero-copy at present time); reject
    /// anything else so the client renders via `wl_shm` instead. `successful` binds the [`Dmabuf`]
    /// as the `wl_buffer`'s user-data, so [`get_dmabuf`] recovers it on commit — no side table.
    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let modifier: u64 = dmabuf.format().modifier.into();
        match dd_iosurface_from_modifier(modifier) {
            Some((iosurface_id, _gpu_render)) => {
                // Accelerated-import readiness gate (supersedes the interim warn-only check). A dd-tagged
                // dmabuf means the client expects the host GPU to render/present into a dd IOSurface, so
                // REJECT the import up front unless BOTH hold:
                //   (a) a healthy host executor is running (crate::gpu::executor_healthy) — otherwise the
                //       frames have nowhere to render and the window shows white; and
                //   (b) the referenced IOSurface validates (non-zero id + representable dimensions/format,
                //       via validate_iosurface) — otherwise we would accept a handle nothing can resolve.
                // Rejecting (`notifier.failed()`) makes the client fall back to wl_shm instead of
                // rendering into a surface the compositor can never present.
                if !crate::gpu::executor_healthy() {
                    // Keep the once-only human diagnostic (also asserted by the source gate) alongside the
                    // now-actionable rejection.
                    crate::gpu::warn_if_accel_client_without_executor();
                    notifier.failed();
                    return;
                }
                if !self.validate_iosurface(&dmabuf, iosurface_id) {
                    eprintln!(
                        "dd-compositor: rejecting accelerated dmabuf import: IOSurface id \
                         {iosurface_id} failed validation ({}x{}, code {:?})",
                        dmabuf.width(),
                        dmabuf.height(),
                        dmabuf.format().code
                    );
                    notifier.failed();
                    return;
                }
                let _ = notifier.successful::<DdState>();
            }
            None => notifier.failed(),
        }
    }
}

impl DdState {
    /// Validate a dd IOSurface-backed dmabuf at IMPORT time, before the buffer is accepted. Offline this
    /// proves the reference is structurally sound — a non-zero IOSurface id and a representable
    /// size/format — so a malformed or zero handle is rejected instead of accepted and later presented as
    /// white. The deep host check (the IOSurface actually exists in the Metal registry at these
    /// dimensions) needs the live GPU bridge and is revalidated at present time: the Metal presenter
    /// resolves the id and returns a `PresentError::Device` if it is gone, which the compositor then
    /// paces as a failed present. Returns `false` to reject the import.
    pub(crate) fn validate_iosurface(&self, dmabuf: &Dmabuf, iosurface_id: u32) -> bool {
        if iosurface_id == 0 {
            return false;
        }
        let (w, h) = (dmabuf.width() as i32, dmabuf.height() as i32);
        if w <= 0 || h <= 0 {
            return false;
        }
        matches!(dmabuf.format().code, Fourcc::Argb8888 | Fourcc::Xrgb8888)
    }

    /// Build a [`SurfaceBuffer`] from a committed dmabuf `wl_buffer`: no CPU pixels, just the host
    /// IOSurface id decoded from the modifier plus the viewport-resolved logical size / sample rect.
    /// The `Presenter` wraps the IOSurface as an `MTLTexture` and composites it zero-copy. Returns
    /// `None` for a non-dmabuf buffer (the caller falls through to the `wl_shm` path) or a dmabuf
    /// whose modifier lacks the dd tag (should not occur — such imports are rejected).
    pub(crate) fn dmabuf_surface_buffer(
        &self,
        sid: u32,
        buffer: &WlBuffer,
        buffer_scale: i32,
        dst: Option<Size<i32, Logical>>,
        src: Option<(f64, f64, f64, f64)>,
    ) -> Option<SurfaceBuffer> {
        let dmabuf = get_dmabuf(buffer).ok()?;
        let modifier: u64 = dmabuf.format().modifier.into();
        let (iosurface_id, gpu_render) = dd_iosurface_from_modifier(modifier)?;
        let tex_w = dmabuf.width() as i32;
        let tex_h = dmabuf.height() as i32;
        if tex_w <= 0 || tex_h <= 0 {
            return None;
        }
        // dd-display convention: format == 1 ⇒ opaque (XRGB); 0 ⇒ honour alpha (ARGB / anything else).
        let format = match dmabuf.format().code {
            Fourcc::Xrgb8888 => 1u32,
            _ => 0u32,
        };
        let (width, height, uv_rect) = logical_size_and_uv(dst, src, tex_w, tex_h, buffer_scale);
        let title = self.titles.get(&sid).cloned().unwrap_or_else(|| "dd".into());
        Some(SurfaceBuffer {
            sid,
            width,
            height,
            texture_width: tex_w,
            texture_height: tex_h,
            stride: tex_w * 4,
            format,
            bgra: Vec::new(),
            title,
            iosurface_id: Some(iosurface_id),
            gpu_render,
            uv_rect,
            // Zero-copy IOSurface: the host texture is shared, not re-uploaded, so partial-damage upload
            // does not apply here.
            damage: None,
            // The compositor composites popups into their parent's frame (see `present_render_root`),
            // rather than opening a native popup window, so no per-surface popup placement is emitted.
            popup: None,
            overlays: Vec::new(),
        })
    }
}

/// The synthetic DRM render node the dd engine presents to a guest: `/dev/dri/renderD128`, whose
/// `st_rdev` is `makedev(226, 128) == (226 << 8) | 128`. A guest GPU client (Chromium's ozone/GPU)
/// reads the dmabuf-feedback `main_device`, `stat`s that node, and takes its accelerated render path
/// only when the two match — so the feedback must advertise exactly this dev_t (identical to the
/// legacy `dd-display/src/server.rs::send_dmabuf_feedback` value).
const DD_MAIN_DEVICE: DmabufDeviceId = DmabufDeviceId::from_linux_dev_t((226u64 << 8) | 128);

/// The exact ARGB/XRGB8888 format+modifier pairs the importer can accept. Genuine Linux `LINEAR`
/// dma-bufs are intentionally absent because the macOS importer only resolves dd IOSurface references. GLES clients
/// (glmark2/es2tri) read this list off the v3 modifier events; v4+ clients read the same set from the
/// feedback format-table's main tranche.
fn dd_dmabuf_formats() -> [Format; 2] {
    let magic = Modifier::from((DD_DMABUF_MOD_MAGIC as u64) << 32);
    [
        Format { code: Fourcc::Argb8888, modifier: magic },
        Format { code: Fourcc::Xrgb8888, modifier: magic },
    ]
}

/// Build the default [`DmabufFeedback`] for the v4/v5 global: a single main tranche of
/// [`dd_dmabuf_formats`] targeting [`DD_MAIN_DEVICE`]. Returns `Err` if the format-table backing file
/// cannot be created — on macOS this is the `PSHMNAMLEN` failure the offline-vendored smithay patch
/// fixes (`third_party/smithay-0.7.0/src/utils/sealed_file.rs`); the caller falls back to a v3 global.
pub(crate) fn build_default_feedback() -> std::io::Result<DmabufFeedback> {
    DmabufFeedbackBuilder::new(DD_MAIN_DEVICE, dd_dmabuf_formats()).build()
}

/// Stand up the `zwp_linux_dmabuf_v1` global and return the [`DmabufState`] delegate. Called once from
/// [`DdState::new`] (only under `DD_DISPLAY_SMITHAY=1`, since that flag is what execs this compositor,
/// so the whole path — v3 or v4 — is already behind it; the legacy `server.rs` default is untouched).
///
/// Preferred: a **version 5** global carrying a default dmabuf-**feedback** (which serves the v4
/// feedback protocol Chromium's ozone/GPU needs to resolve its DRM render node via `main_device` —
/// mirroring `dd-display/src/server.rs::send_dmabuf_feedback`). v3-and-lower binders still receive the
/// same ARGB/XRGB8888 `modifier` events from the feedback's main tranche, so GLES clients
/// (glmark2/es2tri) are unaffected.
///
/// Fallback: if the feedback format-table cannot be built (its `shm_open`ed backing file — on macOS
/// this used to overflow `PSHMNAMLEN`, now fixed in the vendored smithay), we log and fall back to the
/// v3 global so the compositor still comes up with the modifier list. Success/failure is observable via
/// the advertised global version (5 with feedback, 3 without).
pub(crate) fn new_dmabuf_state(dh: &DisplayHandle) -> DmabufState {
    let mut state = DmabufState::new();
    // The table is now guest-mappable and byte-correct, but the private modifier still embeds a dynamic
    // IOSurface id while Wayland feedback describes exact stable format/modifier pairs. Keep the global
    // opt-in until allocation identity moves to validated fd metadata or a versioned private channel.
    if std::env::var("DD_DISPLAY_DMABUF").is_err() {
        return state;
    }
    match build_default_feedback() {
        Ok(feedback) => {
            // The returned DmabufGlobal is only a handle; the global (and a clone of the feedback) live
            // inside `state`, so dropping it does not remove the global.
            let _global = state.create_global_with_default_feedback::<DdState>(dh, &feedback);
        }
        Err(e) => {
            eprintln!(
                "dd-compositor: zwp_linux_dmabuf v4 feedback format-table could not be created \
                 ({e}); falling back to the v3 modifier-list global (no accelerated-Chromium \
                 render-node feedback)"
            );
            let _global = state.create_global::<DdState>(dh, dd_dmabuf_formats());
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v4/v5 dmabuf-feedback format-table must build on the host without a `PSHMNAMLEN`/
    /// `ENAMETOOLONG` failure — the macOS limitation the vendored smithay patch (shortening the
    /// `shm_open` object name in `sealed_file.rs`) exists to fix. If this fails on macOS, the
    /// compositor silently regresses to advertising `zwp_linux_dmabuf` v3 only.
    #[test]
    fn dmabuf_feedback_format_table_builds_under_pshmnamlen() {
        let feedback = build_default_feedback();
        assert!(
            feedback.is_ok(),
            "dmabuf-feedback format-table SealedFile failed to build (macOS PSHMNAMLEN regression?): {:?}",
            feedback.err()
        );
    }
}
