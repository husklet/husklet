//! [`Session`] — the per-connection state the runtime owns, and [`Limits`] — its negotiated ceilings.
//!
//! A `Session` is the singular authority for one connection (§2 of the v2 overview): the negotiated
//! [`Capabilities`], the validation/accounting [`Limits`], the id→native [`SessionResources`], the
//! residency [`Ledger`] (+ its slice of the shared [`GlobalLedger`]), the [`FenceTimeline`], and the
//! pacing [`Clock`]. It holds state only — the workflows that mutate it (negotiate / validate / account /
//! dispatch) are `service/`. Ported from `hl-gpu/src/limits.rs`'s `ReplayLimits` (→ [`Limits`]) plus the
//! per-connection accounting state `ExecutorBudget` carried (→ [`Ledger`] + [`GlobalLedger`]).

use crate::protocol::model::capability::Capabilities;
use crate::runtime::model::resources::{GlobalLedger, Ledger, SessionResources};
use crate::runtime::model::timeline::FenceTimeline;
use crate::runtime::port::clock::Clock;

/// The negotiated per-connection validation + accounting ceilings. `caps` carries the per-object limits
/// (max frame/buffer bytes, texture dim, bind groups, supported command/format/shader bitsets) checked at
/// validation; the remaining fields are connection-residency policy checked at accounting.
#[derive(Clone, Debug)]
pub struct Limits {
    pub caps: Capabilities,
    pub max_connection_bytes: u64,
    pub max_connection_objects: u64,
    /// Negotiated backend copy alignment (bytes): buffer-copy offsets/sizes and image-copy
    /// `bytes_per_row`/offsets must be a multiple of this or the transfer is rejected before the executor
    /// decodes it. `<= 1` disables the check (byte-addressable).
    pub copy_alignment: u64,
    /// Negotiated per-connection compiled-pipeline (PSO/AIR) cache ceiling in bytes.
    pub max_compiled_cache_bytes: u64,
}

impl Limits {
    /// Default ceilings derived from a backend's advertised capabilities.
    pub fn from_capabilities(caps: Capabilities) -> Self {
        Self {
            caps,
            max_connection_bytes: 512 << 20,
            max_connection_objects: 65_536,
            copy_alignment: 4,
            max_compiled_cache_bytes: 64 << 20,
        }
    }
}

/// The authoritative per-connection state. One per client connection; the injected executor is *not* part
/// of it (a `Session` drives whichever `&mut dyn GpuExecutor` the host wired up).
pub struct Session {
    /// Validation + accounting ceilings (its `caps` refreshed by `negotiate`).
    pub limits: Limits,
    /// The executor's advertised capabilities, once negotiated.
    pub caps: Option<Capabilities>,
    /// The singular id → native-handle owner the executor mutates.
    pub resources: SessionResources,
    /// This connection's residency accounting state.
    pub ledger: Ledger,
    /// This connection's slice of the shared process-global residency ceiling.
    pub global: GlobalLedger,
    /// Timeline-fence high-water marks.
    pub timeline: FenceTimeline,
    /// Pacing / timeline-stamp time source.
    pub clock: Box<dyn Clock>,
}

impl Session {
    /// A fresh connection session with the given ceilings, global account slice, and clock.
    pub fn new(limits: Limits, global: GlobalLedger, clock: Box<dyn Clock>) -> Self {
        Self {
            limits,
            caps: None,
            resources: SessionResources::new(),
            ledger: Ledger::default(),
            global,
            timeline: FenceTimeline::new(),
            clock,
        }
    }

    /// Cumulative bytes resident on this connection.
    pub fn residency_bytes(&self) -> u64 {
        self.ledger.residency_bytes()
    }
    /// Cumulative live object count charged to this connection.
    pub fn object_count(&self) -> u64 {
        self.ledger.object_count()
    }
    /// Bytes of this connection's residency attributable to the compiled-pipeline cache.
    pub fn compiled_cache_bytes(&self) -> u64 {
        self.ledger.compiled_cache_bytes()
    }

    /// Explicit teardown: drop every live native handle, clear the fence timeline, and refund this
    /// connection's whole residency to the shared global account — then reset the accounting state so the
    /// later [`Drop`] refunds nothing (no double-refund of the global budget). Idempotent: a second call
    /// refunds `Totals::default()` (a no-op) and re-clears already-empty tables, so a `Drop` after an
    /// explicit `release_all` is safe. Leaves the `Session` a valid, empty connection.
    pub fn release_all(&mut self) {
        self.global.refund(self.ledger.totals);
        self.ledger = Ledger::default();
        // Dropping the old table frees every native handle behind it; a fresh table restores the
        // per-kind generation counters to their initial state for any reuse of this session object.
        self.resources = SessionResources::new();
        self.timeline = FenceTimeline::new();
    }
}

impl Drop for Session {
    /// On disconnect, release this connection's whole residency contribution back to the global account
    /// so a dropped connection cannot leak the shared budget.
    fn drop(&mut self) {
        self.global.refund(self.ledger.totals);
    }
}
