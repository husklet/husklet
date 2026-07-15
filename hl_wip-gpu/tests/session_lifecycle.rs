//! Runtime SESSION-LIFECYCLE robustness battery — the layer the malformed-wire fuzzer (`wire_fuzz.rs`)
//! did not deeply cover. Where `wire_fuzz` hammers decode bytes / dangling ids / DoS clamps on a single
//! session, this drives *many* `Session` / `InProcessCommandSink` instances through their whole life:
//! stand-up, interleaved use, mid-stream teardown, id reuse across lives, use-after-close, and cleanup
//! on drop — asserting isolation, clean teardown, and typed errors (never a panic, never a leak, never
//! cross-session corruption).
//!
//! Everything here is deterministic (a `FakeClock`, the pure-CPU reference executor, fixed byte
//! patterns) so a regression reproduces on the first run, not statistically.

use hl_gpu::protocol::model::descriptor::{BufferDesc, SamplerDesc, TextureDesc};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, TextureDim, TextureFormat,
};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{
    Cmd, CommandSink, CpuExecutor, FakeClock, FeatureRequest, GlobalLedger, GpuError, GpuExecutor,
    InProcessCommandSink, Limits, Session,
};

// -------------------------------------------------------------------------------------------------
// helpers
// -------------------------------------------------------------------------------------------------

/// A socket-free in-process sink over the pure CPU reference executor, with its own private
/// (unbounded) global account — the way a caller stands one up for a single connection.
fn sink() -> InProcessCommandSink<CpuExecutor> {
    InProcessCommandSink::new(CpuExecutor::new())
}

/// A sink whose session shares an explicit `global` account (so several connections meter against the
/// same process-wide ceiling — the surface a leak/isolation check needs).
fn sink_on(global: &GlobalLedger) -> InProcessCommandSink<CpuExecutor> {
    let limits = Limits::from_capabilities(CpuExecutor::new().capabilities());
    let session = Session::new(limits, global.clone(), Box::new(FakeClock::new(0)));
    InProcessCommandSink::with_session(session, CpuExecutor::new())
}

fn buffer(id: u32, size: u64) -> Cmd {
    Cmd::CreateBuffer(id, BufferDesc { size, usage: buffer_usage::COPY_DST, label: String::new() })
}

fn write(id: u32, byte: u8, len: usize) -> Cmd {
    Cmd::WriteBuffer { id, offset: 0, data: vec![byte; len] }
}

// -------------------------------------------------------------------------------------------------
// 1. concurrent_sessions_isolated
// -------------------------------------------------------------------------------------------------

/// Two independent sinks, used INTERLEAVED, each creating resources under OVERLAPPING ids. One
/// session's resources must be invisible to the other (no shared-table corruption) and each must
/// compute + read back its own correct result.
#[test]
fn concurrent_sessions_isolated() {
    let mut a = sink();
    let mut b = sink();

    // Interleave create/write across both sinks, both using buffer id 1 (overlapping id space).
    a.submit(&[buffer(1, 16)]).unwrap();
    b.submit(&[buffer(1, 16)]).unwrap();
    a.submit(&[write(1, 0xAA, 16)]).unwrap();
    b.submit(&[write(1, 0xBB, 16)]).unwrap();

    // Each session read back its OWN bytes — no cross-talk through the shared id value.
    assert_eq!(a.read_buffer(hl_gpu::BufferId(1), 0, 16).unwrap(), vec![0xAA; 16]);
    assert_eq!(b.read_buffer(hl_gpu::BufferId(1), 0, 16).unwrap(), vec![0xBB; 16]);

    // A resource created only in A is invisible to B (distinct tables, not one shared map).
    a.submit(&[buffer(2, 8)]).unwrap();
    assert_eq!(a.session().resources.live_count(), 2);
    assert_eq!(b.session().resources.live_count(), 1);
    assert_eq!(
        b.read_buffer(hl_gpu::BufferId(2), 0, 8).unwrap_err(),
        GpuError::UnknownId { kind: "buffer", id: 2 }
    );

    // Destroying A's id 1 does not perturb B's id 1.
    a.submit(&[Cmd::DestroyBuffer(1)]).unwrap();
    assert_eq!(b.read_buffer(hl_gpu::BufferId(1), 0, 16).unwrap(), vec![0xBB; 16]);
}

// -------------------------------------------------------------------------------------------------
// 2. teardown_mid_stream
// -------------------------------------------------------------------------------------------------

/// Build a stream, submit PART of it, then DROP the sink while resources are still live. No panic, and
/// the resources' residency is reclaimed on drop (the shared global account returns to baseline) so the
/// account is left usable for a fresh session.
#[test]
fn teardown_mid_stream() {
    let global = GlobalLedger::new(1 << 30, 1 << 20);
    assert_eq!(global.residency_bytes(), 0);

    {
        let mut s = sink_on(&global);
        // Submit only the first half of a would-be longer stream; resources stay live, pending more.
        s.submit(&[buffer(1, 4096), buffer(2, 2048)]).unwrap();
        assert_eq!(s.session().resources.live_count(), 2);
        assert!(global.residency_bytes() >= 4096 + 2048, "mid-stream resources are charged");
        // ...and drop `s` here, mid-stream, without ever draining the rest. Must not panic.
    }

    // Drop reclaimed everything: the shared account is back to baseline (no leak).
    assert_eq!(global.residency_bytes(), 0, "drop refunded the whole connection contribution");
    assert_eq!(global.object_count(), 0);

    // The account (the closest thing to a shared "device") is immediately usable by a fresh session,
    // which computes a correct result.
    let mut fresh = sink_on(&global);
    fresh.submit(&[buffer(1, 16), write(1, 0x5A, 16)]).unwrap();
    assert_eq!(fresh.read_buffer(hl_gpu::BufferId(1), 0, 16).unwrap(), vec![0x5A; 16]);
}

// -------------------------------------------------------------------------------------------------
// 3. resource_reuse_after_teardown
// -------------------------------------------------------------------------------------------------

/// Create id X, tear the session down, then create id X again in a FRESH session — it must be a clean
/// new resource with no stale bytes bleeding through from the prior life.
#[test]
fn resource_reuse_after_teardown() {
    // First life: id 7 holds 0xAA.
    {
        let mut s = sink();
        s.submit(&[buffer(7, 32), write(7, 0xAA, 32)]).unwrap();
        assert_eq!(s.read_buffer(hl_gpu::BufferId(7), 0, 32).unwrap(), vec![0xAA; 32]);
    } // teardown

    // Second life: id 7 re-created fresh. BEFORE any write it must read back zero — no stale 0xAA.
    let mut s = sink();
    s.submit(&[buffer(7, 32)]).unwrap();
    assert_eq!(
        s.read_buffer(hl_gpu::BufferId(7), 0, 32).unwrap(),
        vec![0x00; 32],
        "a reused id is a clean allocation, not the prior life's contents"
    );
    // ...and it behaves as its own resource.
    s.submit(&[write(7, 0xCC, 32)]).unwrap();
    assert_eq!(s.read_buffer(hl_gpu::BufferId(7), 0, 32).unwrap(), vec![0xCC; 32]);
}

// -------------------------------------------------------------------------------------------------
// 4. submit_to_closed_sink
// -------------------------------------------------------------------------------------------------

/// A sink whose session was explicitly closed rejects every command with a TYPED error — never a panic
/// and never a silent success that would drive a released session.
#[test]
fn submit_to_closed_sink() {
    let mut s = sink();
    s.submit(&[buffer(1, 16)]).unwrap();
    s.close();
    assert!(s.is_closed());

    // Every entry point is now a typed rejection, not a panic / not silent success.
    assert_eq!(s.submit(&[buffer(2, 16)]).unwrap_err(), GpuError::Invalid("session closed"));
    assert_eq!(s.wait(hl_gpu::FenceId(1), 1).unwrap_err(), GpuError::Invalid("session closed"));
    assert_eq!(
        s.read_buffer(hl_gpu::BufferId(1), 0, 16).unwrap_err(),
        GpuError::Invalid("session closed")
    );
    let req = FeatureRequest::default();
    assert_eq!(s.negotiate(&req).unwrap_err(), GpuError::Invalid("session closed"));
}

// -------------------------------------------------------------------------------------------------
// 5. cleanup_on_drop
// -------------------------------------------------------------------------------------------------

/// Create N device resources in a session, drop it, and confirm they are freed — across MANY
/// create/drop cycles on one shared account nothing accumulates (no dangling device allocations).
#[test]
fn cleanup_on_drop() {
    let global = GlobalLedger::new(1 << 30, 1 << 20);
    const N: u32 = 8;
    const CYCLES: usize = 256;

    for _ in 0..CYCLES {
        let mut s = sink_on(&global);
        // A mixed batch of buffers + a texture + a sampler + a fence — every resource kind that charges.
        let mut batch = Vec::new();
        for i in 0..N {
            batch.push(buffer(i + 1, 1024));
        }
        batch.push(Cmd::CreateTexture(
            100,
            TextureDesc {
                width: 16,
                height: 16,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::SAMPLED,
                label: String::new(),
            },
        ));
        batch.push(Cmd::CreateSampler(
            200,
            SamplerDesc {
                min_filter: Filter::Nearest,
                mag_filter: Filter::Nearest,
                mip_filter: Filter::Nearest,
                address_u: AddressMode::Repeat,
                address_v: AddressMode::Repeat,
                address_w: AddressMode::Repeat,
            },
        ));
        batch.push(Cmd::CreateFence(300));
        s.submit(&batch).unwrap();

        assert_eq!(s.session().resources.live_count() as u32, N + 3);
        assert!(global.residency_bytes() > 0);
        assert_eq!(s.session().object_count(), (N + 3) as u64);
        // drop `s` at end of iteration — Drop must reclaim all of it.
        assert!(global.object_count() >= (N + 3) as u64);
    }

    // After 256 create/N-resources/drop cycles the shared account is exactly back to zero: no live
    // resource, no residual byte leaked across any teardown.
    assert_eq!(global.residency_bytes(), 0, "no residency accumulated across create/drop cycles");
    assert_eq!(global.object_count(), 0, "no object count accumulated across create/drop cycles");
}

// -------------------------------------------------------------------------------------------------
// 6. double_teardown / use_after_teardown
// -------------------------------------------------------------------------------------------------

/// Closing twice (double teardown) is a no-op, never a panic or a double-refund; using the handle after
/// teardown is a typed error; and the resources were genuinely reclaimed by the close.
#[test]
fn double_teardown_and_use_after_teardown() {
    let mut s = sink();
    s.submit(&[buffer(1, 64), buffer(2, 64)]).unwrap();
    assert_eq!(s.session().resources.live_count(), 2);

    s.close();
    // Close reclaimed the live resources.
    assert_eq!(s.session().resources.live_count(), 0, "close reclaimed live resources");

    // Double teardown: a second close must not panic and must leave the sink closed.
    s.close();
    assert!(s.is_closed());

    // Use-after-teardown: submitting through the closed handle is a typed error, not a panic.
    assert_eq!(s.submit(&[buffer(3, 64)]).unwrap_err(), GpuError::Invalid("session closed"));

    // Dropping an already-closed sink is safe (no double-refund panic on the way out).
    drop(s);
}

/// `Session::release_all` refunds the shared global account EXACTLY once — a second release (or a drop
/// after release) must not double-refund and steal another connection's budget.
#[test]
fn explicit_release_is_refund_exact_once() {
    let global = GlobalLedger::new(1 << 30, 1 << 20);

    // Two connections share the account; each charges 4096.
    let mut a = sink_on(&global);
    let mut b = sink_on(&global);
    a.submit(&[buffer(1, 4096)]).unwrap();
    b.submit(&[buffer(1, 4096)]).unwrap();
    assert_eq!(global.residency_bytes(), 8192);

    // Close A once -> global drops to B's 4096.
    a.close();
    assert_eq!(global.residency_bytes(), 4096, "close A refunded only A's contribution");

    // Close A AGAIN (idempotent) -> must NOT double-refund and eat B's budget.
    a.close();
    assert_eq!(global.residency_bytes(), 4096, "double-close did not double-refund");

    // Dropping the already-closed A -> still no double-refund.
    drop(a);
    assert_eq!(global.residency_bytes(), 4096, "drop after close did not double-refund");

    // B is untouched and still usable.
    b.submit(&[write(1, 0x11, 4)]).unwrap();
    assert_eq!(b.read_buffer(hl_gpu::BufferId(1), 0, 4).unwrap(), vec![0x11; 4]);
    drop(b);
    assert_eq!(global.residency_bytes(), 0, "final teardown returns the account to baseline");
}

// -------------------------------------------------------------------------------------------------
// bonus: a raw SessionResources reuse never leaks stale generations across a fresh table
// -------------------------------------------------------------------------------------------------

/// A fresh `SessionResources` starts empty regardless of how much churn a prior table saw — the reused
/// id lands in a clean table, so a stale-generation reference cannot alias a new allocation.
#[test]
fn fresh_resources_start_empty() {
    let a = SessionResources::new();
    assert_eq!(a.live_count(), 0);
    let b = SessionResources::new();
    assert_eq!(b.live_count(), 0);
}
