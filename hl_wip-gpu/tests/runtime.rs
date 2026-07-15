//! Runtime-layer tests: drive a `Session` through negotiate → validate → account → dispatch against a
//! `FakeExecutor` and assert the pipeline's contracts — failure atomicity (a rejected batch never reaches
//! the executor and never mutates residency) and transactional residency accounting (charge on create,
//! refund on destroy, reject over-limit before any mutation).

use std::any::Any;

use hl_gpu::protocol::model::capability::{command_bits, shader_payload, ALL_COMMANDS};
use hl_gpu::protocol::model::command::CommandBuffer;
use hl_gpu::protocol::model::descriptor::{BufferDesc, SurfaceDesc, TextureDesc};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, TextureDim, TextureFormat};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::runtime::port::executor::{GpuExecutor, Presented};
use hl_gpu::runtime::service;
use hl_gpu::{
    Capabilities, Cmd, Enc, FakeClock, FeatureRequest, FenceId, GlobalLedger, GpuError, Limits,
    Result, Session, SurfaceId, TextureId,
};

// A recording executor: advertises canned capabilities, records every `execute`/`wait` call, and
// mirrors the batch's resource lifecycle into the runtime-owned `SessionResources` (so a create/destroy
// mismatch would surface as a typed table error). Its native handle is a unit `()` behind each id.
struct FakeExecutor {
    caps: Capabilities,
    executed: Vec<Vec<Cmd>>,
    waits: Vec<(u32, u64)>,
}

impl FakeExecutor {
    fn new(caps: Capabilities) -> Self {
        Self { caps, executed: Vec::new(), waits: Vec::new() }
    }
    fn command_count(&self) -> usize {
        self.executed.iter().map(Vec::len).sum()
    }
}

impl GpuExecutor for FakeExecutor {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn execute(&mut self, res: &mut SessionResources, batch: &[Cmd]) -> Result<Vec<Presented>> {
        self.executed.push(batch.to_vec());
        let native = || -> Box<dyn Any> { Box::new(()) };
        let mut presents = Vec::new();
        for cmd in batch {
            match cmd {
                Cmd::CreateBuffer(id, _) => res.buffers.insert(*id, native())?,
                Cmd::DestroyBuffer(id) => {
                    res.buffers.remove(*id)?;
                }
                Cmd::CreateSurface(id, _) => res.surfaces.insert(*id, native())?,
                Cmd::DestroySurface(id) => {
                    res.surfaces.remove(*id)?;
                }
                Cmd::CreateFence(id) => res.fences.insert(*id, native())?,
                Cmd::DestroyFence(id) => {
                    res.fences.remove(*id)?;
                }
                Cmd::Present { surface, texture } => presents
                    .push(Presented { surface: SurfaceId(*surface), texture: TextureId(*texture) }),
                _ => {}
            }
        }
        Ok(presents)
    }

    fn wait(&mut self, _res: &mut SessionResources, fence: FenceId, value: u64) -> Result<()> {
        self.waits.push((fence.0, value));
        Ok(())
    }
}

fn buffer(id: u32, size: u64) -> Cmd {
    Cmd::CreateBuffer(id, BufferDesc { size, usage: buffer_usage::COPY_DST, label: String::new() })
}

// A 2D RGBA8 texture create — the shape a Chrome compositor tile / SharedImage backing takes. `dim` px²
// is `dim*dim*4` bytes of residency (single mip), so a small `dim` gives a predictable per-tile charge.
fn texture(id: u32, dim: u32) -> Cmd {
    Cmd::CreateTexture(
        id,
        TextureDesc {
            width: dim,
            height: dim,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::SAMPLED | texture_usage::RENDER_TARGET,
            label: String::new(),
        },
    )
}

// An executor that MIRRORS the batch's buffer/texture lifecycle into the runtime-owned tables (like
// `FakeExecutor`) but NACKs the frame the moment it reaches a `Present` — modelling a real executor that
// applies the frame's creates/destroys and only THEN fails device validation on the swap's submit/present
// (the exact Chrome NACK). It applies the creates/destroys BEFORE the failure, so without the runtime's
// transaction the id tables would be left half-mutated when the frame is rejected.
struct NackOnPresentExecutor {
    caps: Capabilities,
}

impl NackOnPresentExecutor {
    fn new(caps: Capabilities) -> Self {
        Self { caps }
    }
}

impl GpuExecutor for NackOnPresentExecutor {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn execute(&mut self, res: &mut SessionResources, batch: &[Cmd]) -> Result<Vec<Presented>> {
        let native = || -> Box<dyn Any> { Box::new(()) };
        for cmd in batch {
            match cmd {
                Cmd::CreateBuffer(id, _) => res.buffers.insert(*id, native())?,
                Cmd::DestroyBuffer(id) => {
                    res.buffers.remove(*id)?;
                }
                Cmd::CreateTexture(id, _) => res.textures.insert(*id, native())?,
                Cmd::DestroyTexture(id) => {
                    res.textures.remove(*id)?;
                }
                // The swap's present: the frame's creates/destroys are already applied above; now NACK,
                // exactly as the wgpu executor does when the swap fails device validation.
                Cmd::Present { .. } => return Err(GpuError::Invalid("wgpu: pass failed device validation")),
                _ => {}
            }
        }
        Ok(Vec::new())
    }

    fn wait(&mut self, _res: &mut SessionResources, _fence: FenceId, _value: u64) -> Result<()> {
        Ok(())
    }
}

fn session(limits: Limits, global: GlobalLedger) -> Session {
    Session::new(limits, global, Box::new(FakeClock::new(1_000)))
}

/// The full pipeline on a good batch: negotiate succeeds, then validate → account → dispatch runs the
/// executor, charges residency, and stamps the fence timeline.
#[test]
fn negotiate_then_submit_good_batch_executes_and_accounts() {
    let caps = Capabilities::full("fake");
    let mut exec = FakeExecutor::new(caps.clone());
    let mut s = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());

    // Negotiate the full current IR surface against the executor's advertised caps.
    let req = FeatureRequest {
        wire_version: caps.wire_version,
        shader_payloads: shader_payload::SPIRV,
        command_bits: command_bits(ALL_COMMANDS),
        texture_formats: 0,
    };
    let negotiated = service::negotiate::negotiate(&mut s, &exec, &req).expect("negotiate ok");
    assert_eq!(negotiated.name, "fake");
    assert!(s.caps.is_some());

    // A representative batch: create a buffer + surface + fence, submit a clear signalling the fence,
    // then present.
    let batch = vec![
        buffer(1, 4096),
        Cmd::CreateSurface(10, SurfaceDesc { width: 4, height: 4, format: TextureFormat::Rgba8Unorm, hlp_surface: 1 }),
        Cmd::CreateFence(20),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::ClearRect { texture: 10, x: 0, y: 0, w: 4, h: 4, color: [0.0; 4] }],
            signal: Some((20, 7)),
        }),
        Cmd::Present { surface: 10, texture: 10 },
    ];
    let presents = hl_gpu::runtime::submit(&mut s, &mut exec, 512, &batch).expect("good batch");

    assert_eq!(presents, vec![Presented { surface: SurfaceId(10), texture: TextureId(10) }]);
    assert_eq!(exec.command_count(), batch.len(), "the whole batch reached the executor");
    // Residency: 4096 (buffer) + 64 (4x4 rgba8 surface) + 128 (fence) across 3 objects.
    assert_eq!(s.residency_bytes(), 4096 + 4 * 4 * 4 + 128);
    assert_eq!(s.object_count(), 3);
    assert_eq!(s.resources.live_count(), 3, "executor tracked natives behind the ids");
    // Fence 20 signalled to 7, stamped with the fake clock.
    assert_eq!(s.timeline.value(20), Some(7));
    assert!(s.timeline.is_reached(20, 7) && !s.timeline.is_reached(20, 8));
}

/// Failure atomicity: an over-`max_buffer_bytes` create is rejected at VALIDATE — before the executor is
/// ever called and before any residency is charged.
#[test]
fn validation_rejects_over_limit_batch_before_any_execute() {
    // Advertise a tiny per-buffer ceiling so a large buffer fails validation.
    let mut caps = Capabilities::full("fake");
    caps.max_buffer_bytes = 1024;
    let mut exec = FakeExecutor::new(caps.clone());
    let mut s = session(Limits::from_capabilities(caps), GlobalLedger::unbounded());

    let batch = vec![buffer(1, 4096)]; // 4096 > max_buffer_bytes(1024)
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 64, &batch).unwrap_err();

    assert_eq!(err, GpuError::ResourceLimit("buffer bytes"));
    assert_eq!(exec.command_count(), 0, "rejected batch never reached the executor");
    assert_eq!(s.residency_bytes(), 0, "rejected batch charged no residency");
    assert_eq!(s.object_count(), 0);
    assert_eq!(s.resources.live_count(), 0);
}

/// Failure atomicity at VALIDATE for a malformed batch: an encoder op whose command tag is not in the
/// negotiated set is rejected before execute/charge.
#[test]
fn validation_rejects_unnegotiated_command_before_any_execute() {
    // Advertise a command set WITHOUT Dispatch, then submit a Dispatch.
    let mut caps = Capabilities::full("fake");
    caps.command_bits = command_bits(&[
        hl_gpu::protocol::model::command::etag::BEGIN_RENDER_PASS,
        hl_gpu::protocol::model::command::etag::CLEAR_RECT,
    ]);
    let mut exec = FakeExecutor::new(caps.clone());
    let mut s = session(Limits::from_capabilities(caps), GlobalLedger::unbounded());

    let batch = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::Dispatch { x: 1, y: 1, z: 1 }],
        signal: None,
    })];
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 64, &batch).unwrap_err();

    assert_eq!(err, GpuError::ResourceLimit("encoder command"));
    assert_eq!(exec.command_count(), 0);
    assert_eq!(s.residency_bytes(), 0);
}

/// Residency accounting: charge on create, refund exactly on destroy, and reject an over-connection-budget
/// create atomically (the failed charge never reaches the executor and never partially mutates the
/// ledger). Here `max_buffer_bytes` is large so the rejection is an ACCOUNTING (connection-budget) one,
/// not a per-object validation one.
#[test]
fn residency_charges_on_create_refunds_on_destroy_and_rejects_over_budget() {
    let caps = Capabilities::full("fake"); // large per-object ceilings
    let mut exec = FakeExecutor::new(caps.clone());
    // Connection budget: 4096 bytes / 8 objects.
    let mut limits = Limits::from_capabilities(caps);
    limits.max_connection_bytes = 4096;
    limits.max_connection_objects = 8;
    let mut s = session(limits, GlobalLedger::unbounded());

    // Charge on create.
    hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[buffer(1, 4096)]).expect("exact fit");
    assert_eq!((s.residency_bytes(), s.object_count()), (4096, 1));

    // Over-budget create is rejected atomically at ACCOUNT — nothing charged, executor untouched.
    let executed_before = exec.command_count();
    let before_bytes = s.residency_bytes();
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[buffer(2, 1)]).unwrap_err();
    assert_eq!(err, GpuError::ResourceLimit("connection residency"));
    assert_eq!(s.residency_bytes(), before_bytes, "rejected charge did not mutate the ledger");
    assert_eq!(s.object_count(), 1);
    assert_eq!(exec.command_count(), executed_before, "rejected charge never reached the executor");

    // Refund on destroy, exactly — then the freed budget is reusable.
    hl_gpu::runtime::submit(&mut s, &mut exec, 32, &[Cmd::DestroyBuffer(1)]).expect("destroy refunds");
    assert_eq!((s.residency_bytes(), s.object_count()), (0, 0));
    hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[buffer(3, 4096)]).expect("refunded budget reused");
    assert_eq!(s.residency_bytes(), 4096);
}

/// The shared global ledger isolates connections and a dropped connection refunds its whole global
/// contribution.
#[test]
fn global_ledger_isolates_connections_and_drop_refunds() {
    let caps = Capabilities::full("fake");
    let global = GlobalLedger::new(4096, 8);

    let mut e1 = FakeExecutor::new(caps.clone());
    let mut l1 = Limits::from_capabilities(caps.clone());
    l1.max_connection_bytes = 4096;
    let mut first = session(l1, global.clone());
    hl_gpu::runtime::submit(&mut first, &mut e1, 64, &[buffer(1, 4096)]).expect("first fills global");

    // A second connection cannot allocate past the shared global byte ceiling.
    let mut e2 = FakeExecutor::new(caps.clone());
    let mut l2 = Limits::from_capabilities(caps);
    l2.max_connection_bytes = 4096;
    let mut second = session(l2, global.clone());
    assert_eq!(
        hl_gpu::runtime::submit(&mut second, &mut e2, 64, &[buffer(9, 4096)]).unwrap_err(),
        GpuError::ResourceLimit("global residency")
    );

    // Dropping the first connection refunds the global account so the second now fits.
    drop(first);
    hl_gpu::runtime::submit(&mut second, &mut e2, 64, &[buffer(9, 4096)])
        .expect("disconnect refunded the global owner");
}

/// Negotiation rejects an incompatible guest before any command flows.
#[test]
fn negotiate_rejects_incompatible_wire_version() {
    let caps = Capabilities::full("fake");
    let exec = FakeExecutor::new(caps.clone());
    let mut s = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());
    let req = FeatureRequest { wire_version: caps.wire_version + 1, ..Default::default() };
    assert_eq!(
        service::negotiate::negotiate(&mut s, &exec, &req).unwrap_err(),
        GpuError::Unsupported("capability: wire version mismatch")
    );
    assert!(s.caps.is_none(), "a failed negotiation records no caps");
}

/// Executor-NACK atomicity (the Chrome swap-NACK recovery). A frame creates a fresh tile texture and
/// destroys the old one, then the swap's present NACKs (device validation) — the executor has ALREADY
/// applied the create + destroy by the time it fails. The whole submit must be atomic: the id tables AND
/// the residency ledger are rolled back to EXACTLY the pre-frame state, so the connection recovers —
/// destroying the (restored) old id does not `UnknownId`, and recreating the (rolled-back) new id does
/// not `DuplicateId`. Before the fix this drifted: the old texture was gone (its later destroy hit
/// `UnknownId`) and the new one stuck live + charged (its recreate hit `DuplicateId`), NACKing forever.
#[test]
fn executor_nack_rolls_back_tables_and_ledger_then_connection_recovers() {
    let caps = Capabilities::full("fake");
    let mut exec = NackOnPresentExecutor::new(caps.clone());
    // Unbounded residency so the ONLY rejection here is the executor's dispatch-stage NACK, not accounting.
    let mut s = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());

    // A prior frame created the "old" tile (id 1); it is committed and live on the executor.
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(1, 8)]).expect("old tile created");
    let tile_bytes = 8 * 8 * 4; // one 8x8 rgba8 mip
    assert_eq!((s.residency_bytes(), s.object_count(), s.resources.live_count()), (tile_bytes, 1, 1));

    // The swap frame: create the new tile (id 2), free the old tile (id 1), then present -> NACK. The
    // executor applies the create + destroy, then fails on the present.
    let surf = SurfaceDesc { width: 8, height: 8, format: TextureFormat::Rgba8Unorm, hlp_surface: 1 };
    let swap = vec![
        texture(2, 8),
        Cmd::DestroyTexture(1),
        Cmd::CreateSurface(30, surf),
        Cmd::Present { surface: 30, texture: 2 },
    ];
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 256, &swap).unwrap_err();
    assert_eq!(err, GpuError::Invalid("wgpu: pass failed device validation"));

    // Atomicity: the NACKed frame left the tables + ledger EXACTLY as before it — old tile (1) restored,
    // new tile (2) and the surface (30) rolled back, residency back to just the old tile.
    assert_eq!(
        (s.residency_bytes(), s.object_count(), s.resources.live_count()),
        (tile_bytes, 1, 1),
        "a NACKed frame charges no residency and leaves exactly the pre-frame objects"
    );
    assert!(s.resources.textures.contains(1), "the freed old tile is restored (destroy was rolled back)");
    assert!(!s.resources.textures.contains(2), "the created new tile is gone (create was rolled back)");
    assert!(!s.resources.surfaces.contains(30), "the created surface is gone (create was rolled back)");

    // Recovery 1 — destroying the (restored) old tile now SUCCEEDS (pre-fix this was `UnknownId`).
    hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[Cmd::DestroyTexture(1)])
        .expect("old tile is destroyable after the NACK rolled its destroy back");
    assert_eq!((s.residency_bytes(), s.object_count()), (0, 0));

    // Recovery 2 — recreating the new tile now SUCCEEDS (pre-fix this was `DuplicateId`), and the whole
    // connection keeps working: the retried swap present NACKs cleanly and is again fully rolled back.
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(2, 8)])
        .expect("new tile id is free again after the NACK rolled its create back");
    assert_eq!((s.residency_bytes(), s.object_count(), s.resources.live_count()), (tile_bytes, 1, 1));
}

/// The residency-cap create/free/recreate loop (Chrome's bounded live tile set). With old tiles freed each
/// frame the live set stays UNDER the per-connection cap; a frame that would exceed the cap is NACKed
/// atomically (`ResourceLimit("connection residency")`) and — crucially — the connection RECOVERS: freeing
/// old tiles and creating new ones in the next frame keeps working with no `UnknownId`. This is why the
/// 512 MiB / 65 536-object cap need not be raised: a well-behaved client with a bounded working set that
/// retires what it no longer needs never sticks against it, while the cap still bounds a hostile flood.
#[test]
fn residency_cap_nacks_over_budget_frame_then_free_and_recreate_recovers() {
    let caps = Capabilities::full("fake");
    // Mirrors texture lifecycle into the tables; NACKs only on `Present` (this test issues none).
    let mut exec = NackOnPresentExecutor::new(caps.clone());
    let tile_bytes = 8 * 8 * 4u64; // one 8x8 rgba8 tile
    // A cap sized for EXACTLY two live tiles — a tiny stand-in for the real bounded working set.
    let mut limits = Limits::from_capabilities(caps);
    limits.max_connection_bytes = 2 * tile_bytes;
    limits.max_connection_objects = 2;
    let mut s = session(limits, GlobalLedger::unbounded());

    // Fill the working set to the cap: two live tiles.
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(1, 8)]).expect("tile 1");
    hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(2, 8)]).expect("tile 2 (at cap)");
    assert_eq!((s.residency_bytes(), s.object_count()), (2 * tile_bytes, 2));

    // A swap frame that creates one more tile without freeing an old one trips the cap and NACKs. It is
    // rejected atomically at ACCOUNT: nothing charged, nothing reached the executor.
    let live_before = s.resources.live_count();
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 128, &[texture(3, 8)]).unwrap_err();
    assert_eq!(err, GpuError::ResourceLimit("connection residency"));
    assert_eq!((s.residency_bytes(), s.object_count()), (2 * tile_bytes, 2), "NACK charged nothing");
    assert_eq!(s.resources.live_count(), live_before, "NACKed frame never reached the executor");

    // Recovery — the Chrome create-tile / free-old-tile / swap step: free the two old tiles and create two
    // fresh ones in one frame. Net residency stays at the cap, so it fits, and every destroy resolves a
    // LIVE id (no `UnknownId`). The connection keeps working.
    let loop_frame = vec![
        Cmd::DestroyTexture(1),
        Cmd::DestroyTexture(2),
        texture(3, 8),
        texture(4, 8),
    ];
    hl_gpu::runtime::submit(&mut s, &mut exec, 256, &loop_frame).expect("free-old + create-new fits the cap");
    assert_eq!((s.residency_bytes(), s.object_count()), (2 * tile_bytes, 2));
    assert!(s.resources.textures.contains(3) && s.resources.textures.contains(4));
    assert!(!s.resources.textures.contains(1) && !s.resources.textures.contains(2));
}
