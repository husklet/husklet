//! [`Session`] — the per-connection state the runtime owns, and [`Limits`] — its negotiated ceilings.
//!
//! A `Session` is the singular authority for one connection (§2 of the v2 overview): the negotiated
//! [`Capabilities`], the validation/accounting [`Limits`], the id→native [`SessionResources`], the
//! residency [`Ledger`] (+ its slice of the shared [`GlobalLedger`]), the [`FenceTimeline`], and the
//! pacing [`Clock`]. It holds state only — the workflows that mutate it (negotiate / validate / account /
//! dispatch) are `service/`. Ported from `hl-gpu/src/limits.rs`'s `ReplayLimits` (→ [`Limits`]) plus the
//! per-connection accounting state `ExecutorBudget` carried (→ [`Ledger`] + [`GlobalLedger`]).

use crate::protocol::model::capability::Capabilities;
use crate::runtime::model::resources::{Account, GlobalLedger, Ledger, SessionResources};
use crate::runtime::model::sharing::{ExportId, Exports, SessionId};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub(crate) enum ResourceSharing {
    Owner(ExportId),
    Importer(ExportId),
}
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
    /// The buffer-copy alignment every backend here requires, in bytes.
    ///
    /// This is a real host constraint, not a policy knob: `wgpu::CommandEncoder::copy_buffer_to_buffer`
    /// requires both offsets and the size to be multiples of `COPY_BUFFER_ALIGNMENT`, which is 4. A guest
    /// lowering an API whose copies are byte-granular — `cuMemcpyDtoD` is — must therefore split the
    /// unaligned edges out itself rather than expect the transfer to be accepted.
    ///
    /// It is NOT advertised through `Capabilities`, so a guest cannot negotiate it and has to assume this
    /// value. Anything stricter fails closed: the middle copy is refused and the error is reported.
    pub const DEFAULT_COPY_ALIGNMENT: u64 = 4;

    /// Hard ceiling on a draw's `instance_count`.
    ///
    /// The instance loop re-runs the whole primitive set once per instance, and `instance_count` is
    /// bounds-checked only when some vertex layout is per-instance; otherwise a maximal count means ~4
    /// billion full-framebuffer rasterizations. That is a pure CPU-time denial of service — it grows no
    /// allocation, so no residency ceiling notices it. Set far above any legitimate draw: the largest
    /// instance count anywhere in this workspace is 40, and browser-class instanced rendering runs to
    /// thousands.
    pub const MAX_DRAW_INSTANCES: u32 = 1 << 20;

    /// Hard ceiling on a single dispatch's total launch-block count (`grid_x * grid_y * grid_z`).
    ///
    /// A per-thread step cap bounds work WITHIN a block, but the block count was uncapped, so a validated
    /// dispatch over a real kernel could iterate up to `u32::MAX^3` blocks. The largest real grid any
    /// program here runs is ~262k blocks.
    pub const MAX_DISPATCH_BLOCKS: u64 = 1 << 26;

    /// Hard ceiling on a kernel's threads per block (`block_x * block_y * block_z`).
    ///
    /// Not an arbitrary safety number: it is CUDA's architectural `maxThreadsPerBlock` and WebGPU's
    /// `maxComputeInvocationsPerWorkgroup`, so a kernel above it could not launch on real hardware either.
    /// A guest front end derives the block shape from guest-supplied kernel source, so this is untrusted
    /// input reaching an allocation.
    pub const MAX_BLOCK_THREADS: u64 = 1024;

    /// Hard ceiling on a kernel's per-block shared-memory allocation.
    ///
    /// Allocated fresh per block from a guest-declared size, so an uncapped value asks for up to 4 GiB per
    /// block. Sits above CUDA's 48 KiB standard per-block limit and well above WebGPU's 16 KiB
    /// workgroup-storage limit, so no launchable kernel is affected.
    pub const MAX_SHARED_BYTES: u32 = 64 << 10;

    /// Default ceilings derived from a backend's advertised capabilities.
    ///
    /// The per-connection residency ceiling is a hostile-guest DoS guard — a single connection must never be
    /// able to pin so much host GPU memory / so many objects that it OOMs the host — NOT a correctness bound
    /// on a well-behaved client. It is sized for a BROWSER-CLASS client (the demanding real case): Chrome
    /// keeps a large live working set resident — hundreds of compositor tiles + glyph/mask atlases + per-GPU-
    /// context render targets, shaders and pipelines across its many worker contexts — that legitimately runs
    /// to well over the old 512 MiB / 65 536-object figures. Those old caps were mis-sized for a browser and
    /// made a healthy Chrome frame NACK `ResourceLimit("connection residency")`.
    ///
    /// The true unbounded-accumulation bug (Chrome loses a GL context and recreates its whole working set with
    /// fresh ids every cycle, and the guest never retired the abandoned set) is fixed at its ROOT by
    /// context-teardown retirement in the GL shim (`GlContext::retire_all`), so the resident set is now BOUNDED
    /// to what is actually live. This ceiling is therefore headroom for the live working set, not a band-aid
    /// over a leak — raising it alone would only have delayed the wall.
    ///
    /// Derived from the advertised `max_buffer_bytes` (a backend that accepts bigger single allocations grants
    /// proportionally bigger working-set headroom) and floored at 2 GiB so even a conservative backend gives a
    /// browser room. It stays FINITE and per-connection; the process-wide [`GlobalLedger`] remains the real
    /// host-OOM guard across all connections.
    pub fn from_capabilities(caps: Capabilities) -> Self {
        // Browser-class working-set headroom: 2× the largest single allocation the backend accepts, never
        // below 2 GiB. Finite by construction (a hostile guest still cannot pin unbounded host memory).
        let max_connection_bytes = caps.max_buffer_bytes.saturating_mul(2).max(2 << 30);
        Self {
            caps,
            max_connection_bytes,
            // A rich page's LIVE working set (per-tile textures + per-program shaders/pipelines across many GPU
            // contexts) can hold well over 64k distinct objects at once; 256k keeps the object guard finite
            // without tripping a healthy browser frame.
            max_connection_objects: 262_144,
            copy_alignment: Self::DEFAULT_COPY_ALIGNMENT,
            // Chrome links a large program set; 128 MiB of compiled PSO/AIR cache is browser-class headroom
            // while still bounding the compiled-pipeline residency separately from raw data.
            max_compiled_cache_bytes: 128 << 20,
        }
    }
}

/// The authoritative per-connection state. One per client connection; the injected executor is *not* part
/// of it (a `Session` drives whichever `&mut dyn GpuExecutor` the host wired up).
pub struct Session {
    /// This connection's identity, minted by [`SessionId::next`] and unique for the life of the process.
    /// Every cross-connection sharing rule keys on it.
    pub id: SessionId,
    /// The process-global export registry, when this connection is permitted to share.
    ///
    /// `None` — the default — means sharing is not wired for this connection, and every export/import is
    /// refused as [`GpuError::Unsupported`]. It is deliberately NOT a private registry created per
    /// session: that would compile, satisfy every registry test, and share nothing, which is the failure
    /// this shape exists to make impossible. The composition root clones ONE [`Exports`] into every
    /// session ([`with_exports`](Self::with_exports)); a `None` here fails closed and says so.
    ///
    /// [`GpuError::Unsupported`]: crate::protocol::model::error::GpuError::Unsupported
    pub exports: Option<Exports>,
    pub(crate) buffer_sharing: HashMap<u32, ResourceSharing>,
    pub(crate) texture_sharing: HashMap<u32, ResourceSharing>,
    /// Validation + accounting ceilings (its `caps` refreshed by `negotiate`).
    pub limits: Limits,
    /// The executor's advertised capabilities, once negotiated.
    pub caps: Option<Capabilities>,
    /// The singular id → native-handle owner the executor mutates.
    pub resources: SessionResources,
    /// Cloneable accounting authority shared with export-registry lifetime transitions.
    pub account: Account,
    /// This connection's slice of the shared process-global residency ceiling.
    pub global: GlobalLedger,
    /// Timeline-fence high-water marks.
    pub timeline: FenceTimeline,
    /// Pacing / timeline-stamp time source.
    pub clock: Box<dyn Clock>,
}

impl Session {
    fn release_sharing(&mut self) {
        let Some(exports) = self.exports.clone() else {
            return;
        };
        let bindings: Vec<ResourceSharing> = self.buffer_sharing.values().chain(self.texture_sharing.values()).copied().collect();
        for binding in bindings {
            loop {
                let result = match binding {
                    ResourceSharing::Owner(export) => {
                        exports.prepare_owner_release(self.id, export).map(|plan| {
                            plan.commit();
                            Some(())
                        })
                    }
                    ResourceSharing::Importer(export) => exports
                        .prepare_import_release(self.id, export)
                        .map(|plan| Some(plan.commit())),
                };
                match result {
                    Ok(_) => break,
                    Err(crate::GpuError::MappedElsewhere { .. }) => {
                        let export = match binding {
                            ResourceSharing::Owner(id) | ResourceSharing::Importer(id) => id,
                        };
                        exports.settle_transition(export, std::time::Duration::from_millis(10));
                    }
                    // The binding map and registry are updated together. If teardown finds them divergent,
                    // `forget_session` below is the only safe infallible recovery and still drops the native
                    // references; do not spin forever on a state that cannot become valid.
                    Err(_) => break,
                }
            }
        }
        // Clear a claim taken directly through the registry even when no local binding was installed.
        // Accounted bindings were already transitioned above, so this is only orphan-state recovery.
        exports.forget_session(self.id);
        self.buffer_sharing.clear();
        self.texture_sharing.clear();
    }

    /// A fresh connection session with the given ceilings, global account slice, and clock.
    pub fn new(limits: Limits, global: GlobalLedger, clock: Box<dyn Clock>) -> Self {
        let id = SessionId::next();
        let account = Account::new();
        account
            .bind_session(id)
            .expect("fresh account binds to fresh session");
        Self {
            id,
            exports: None,
            buffer_sharing: HashMap::new(),
            texture_sharing: HashMap::new(),
            limits,
            caps: None,
            resources: SessionResources::new(),
            account,
            global,
            timeline: FenceTimeline::new(),
            clock,
        }
    }

    /// Join this connection to the process-global export registry. The composition root clones ONE
    /// [`Exports`] into every session it creates; a session that never gets one refuses to share rather
    /// than sharing with nobody.
    pub fn with_exports(mut self, exports: Exports) -> Self {
        self.exports = Some(exports);
        self
    }

    /// Cumulative bytes resident on this connection.
    pub fn residency_bytes(&self) -> u64 {
        self.account.ledger().residency_bytes()
    }
    /// Cumulative live object count charged to this connection.
    pub fn object_count(&self) -> u64 {
        self.account.ledger().object_count()
    }
    /// Bytes of this connection's residency attributable to the compiled-pipeline cache.
    pub fn compiled_cache_bytes(&self) -> u64 {
        self.account.ledger().compiled_cache_bytes()
    }

    /// Explicit teardown: drop every live native handle, clear the fence timeline, and refund this
    /// connection's whole residency to the shared global account — then reset the accounting state so the
    /// later [`Drop`] refunds nothing (no double-refund of the global budget). Idempotent: a second call
    /// refunds `Totals::default()` (a no-op) and re-clears already-empty tables, so a `Drop` after an
    /// explicit `release_all` is safe. Leaves the `Session` a valid, empty connection.
    pub fn release_all(&mut self) {
        self.release_sharing();
        self.global.refund(self.account.ledger().totals);
        self.account.replace_ledger(Ledger::default());
        // Dropping the tables below frees this connection's natives, so anything it exported has been
        // released by its owner as of here — the registry retains the storage only for live importers.
        // Dropping the old table frees every native handle behind it; a fresh table restores the
        // per-kind generation counters to their initial state for any reuse of this session object.
        self.resources = SessionResources::new();
        self.buffer_sharing.clear();
        self.texture_sharing.clear();
        self.timeline = FenceTimeline::new();
    }
}

impl Drop for Session {
    /// On disconnect, release this connection's whole residency contribution back to the global account
    /// so a dropped connection cannot leak the shared budget.
    fn drop(&mut self) {
        self.release_sharing();
        self.global.refund(self.account.ledger().totals);
        // Drop every reference this connection held in BOTH directions: its claims are released, its
        // imports drop their refcount, and an export it owned is retained only while someone still
        // imports it. Without this a departed connection pins storage forever and leaves a permanent
        // `MappedBy` claim against a session that no longer exists.
    }
}
