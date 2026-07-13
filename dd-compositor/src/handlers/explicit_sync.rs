//! `zwp_linux_explicit_synchronization_v1` — a real per-surface explicit-sync fence contract.
//!
//! This is the Wayland side of the row `compositor_explicit_sync_waits_acquire_before_sampling_and_
//! releases_after_gpu_completion`: it is NOT just internal Metal ordering, it is the protocol contract a
//! GPU client (GLES/Vulkan) uses to hand the compositor an **acquire fence** (a `dma_fence` sync_file fd)
//! that the compositor MUST wait on before it samples the just-committed buffer, and a **release** object
//! the compositor signals only AFTER its own GPU work on that buffer has completed, so the client may
//! recycle the buffer without a data race.
//!
//! Smithay's own `drm_syncobj` implementation is gated behind the DRM backend (a real `DrmDeviceFd`),
//! which the `wayland_frontend`-only dd-compositor build does not link. So this module implements the
//! older, device-independent `zwp_linux_explicit_synchronization_v1` protocol directly (manual
//! `Dispatch`/`GlobalDispatch` on [`DdState`]), storing the per-commit acquire fence + release object and
//! exposing the two contract methods the present path drives:
//!
//!   * [`DdState::take_acquire_fence`] — call BEFORE sampling a surface's buffer; returns the committed
//!     acquire fence to wait on (see [`wait_acquire_fence`], a real pollable-fd wait, Linux-testable).
//!   * [`DdState::signal_buffer_release`] — call AFTER the compositor's GPU work on the buffer completes;
//!     sends `fenced_release`(completion fence) or `immediate_release` to the client.
//!
//! On macOS the acquire wait and the release fence bridge to Metal via
//! `dd_display::explicit_sync_bridge` (an `MTLSharedEvent`); that interop is mac-gated and not exercised
//! by the Linux protocol tests.

use std::collections::{HashMap, HashSet};
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};

use smithay::reexports::wayland_protocols::wp::linux_explicit_synchronization::zv1::server::{
    zwp_linux_buffer_release_v1::{self, ZwpLinuxBufferReleaseV1},
    zwp_linux_explicit_synchronization_v1::{self, ZwpLinuxExplicitSynchronizationV1},
    zwp_linux_surface_synchronization_v1::{self, Error as SurfaceSyncError, ZwpLinuxSurfaceSynchronizationV1},
};
use smithay::reexports::wayland_server::{
    backend::GlobalId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle,
    GlobalDispatch, New, Resource, Weak as WlWeak,
};
use smithay::wayland::compositor::{add_pre_commit_hook, with_states, BufferAssignment, SurfaceAttributes};

use crate::DdState;

/// The `zwp_linux_explicit_synchronization_v1` version advertised (v2; the requests this compositor
/// handles — `get_synchronization`/`set_acquire_fence`/`get_release` — are all present since v1).
const EXPLICIT_SYNC_VERSION: u32 = 2;

/// Per-`zwp_linux_surface_synchronization_v1` user data: the surface it was created for.
pub struct SurfaceSyncData {
    surface: WlWeak<WlSurface>,
}

/// The acquire fence + release object a client set for the NEXT commit of a surface.
#[derive(Default)]
struct PendingSync {
    /// The `dma_fence` sync_file fd the compositor must wait on before sampling the committed buffer.
    acquire: Option<OwnedFd>,
    /// The release object the compositor signals after its GPU use of the committed buffer completes.
    release: Option<ZwpLinuxBufferReleaseV1>,
}

/// The acquire fence + release object bound to the CURRENTLY committed buffer of a surface.
#[derive(Default)]
struct CommittedSync {
    acquire: Option<OwnedFd>,
    release: Option<ZwpLinuxBufferReleaseV1>,
}

/// Aggregate explicit-sync state, held in [`DdState`].
pub struct ExplicitSyncState {
    #[allow(dead_code)]
    global: GlobalId,
    /// Surfaces that currently own a `zwp_linux_surface_synchronization_v1` (enforces the one-per-surface
    /// `synchronization_exists` rule). Keyed by surface id.
    syncs: HashMap<u32, WlWeak<ZwpLinuxSurfaceSynchronizationV1>>,
    /// Acquire fence + release staged by the client for each surface's next commit.
    pending: HashMap<u32, PendingSync>,
    /// Acquire fence + release bound to each surface's current (committed) buffer.
    committed: HashMap<u32, CommittedSync>,
    /// Surfaces that already have the pre-commit hook installed (install once).
    hooked: HashSet<u32>,
}

impl ExplicitSyncState {
    pub fn new(dh: &DisplayHandle) -> Self {
        let global = dh.create_global::<DdState, ZwpLinuxExplicitSynchronizationV1, ()>(
            EXPLICIT_SYNC_VERSION,
            (),
        );
        Self {
            global,
            syncs: HashMap::new(),
            pending: HashMap::new(),
            committed: HashMap::new(),
            hooked: HashSet::new(),
        }
    }
}

impl DdState {
    /// Surfaces (by sid) that currently have committed explicit-sync fences — the present path iterates
    /// these to wait each acquire fence before sampling and to signal each release after GPU completion.
    pub fn explicit_sync_committed_sids(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.explicit_sync.committed.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// Whether `sid` has a live acquire fence staged/committed but not yet consumed (test/present query).
    pub fn has_pending_acquire(&self, sid: u32) -> bool {
        self.explicit_sync.committed.get(&sid).map(|c| c.acquire.is_some()).unwrap_or(false)
    }

    /// Whether `sid` has a committed release object still owed a signal (test/present query).
    pub fn has_committed_release(&self, sid: u32) -> bool {
        self.explicit_sync.committed.get(&sid).map(|c| c.release.is_some()).unwrap_or(false)
    }

    /// Take the acquire fence bound to a surface's current buffer. The present path calls this BEFORE it
    /// samples the buffer; the returned fence (if any) must be waited on (see [`wait_acquire_fence`] /
    /// the mac `MTLSharedEvent` bridge) so the compositor never reads pixels the client's GPU has not yet
    /// finished producing.
    pub fn take_acquire_fence(&mut self, sid: u32) -> Option<OwnedFd> {
        self.explicit_sync.committed.get_mut(&sid).and_then(|c| c.acquire.take())
    }

    /// Signal the release object bound to a surface's current buffer AFTER the compositor's GPU work on it
    /// has completed. `completion` is the compositor's own completion fence (a Metal-completion sync_file
    /// on the real host); when present the client receives `fenced_release(fence)` and may wait on it,
    /// otherwise `immediate_release` tells the client the buffer is already free. Both are destructor
    /// events, so the release object is consumed. Returns `true` if a release was owed and signalled.
    pub fn signal_buffer_release(&mut self, sid: u32, completion: Option<OwnedFd>) -> bool {
        let release = match self.explicit_sync.committed.get_mut(&sid).and_then(|c| c.release.take()) {
            Some(r) => r,
            None => return false,
        };
        match completion {
            Some(fence) => release.fenced_release(fence.as_fd()),
            None => release.immediate_release(),
        }
        true
    }

    /// Pre-commit hook body: enforce the buffer/fence coupling and roll the staged acquire/release into
    /// the committed slot so the present path sees exactly the fences that belong to the new buffer.
    fn explicit_sync_pre_commit(&mut self, surface: &WlSurface, resource: &ZwpLinuxSurfaceSynchronizationV1) {
        let sid = self.surface_id(surface);
        let pending = self.explicit_sync.pending.remove(&sid).unwrap_or_default();
        if pending.acquire.is_none() && pending.release.is_none() {
            return; // no explicit-sync activity this commit
        }
        // A commit that carries an acquire fence or a release MUST attach a buffer for them to apply to.
        let has_new_buffer = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            matches!(cached.pending().buffer, Some(BufferAssignment::NewBuffer(_)))
        });
        if !has_new_buffer {
            resource.post_error(SurfaceSyncError::NoBuffer as u32, "acquire/release without an attached buffer");
            return;
        }
        // Any release still owed on the previous buffer is dropped here (superseded by the new commit);
        // signalling it immediately keeps the client from stalling on a buffer we will never read again.
        if let Some(prev) = self.explicit_sync.committed.remove(&sid) {
            if let Some(r) = prev.release {
                r.immediate_release();
            }
        }
        self.explicit_sync.committed.insert(sid, CommittedSync { acquire: pending.acquire, release: pending.release });
    }

    fn arm_explicit_sync_hook(&mut self, surface: &WlSurface, obj: &ZwpLinuxSurfaceSynchronizationV1) {
        let sid = self.surface_id(surface);
        if !self.explicit_sync.hooked.insert(sid) {
            return;
        }
        let obj = obj.downgrade();
        add_pre_commit_hook::<DdState, _>(surface, move |state, _dh, surface| {
            if let Ok(resource) = obj.upgrade() {
                state.explicit_sync_pre_commit(surface, &resource);
            }
        });
    }
}

// ---- zwp_linux_explicit_synchronization_v1 (the manager global) ------------------------------------

impl GlobalDispatch<ZwpLinuxExplicitSynchronizationV1, ()> for DdState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpLinuxExplicitSynchronizationV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpLinuxExplicitSynchronizationV1, ()> for DdState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpLinuxExplicitSynchronizationV1,
        request: zwp_linux_explicit_synchronization_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwp_linux_explicit_synchronization_v1::{Error, Request};
        match request {
            Request::GetSynchronization { id, surface } => {
                let sid = state.surface_id(&surface);
                let exists = state
                    .explicit_sync
                    .syncs
                    .get(&sid)
                    .map(|w| w.upgrade().is_ok())
                    .unwrap_or(false);
                if exists {
                    resource.post_error(
                        Error::SynchronizationExists as u32,
                        "the surface already has a synchronization object",
                    );
                    return;
                }
                let obj = data_init.init(id, SurfaceSyncData { surface: surface.downgrade() });
                state.explicit_sync.syncs.insert(sid, obj.downgrade());
                state.arm_explicit_sync_hook(&surface, &obj);
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

// ---- zwp_linux_surface_synchronization_v1 (per-surface acquire/release) -----------------------------

impl Dispatch<ZwpLinuxSurfaceSynchronizationV1, SurfaceSyncData> for DdState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpLinuxSurfaceSynchronizationV1,
        request: zwp_linux_surface_synchronization_v1::Request,
        data: &SurfaceSyncData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwp_linux_surface_synchronization_v1::Request;
        // Every request but `destroy` requires the surface to still be alive.
        let surface = match data.surface.upgrade() {
            Ok(s) => s,
            Err(_) => {
                if !matches!(request, Request::Destroy) {
                    resource.post_error(SurfaceSyncError::NoSurface as u32, "the associated surface was destroyed");
                }
                return;
            }
        };
        let sid = state.surface_id(&surface);
        match request {
            Request::SetAcquireFence { fd } => {
                let pend = state.explicit_sync.pending.entry(sid).or_default();
                if pend.acquire.is_some() {
                    resource.post_error(SurfaceSyncError::DuplicateFence as u32, "acquire fence already set this commit");
                    return;
                }
                pend.acquire = Some(fd);
            }
            Request::GetRelease { release } => {
                if state.explicit_sync.pending.get(&sid).map(|p| p.release.is_some()).unwrap_or(false) {
                    resource.post_error(SurfaceSyncError::DuplicateRelease as u32, "release already requested this commit");
                    return;
                }
                let rel = data_init.init(release, ());
                state.explicit_sync.pending.entry(sid).or_default().release = Some(rel);
            }
            Request::Destroy => {
                state.explicit_sync.syncs.remove(&sid);
            }
            _ => {}
        }
    }
}

// ---- zwp_linux_buffer_release_v1 (server→client only; no requests) ----------------------------------

impl Dispatch<ZwpLinuxBufferReleaseV1, ()> for DdState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwpLinuxBufferReleaseV1,
        _request: zwp_linux_buffer_release_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

/// Wait for an acquire fence (a `dma_fence` sync_file, or any pollable fd) to signal, up to `timeout_ms`.
/// A sync_file becomes readable (`POLLIN`) when its fence is signalled — the same edge an eventfd/pipe
/// gives, which is what the Linux protocol tests drive. Returns `Ok(true)` if it signalled, `Ok(false)`
/// on timeout. This is the CPU-side wait; on macOS the compositor instead bridges the fence into an
/// `MTLSharedEvent` wait on the Metal queue (see `dd_display::explicit_sync_bridge`).
pub fn wait_acquire_fence(fence: BorrowedFd<'_>, timeout_ms: i32) -> std::io::Result<bool> {
    // SAFETY: `pollfd` is a plain C struct; `fence` is a valid borrowed fd for the call's duration.
    let mut pfd = libc::pollfd { fd: fence.as_raw_fd_compat(), events: libc::POLLIN, revents: 0 };
    let rc = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, timeout_ms) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(rc > 0 && (pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0)
}

/// Small shim so `wait_acquire_fence` doesn't need to import `AsRawFd` at call sites.
trait AsRawFdCompat {
    fn as_raw_fd_compat(&self) -> std::os::unix::io::RawFd;
}
impl AsRawFdCompat for BorrowedFd<'_> {
    fn as_raw_fd_compat(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.as_raw_fd()
    }
}
