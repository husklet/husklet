//! Runtime-layer tests: drive a `Session` through negotiate → validate → account → dispatch against a
//! `FakeExecutor` and assert the pipeline's contracts — failure atomicity (a rejected batch never reaches
//! the executor and never mutates residency) and transactional residency accounting (charge on create,
//! refund on destroy, reject over-limit before any mutation).

use std::any::Any;

use hl_gpu::protocol::model::capability::{command_bits, shader_payload, ALL_COMMANDS};
use hl_gpu::protocol::model::command::CommandBuffer;
use hl_gpu::protocol::model::descriptor::{BufferDesc, SurfaceDesc};
use hl_gpu::protocol::model::enums::{buffer_usage, TextureFormat};
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
