use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroU64;
use std::time::Instant;

use super::*;
use crate::adapter::smithay::native::{NativeFrame, NativeFrameOutcome, NativeFrames};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Key {
    token: NonZeroU64,
    serial: NonZeroU64,
}

pub(super) struct Deferred {
    pub(super) surface: SurfaceId,
    pub(super) commit: Commit,
    pub(super) was_mapped: bool,
    pub(super) min_size: (Option<i32>, Option<i32>),
    pub(super) max_size: (Option<i32>, Option<i32>),
    pub(super) frame_callbacks: Vec<WlCallback>,
    pub(super) feedbacks: Vec<PresentationFeedbackCallback>,
    pub(super) buffer: Option<WlBuffer>,
    pub(super) external: Option<Dmabuf>,
    pub(super) metadata: Option<Metadata>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Metadata {
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) stride: u32,
    pub(super) format: Format,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImportFailure {
    Width,
    Height,
    Stride,
}

impl ImportFailure {
    /// Stable key for counting occurrences of this cause per surface.
    fn as_str(self) -> &'static str {
        match self {
            ImportFailure::Width => "width",
            ImportFailure::Height => "height",
            ImportFailure::Stride => "stride",
        }
    }
}

impl Metadata {
    fn from_surface(surface: &hl_iosurface::Surface) -> Option<Self> {
        let (width, height, stride) = surface.dimensions();
        Some(Self {
            width: i32::try_from(width).ok()?,
            height: i32::try_from(height).ok()?,
            stride: u32::try_from(stride).ok()?,
            // Native frames are explicitly BGRA IOSurfaces. On little-endian hosts that is Wayland's
            // ARGB8888 byte layout; protocol-only associations therefore need no dmabuf metadata.
            format: Format::Argb8888,
        })
    }

    fn failure(self, actual: (usize, usize, usize)) -> Option<ImportFailure> {
        let (width, height, stride) = actual;
        if i32::try_from(width).ok() != Some(self.width) {
            return Some(ImportFailure::Width);
        }
        if i32::try_from(height).ok() != Some(self.height) {
            return Some(ImportFailure::Height);
        }
        // The guest dma-buf and host IOSurface are different storage objects. Rendering repacks the
        // guest image into a BGRA IOSurface, so their row pitches need not match. Validate the host
        // surface against its own tight-row minimum; comparing it with the guest pitch rejected valid
        // odd-width Chrome popups (for example 776px: guest 3136, IOSurface 3104).
        if stride < width.saturating_mul(4) {
            return Some(ImportFailure::Stride);
        }
        None
    }
}

pub(super) struct Ready {
    pub(super) frame: NativeFrame,
    pub(super) deferred: Deferred,
}

pub(super) enum Defer {
    Ready(Ready),
    Reuse(Deferred),
    Waiting,
}

/// Why a deferral did not produce a frame, for the report in [`NativeState::report_deferrals`].
///
/// `Defer::Waiting` is one value covering two entirely different fates: a commit PARKED awaiting the
/// native frame that will complete it, and a commit REFUSED outright (its token cancelled, poisoned,
/// closing, inactive, or its serial already overtaken) and handed straight back to be discarded. They
/// were indistinguishable to every caller and to every reader of the log, which is what made a commit
/// parked forever look exactly like a commit that was never made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeferOutcome {
    /// The frame was already here: joined immediately.
    Joined,
    /// Re-presenting the frame this serial already joined.
    Reused,
    /// Parked, awaiting a native frame for this `(token, serial)`.
    Parked,
    /// Refused and returned for discard. Never parked, so no frame can ever complete it.
    Refused,
}

impl DeferOutcome {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Reused => "reused",
            Self::Parked => "parked",
            Self::Refused => "refused",
        }
    }
}

/// Running totals behind the deferral report, and when each outstanding commit was parked.
///
/// The three states a reader needs to tell apart are never-deferred (all counters zero), deferred and
/// ready (`joined` climbing), and deferred and still waiting (`outstanding` non-zero and `oldest_ms`
/// growing). A single count could not distinguish them and an absence of output could not either.
#[derive(Default)]
pub(super) struct Deferrals {
    joined: u64,
    reused: u64,
    parked: u64,
    refused: u64,
    /// When each key was parked. Pruned against the live commit table at report time rather than
    /// maintained at every removal site, so it cannot drift or leak however commits are retired.
    parked_at: HashMap<Key, Instant>,
    /// First occurrence of each (fate, reason) pair, so every distinct way a deferral can end says so
    /// ONCE at error level with the surface and token that hit it.
    ///
    /// The totals alone were not enough and that gap cost a diagnosis: a wedged surface reported
    /// `parked=3 refused=3` while the only line naming a deferral — a `hl_debug!` compiled out of the
    /// build that ships — fired zero times, so the counts were known to come from *some* path and the
    /// reader could not tell which. A counter that cannot name its own call site can start a sentence
    /// and not finish it.
    reasons: crate::diagnostic::Tally<(&'static str, &'static str)>,
}

struct Pending {
    surface: SurfaceId,
    deferred: Option<Deferred>,
    buffer: Option<WlBuffer>,
    external: Option<Dmabuf>,
    terminal: bool,
}

enum PendingFrame {
    Ready(Box<Ready>),
    Discarded,
    Unmatched(NativeFrame),
}

impl Pending {
    fn commit(deferred: Deferred) -> Self {
        Self {
            surface: deferred.surface,
            deferred: Some(deferred),
            buffer: None,
            external: None,
            terminal: false,
        }
    }

    fn surface(&self) -> SurfaceId {
        self.surface
    }

    fn into_deferred(mut self) -> Option<Deferred> {
        self.release_lease();
        self.deferred
    }

    fn published(&self) -> Option<super::buffer::ExternalBuffer> {
        let dmabuf = self
            .deferred
            .as_ref()
            .and_then(|deferred| deferred.external.as_ref())
            .or(self.external.as_ref())?;
        super::buffer::ExternalBuffer::published(dmabuf)
    }

    fn matches_external(&self, external: &WeakDmabuf) -> bool {
        self.external
            .as_ref()
            .is_some_and(|dmabuf| dmabuf.weak() == *external)
    }

    fn release_lease(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            buffer.release();
        }
        self.external = None;
    }
}

fn settle_callbacks<T>(
    callbacks: impl IntoIterator<Item = T>,
    now_nanos: u64,
    mut done: impl FnMut(T, u32),
) {
    let time_ms = (now_nanos / 1_000_000) as u32;
    for callback in callbacks {
        done(callback, time_ms);
    }
}

pub(super) struct NativeState {
    ingress: NativeFrames,
    frames: HashMap<Key, NativeFrame>,
    frame_order: VecDeque<Key>,
    commits: HashMap<Key, Deferred>,
    commit_order: VecDeque<Key>,
    pending: HashMap<NonZeroU64, VecDeque<Pending>>,
    last_frame: HashMap<NonZeroU64, NonZeroU64>,
    last_joined: HashMap<NonZeroU64, NonZeroU64>,
    active_tokens: HashSet<NonZeroU64>,
    registrations: HashMap<NonZeroU64, usize>,
    closing: HashSet<NonZeroU64>,
    poisoned: HashSet<NonZeroU64>,
    discarded: Vec<Deferred>,
    canceled: HashSet<Key>,
    canceled_order: VecDeque<Key>,
    serial_capacity: usize,
    token_capacity: usize,
    deferrals: Deferrals,
    /// The deferral report's cadence. Error level, `PRESENT` tag: it has to survive a release build and
    /// land on the tag an operator investigating presentation already enables.
    report: crate::diagnostic::Heartbeat<()>,
}

mod host;
mod retire;
mod state;
#[cfg(test)]
include!("native/tests.rs");
