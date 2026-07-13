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

use std::os::fd::AsRawFd;

use dd_display::present::{IOSurfaceMetadata, SurfaceBuffer};

use smithay::{
    backend::allocator::{
        dmabuf::{Dmabuf, DmabufFlags},
        Buffer as _, Format, Fourcc, Modifier,
    },
    reexports::wayland_server::{protocol::wl_buffer::WlBuffer, DisplayHandle},
    utils::{Logical, Size},
    wayland::dmabuf::{
        get_dmabuf, DmabufDeviceId, DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal,
        DmabufHandler, DmabufState, ImportNotifier,
    },
};

use crate::{handlers::compositor::logical_size_and_uv, DdState};

/// dd-private dmabuf modifier layout (shared with `dd-display/src/server.rs` and the guest shims'
/// modifier producers). `modifier_lo` = the host IOSurface id. `modifier_hi` packs:
///   - bits 0..=15  `DD_DMABUF_MOD_MAGIC` — the tag identifying a dd IOSurface-backed buffer;
///   - bit  16      `DD_DMABUF_RENDER_BIT` — the guest asked the host GPU to render into it;
///   - bits 17..=31 the **allocation generation** (`DD_DMABUF_GEN_*`) — see below.
///
/// The IOSurface `id` (`modifier_lo`) is a macOS-minted `IOSurfaceGetID` that dd RECYCLES: a retired
/// pool surface is re-handed to the next same-size allocation, and a released legacy id becomes
/// OS-recyclable. So an id alone cannot distinguish a live allocation from a stale reference whose
/// backing surface was retired and reissued under the same id. The generation authenticates that: the
/// host stamps each id's current allocation with a monotonically bumped generation, the guest echoes
/// the generation it was given in `modifier_hi`, and import rejects a generation that no longer matches
/// the id's live allocation (`DmabufValidationError::StaleGeneration`).
///
/// Generation `0` is reserved as "unversioned": a guest that does not source a generation sends 0, and
/// the check is skipped for it (backward compatibility — no false rejections for legacy producers).
pub(crate) const DD_DMABUF_MOD_MAGIC: u32 = 0x6464;
pub(crate) const DD_DMABUF_RENDER_BIT: u32 = 0x1_0000;
/// Bit offset of the allocation generation within `modifier_hi`.
pub(crate) const DD_DMABUF_GEN_SHIFT: u32 = 17;
/// 15-bit generation field mask (post-shift): values 1..=0x7fff cycle; 0 == unversioned.
pub(crate) const DD_DMABUF_GEN_MASK: u32 = 0x7fff;

/// Compose `modifier_hi` from its parts (used by the guest producers and tests). `generation` is masked
/// to the 15-bit field; pass 0 for an unversioned (legacy) buffer.
pub(crate) fn compose_modifier_hi(gpu_render: bool, generation: u32) -> u32 {
    DD_DMABUF_MOD_MAGIC
        | if gpu_render { DD_DMABUF_RENDER_BIT } else { 0 }
        | ((generation & DD_DMABUF_GEN_MASK) << DD_DMABUF_GEN_SHIFT)
}

#[derive(Clone, Copy)]
struct DmabufImportCaps {
    format: Fourcc,
}

const DMABUF_IMPORT_CAPS: [DmabufImportCaps; 2] = [
    DmabufImportCaps { format: Fourcc::Argb8888 },
    DmabufImportCaps { format: Fourcc::Xrgb8888 },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DmabufValidationError {
    UnsupportedPair,
    InvalidIdentity,
    InvalidFlags,
    InvalidPlaneCount,
    InvalidPlaneIndex,
    InvalidOffset,
    InvalidStride,
    BackingTooSmall,
    MissingMetadata,
    MetadataMismatch,
    /// The modifier's allocation generation does not match the id's live host allocation — a stale
    /// reference to a surface that was retired and whose id was reissued.
    StaleGeneration,
}

fn modifier_has_dd_layout(modifier: u64) -> bool {
    ((modifier >> 32) as u32) & 0xffff == DD_DMABUF_MOD_MAGIC
}

fn supports_pair(format: Fourcc, modifier: u64) -> bool {
    modifier_has_dd_layout(modifier) && DMABUF_IMPORT_CAPS.iter().any(|caps| caps.format == format)
}

/// The allocation generation stamped in `modifier_hi` (0 == unversioned/legacy).
pub(crate) fn modifier_generation(modifier: u64) -> u32 {
    ((modifier >> 32) as u32 >> DD_DMABUF_GEN_SHIFT) & DD_DMABUF_GEN_MASK
}

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
                if let Err(reason) = self.validate_iosurface(&dmabuf, iosurface_id) {
                    eprintln!(
                        "dd-compositor: rejecting accelerated dmabuf import: IOSurface id \
                         {iosurface_id} failed validation ({reason:?}, {}x{}, code {:?})",
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

fn validate_dmabuf(
    dmabuf: &Dmabuf,
    iosurface_id: u32,
    metadata: Option<IOSurfaceMetadata>,
) -> Result<(), DmabufValidationError> {
    let modifier: u64 = dmabuf.format().modifier.into();
    if !supports_pair(dmabuf.format().code, modifier) {
        return Err(DmabufValidationError::UnsupportedPair);
    }
    if iosurface_id == 0 {
        return Err(DmabufValidationError::InvalidIdentity);
    }
    if dmabuf.flags() != DmabufFlags::empty() {
        return Err(DmabufValidationError::InvalidFlags);
    }
    if dmabuf.num_planes() != 1 {
        return Err(DmabufValidationError::InvalidPlaneCount);
    }
    if dmabuf.plane_indices().next() != Some(0) {
        return Err(DmabufValidationError::InvalidPlaneIndex);
    }
    let offset = dmabuf.offsets().next().ok_or(DmabufValidationError::InvalidPlaneCount)?;
    if offset != 0 {
        return Err(DmabufValidationError::InvalidOffset);
    }
    let (width, height) = (dmabuf.width(), dmabuf.height());
    if width == 0 || height == 0 {
        return Err(DmabufValidationError::InvalidStride);
    }
    let min_stride = width.checked_mul(4).ok_or(DmabufValidationError::InvalidStride)?;
    let stride = dmabuf.strides().next().ok_or(DmabufValidationError::InvalidPlaneCount)?;
    if stride < min_stride {
        return Err(DmabufValidationError::InvalidStride);
    }
    let required = (stride as u64)
        .checked_mul(height as u64)
        .and_then(|bytes| bytes.checked_add(offset as u64))
        .ok_or(DmabufValidationError::BackingTooSmall)?;
    let plane = dmabuf.handles().next().ok_or(DmabufValidationError::InvalidPlaneCount)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(plane.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(DmabufValidationError::BackingTooSmall);
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_size < 0 || (stat.st_size as u64) < required {
        return Err(DmabufValidationError::BackingTooSmall);
    }
    let metadata = metadata.ok_or(DmabufValidationError::MissingMetadata)?;
    if metadata.width != width
        || metadata.height != height
        || metadata.bytes_per_row != stride
        || metadata.pixel_format != 0x4247_5241
    {
        return Err(DmabufValidationError::MetadataMismatch);
    }
    // Allocation-generation authentication: a versioned guest (non-zero generation) must match the id's
    // CURRENT live allocation generation. A mismatch is a stale reference — the surface this id named
    // was retired and its id reissued — so reject before the buffer is accepted (no partial state; the
    // caller has not bound the wl_buffer yet). Generation 0 is unversioned (legacy) and skips the check.
    let guest_generation = modifier_generation(modifier);
    if guest_generation != 0 && guest_generation != metadata.generation {
        return Err(DmabufValidationError::StaleGeneration);
    }
    Ok(())
}

impl DdState {
    /// Validate a dd IOSurface-backed dmabuf at IMPORT time, before the buffer is accepted. Offline this
    /// proves the reference is structurally sound — a non-zero IOSurface id and a representable
    /// size/format — so a malformed or zero handle is rejected instead of accepted and later presented as
    /// white. The deep host check (the IOSurface actually exists in the Metal registry at these
    /// dimensions) needs the live GPU bridge and is revalidated at present time: the Metal presenter
    /// resolves the id and returns a `PresentError::Device` if it is gone, which the compositor then
    /// paces as a failed present. Returns `false` to reject the import.
    pub(crate) fn validate_iosurface(
        &self,
        dmabuf: &Dmabuf,
        iosurface_id: u32,
    ) -> Result<(), DmabufValidationError> {
        validate_dmabuf(dmabuf, iosurface_id, self.presenter.iosurface_metadata(iosurface_id))
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
    DMABUF_IMPORT_CAPS.map(|caps| Format { code: caps.format, modifier: magic })
}

/// Build the default [`DmabufFeedback`] for the v4/v5 global: a single main tranche of
/// [`dd_dmabuf_formats`] targeting [`DD_MAIN_DEVICE`]. Returns `Err` if the format-table backing file
/// cannot be created — on macOS this is the `PSHMNAMLEN` failure the offline-vendored smithay patch
/// fixes (`vendor/smithay-0.7.0/src/utils/sealed_file.rs`); the caller falls back to a v3 global.
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
    use std::os::fd::{FromRawFd, OwnedFd};

    /// A plane fd of EXACTLY `size` bytes for the backing-size validation. Backed by an unlinked regular
    /// temp file, not a POSIX shm object: macOS rounds a shm `ftruncate` up to the page size, so an shm
    /// fd's `fstat` over-reports and the `BackingTooSmall` guard (which `fstat`s the plane fd) could not
    /// be exercised with a sub-page size. A regular file reports its exact `ftruncate` size.
    fn plane_fd(size: usize) -> OwnedFd {
        let dir = std::env::var_os("TMPDIR")
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/tmp".into());
        let mut templ: Vec<u8> =
            format!("{}/dd-plane-XXXXXX", dir.trim_end_matches('/')).into_bytes();
        templ.push(0);
        let fd = unsafe { libc::mkstemp(templ.as_mut_ptr() as *mut libc::c_char) };
        assert!(fd >= 0, "mkstemp plane fd");
        assert_eq!(unsafe { libc::ftruncate(fd, size as libc::off_t) }, 0, "ftruncate plane fd");
        unsafe { libc::unlink(templ.as_ptr() as *const libc::c_char) };
        unsafe { OwnedFd::from_raw_fd(fd) }
    }

    fn fixture(
        format: Fourcc,
        flags: DmabufFlags,
        plane_idx: u32,
        offset: u32,
        stride: u32,
        backing: usize,
        second_plane: bool,
    ) -> Dmabuf {
        let modifier = Modifier::from(((DD_DMABUF_MOD_MAGIC as u64) << 32) | 7);
        let mut builder = Dmabuf::builder((16, 8), format, modifier, flags);
        assert!(builder.add_plane(plane_fd(backing), plane_idx, offset, stride));
        if second_plane {
            assert!(builder.add_plane(plane_fd(backing), 1, 0, stride));
        }
        builder.build().unwrap()
    }

    fn metadata() -> IOSurfaceMetadata {
        IOSurfaceMetadata {
            width: 16,
            height: 8,
            bytes_per_row: 64,
            pixel_format: 0x4247_5241,
            generation: 0,
        }
    }

    /// A valid Argb8888 fixture whose modifier carries the given allocation `generation` (id `mod_lo`=7).
    fn fixture_generation(generation: u32) -> Dmabuf {
        let modifier =
            Modifier::from(((compose_modifier_hi(false, generation) as u64) << 32) | 7);
        let mut builder = Dmabuf::builder((16, 8), Fourcc::Argb8888, modifier, DmabufFlags::empty());
        assert!(builder.add_plane(plane_fd(512), 0, 0, 64));
        builder.build().unwrap()
    }

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

    #[test]
    fn dmabuf_import_validation_rejects_every_untrusted_plane_and_metadata_dimension() {
        let valid = fixture(Fourcc::Argb8888, DmabufFlags::empty(), 0, 0, 64, 512, false);
        assert_eq!(validate_dmabuf(&valid, 7, Some(metadata())), Ok(()));
        assert_eq!(
            validate_dmabuf(&valid, 0, Some(metadata())),
            Err(DmabufValidationError::InvalidIdentity)
        );
        assert_eq!(
            validate_dmabuf(&valid, 7, None),
            Err(DmabufValidationError::MissingMetadata)
        );
        assert_eq!(
            validate_dmabuf(
                &valid,
                7,
                Some(IOSurfaceMetadata { width: 15, ..metadata() }),
            ),
            Err(DmabufValidationError::MetadataMismatch)
        );

        let cases = [
            (
                fixture(Fourcc::Nv12, DmabufFlags::empty(), 0, 0, 64, 512, false),
                DmabufValidationError::UnsupportedPair,
            ),
            (
                fixture(Fourcc::Argb8888, DmabufFlags::Y_INVERT, 0, 0, 64, 512, false),
                DmabufValidationError::InvalidFlags,
            ),
            (
                fixture(Fourcc::Argb8888, DmabufFlags::empty(), 0, 0, 64, 512, true),
                DmabufValidationError::InvalidPlaneCount,
            ),
            (
                fixture(Fourcc::Argb8888, DmabufFlags::empty(), 2, 0, 64, 512, false),
                DmabufValidationError::InvalidPlaneIndex,
            ),
            (
                fixture(Fourcc::Argb8888, DmabufFlags::empty(), 0, 4, 64, 516, false),
                DmabufValidationError::InvalidOffset,
            ),
            (
                fixture(Fourcc::Argb8888, DmabufFlags::empty(), 0, 0, 60, 480, false),
                DmabufValidationError::InvalidStride,
            ),
            (
                fixture(Fourcc::Argb8888, DmabufFlags::empty(), 0, 0, 64, 511, false),
                DmabufValidationError::BackingTooSmall,
            ),
        ];
        for (dmabuf, expected) in cases {
            assert_eq!(validate_dmabuf(&dmabuf, 7, Some(metadata())), Err(expected));
        }
    }

    #[test]
    fn dmabuf_import_authenticates_the_allocation_generation() {
        // The id's live host allocation is at generation 5.
        let live = IOSurfaceMetadata { generation: 5, ..metadata() };

        // A matching versioned reference imports.
        assert_eq!(validate_dmabuf(&fixture_generation(5), 7, Some(live)), Ok(()));

        // A STALE reference — the id was retired and reissued, so its live generation moved on — is
        // rejected before the buffer is accepted (no partial state).
        assert_eq!(
            validate_dmabuf(&fixture_generation(4), 7, Some(live)),
            Err(DmabufValidationError::StaleGeneration),
        );
        assert_eq!(
            validate_dmabuf(&fixture_generation(6), 7, Some(live)),
            Err(DmabufValidationError::StaleGeneration),
        );

        // An unversioned reference (generation 0) skips the check — legacy producers are unaffected, so
        // there are no false rejections while the guest side does not yet source a generation.
        assert_eq!(validate_dmabuf(&fixture_generation(0), 7, Some(live)), Ok(()));
        // ...and it also imports against an untracked (generation-0) host allocation.
        assert_eq!(validate_dmabuf(&fixture_generation(0), 7, Some(metadata())), Ok(()));

        // The modifier generation round-trips through the encode/decode helpers.
        assert_eq!(modifier_generation((compose_modifier_hi(false, 5) as u64) << 32), 5);
        assert_eq!(modifier_generation((compose_modifier_hi(true, 0x7fff) as u64) << 32), 0x7fff);
        // The render bit and generation are independent fields.
        assert!(compose_modifier_hi(true, 3) & DD_DMABUF_RENDER_BIT != 0);
        assert_eq!(modifier_generation((compose_modifier_hi(true, 3) as u64) << 32), 3);
    }
}
