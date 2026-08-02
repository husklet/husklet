//! Account/dispatch FAILURE-ATOMICITY battery — the runtime seam where `account::charge_frame` runs
//! (charging residency) immediately before `dispatch` hands the batch to the executor.
//!
//! The hazard this pins down: a `Create*` over a STILL-LIVE resource id. The executor's id table rejects
//! it as `DuplicateId`; the accountant must agree and reject it too — WITHOUT mutating the connection
//! ledger or the shared global account. A frame the executor will refuse must leave residency +
//! object-count byte-identical to what they were before the frame, no matter how many times a hostile or
//! buggy guest retries it (no upward drift that falsely trips `ResourceLimit`, no downward drift that
//! bypasses the DoS clamp). A subsequent LEGAL create must still charge and dispatch exactly as before.
//!
//! Deterministic: a `FakeClock`, the pure-CPU reference oracle, a bounded shared `GlobalLedger` — a
//! regression reproduces on the first run.

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{
    Cmd, CommandSink, CpuExecutor, FakeClock, GlobalLedger, GpuError, GpuExecutor,
    InProcessCommandSink, Limits, Session,
};

/// A sink whose session shares an explicit bounded `global` account and a fixed clock, so we can watch
/// BOTH the per-connection ledger and the process-global account move (or not) across a frame.
fn sink_on(global: &GlobalLedger) -> InProcessCommandSink<CpuExecutor> {
    let limits = Limits::from_capabilities(CpuExecutor::new().capabilities());
    let session = Session::new(limits, global.clone(), Box::new(FakeClock::new(0)));
    InProcessCommandSink::with_session(session, CpuExecutor::new())
}

fn buffer(id: u32, size: u64) -> Cmd {
    Cmd::CreateBuffer(
        id,
        BufferDesc {
            size,
            usage: buffer_usage::COPY_DST,
            label: String::new(),
        },
    )
}

/// A full snapshot of every accounting quantity a drifting frame could perturb: the per-connection
/// residency bytes / object count / compiled-cache bytes, the live-charge map, and the shared global
/// account's bytes + object count.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountSnapshot {
    conn_bytes: u64,
    conn_objects: u64,
    conn_compiled: u64,
    live: std::collections::BTreeMap<(u8, u32), u64>,
    global_bytes: u64,
    global_objects: u64,
}

fn snapshot(s: &InProcessCommandSink<CpuExecutor>, global: &GlobalLedger) -> AccountSnapshot {
    let sess = s.session();
    AccountSnapshot {
        conn_bytes: sess.residency_bytes(),
        conn_objects: sess.object_count(),
        conn_compiled: sess.compiled_cache_bytes(),
        live: sess
            .account
            .ledger()
            .live
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect(),
        global_bytes: global.residency_bytes(),
        global_objects: global.object_count(),
    }
}

// -------------------------------------------------------------------------------------------------
// 1. a Create over a live id is a typed DuplicateId AND leaves the ledger byte-identical
// -------------------------------------------------------------------------------------------------

#[test]
fn duplicate_create_over_live_id_is_typed_and_charges_nothing() {
    let global = GlobalLedger::new(1 << 30, 1 << 20);
    let mut s = sink_on(&global);

    // A legal first create charges normally.
    s.submit(&[buffer(1, 4096)]).unwrap();
    let before = snapshot(&s, &global);
    assert_eq!(before.conn_bytes, 4096);
    assert_eq!(before.conn_objects, 1);
    assert_eq!(before.global_bytes, 4096);
    assert_eq!(before.global_objects, 1);

    // Re-create the SAME live id, even with a DIFFERENT size (the old drift bug swapped the charge to the
    // new size while the executor rejected the create — inflating residency for a create that never
    // happened). It must be a typed DuplicateId...
    let err = s.submit(&[buffer(1, 1 << 20)]).unwrap_err();
    assert_eq!(
        err,
        GpuError::DuplicateId {
            kind: "buffer",
            id: 1
        }
    );

    // ...and the ENTIRE account is byte-identical to before the rejected frame: no residency swap, no
    // object drift, no global-account movement.
    assert_eq!(
        snapshot(&s, &global),
        before,
        "a rejected duplicate create must not move the account"
    );
}

// -------------------------------------------------------------------------------------------------
// 2. N retries never drift the account (neither up nor down)
// -------------------------------------------------------------------------------------------------

#[test]
fn repeated_duplicate_creates_never_drift_the_account() {
    let global = GlobalLedger::new(1 << 30, 1 << 20);
    let mut s = sink_on(&global);

    // Two live resources of different kinds, so a per-kind drift would show.
    s.submit(&[buffer(1, 4096), buffer(2, 2048), Cmd::CreateFence(9)])
        .unwrap();
    let before = snapshot(&s, &global);

    const N: usize = 64;
    for i in 0..N {
        // Alternate the kind/size of the offending duplicate so any asymmetric drift (buffer vs fence,
        // grow vs shrink) would accumulate over the loop.
        let dup = if i % 2 == 0 {
            buffer(1, (i as u64 + 1) * 8192)
        } else {
            Cmd::CreateFence(9)
        };
        let err = s.submit(&[dup]).unwrap_err();
        match err {
            GpuError::DuplicateId { .. } => {}
            other => panic!("attempt {i}: expected DuplicateId, got {other:?}"),
        }
        // Invariant after EVERY attempt: nothing moved.
        assert_eq!(
            snapshot(&s, &global),
            before,
            "attempt {i} drifted the account"
        );
    }

    // And a duplicate buried AFTER a would-be-legal create in the same frame still rejects the WHOLE
    // frame atomically — the legal create ahead of it must not leak a charge.
    let err = s.submit(&[buffer(50, 512), buffer(1, 64)]).unwrap_err();
    assert_eq!(
        err,
        GpuError::DuplicateId {
            kind: "buffer",
            id: 1
        }
    );
    assert_eq!(
        snapshot(&s, &global),
        before,
        "a mixed frame that rejects must charge nothing at all"
    );
}

// -------------------------------------------------------------------------------------------------
// 3. a legal create AFTER rejections still charges + dispatches exactly as before
// -------------------------------------------------------------------------------------------------

#[test]
fn legal_create_after_rejections_still_charges_and_dispatches() {
    let global = GlobalLedger::new(1 << 30, 1 << 20);
    let mut s = sink_on(&global);

    s.submit(&[buffer(1, 4096)]).unwrap();
    let before = snapshot(&s, &global);

    // A handful of rejected duplicate frames in between.
    for _ in 0..8 {
        assert!(matches!(
            s.submit(&[buffer(1, 999)]).unwrap_err(),
            GpuError::DuplicateId {
                kind: "buffer",
                id: 1
            }
        ));
    }
    assert_eq!(
        snapshot(&s, &global),
        before,
        "rejections left the account untouched"
    );

    // A FRESH legal create now charges exactly its own footprint on top of the untouched baseline and
    // reaches the executor (the resource is really live: it reads back and is counted).
    s.submit(&[buffer(2, 2048)]).unwrap();
    let after = snapshot(&s, &global);
    assert_eq!(
        after.conn_bytes,
        before.conn_bytes + 2048,
        "the legal create charged exactly its size"
    );
    assert_eq!(after.conn_objects, before.conn_objects + 1);
    assert_eq!(after.global_bytes, before.global_bytes + 2048);
    assert_eq!(after.global_objects, before.global_objects + 1);
    assert_eq!(
        s.session().resources.live_count(),
        2,
        "the legal create really dispatched"
    );

    // The legal destroy-then-recreate of a live id in ONE frame is still accepted (the destroy clears the
    // id before the create re-charges it) — the fix only rejects a create over a *still-live* id.
    s.submit(&[Cmd::DestroyBuffer(1), buffer(1, 128)]).unwrap();
    let recycled = snapshot(&s, &global);
    // id 1 went 4096 -> 128; only that delta moved, and the object count is unchanged.
    assert_eq!(recycled.conn_bytes, after.conn_bytes - 4096 + 128);
    assert_eq!(recycled.conn_objects, after.conn_objects);
    assert_eq!(
        s.read_buffer(hl_gpu::BufferId(1), 0, 128).unwrap(),
        vec![0u8; 128]
    );
}

// -------------------------------------------------------------------------------------------------
// 4. an ownership transfer in and back out is arithmetically symmetric
// -------------------------------------------------------------------------------------------------

/// Accepting ownership of an object and then releasing it must return EVERY counter to its starting
/// value. A pipeline is the sharp case: its bytes belong to the compiled-pipeline sub-total as well as
/// the aggregate, so an accept that charges only the aggregate while the matching release refunds the
/// sub-total too underflows `compiled_bytes` — a wrapped ceiling that would then reject every later
/// pipeline (or, in a debug build, an overflow panic).
#[test]
fn ownership_transfer_in_and_out_leaves_every_counter_at_its_start() {
    let global = GlobalLedger::new(1 << 30, 1 << 20);
    let limits = Limits::from_capabilities(CpuExecutor::new().capabilities());
    let mut session = Session::new(limits, global.clone(), Box::new(FakeClock::new(0)));

    let before = (
        session.residency_bytes(),
        session.object_count(),
        session.compiled_cache_bytes(),
    );
    hl_gpu::runtime::service::account::accept_ownership(
        &mut session,
        hl_gpu::runtime::KIND_PIPELINE,
        11,
        4096,
    )
    .expect("accepting a transferred pipeline charges it");
    let released = hl_gpu::runtime::service::account::release_ownership(
        &mut session,
        hl_gpu::runtime::KIND_PIPELINE,
        11,
    )
    .expect("releasing it refunds it");
    assert_eq!(released, 4096);
    assert_eq!(
        (
            session.residency_bytes(),
            session.object_count(),
            session.compiled_cache_bytes()
        ),
        before,
        "an accept/release round trip must leave the ledger exactly as it was"
    );
}

/// A surface and a texture of the same format and extent are charged the same residency.
///
/// `bytes_per_texel` answers `None` for depth/stencil and for every block-compressed format, so a
/// caller taking a default invents a footprint for exactly the formats it could not describe. The
/// texture path found this and special-cased `Depth24PlusStencil8` as an 8-byte plane; the surface path
/// kept the bare 4-byte default. The same format was therefore charged 4 bytes per texel as a surface
/// and 8 as a texture — measured at 256x256, a 2x undercharge — and nothing validates a surface's
/// format, so it is reachable rather than theoretical.
///
/// Undercharging is the direction that matters: residency limits stop reflecting what the executor
/// actually keeps resident. The test compares the two paths against each other rather than against a
/// constant, so it pins the agreement itself and cannot be satisfied by both drifting together.
#[test]
fn a_surface_and_a_texture_of_one_format_are_charged_alike() {
    use hl_gpu::protocol::model::descriptor::{SurfaceDesc, TextureDesc};
    use hl_gpu::protocol::model::enums::{texture_usage, TextureFormat};
    use hl_gpu::SurfaceToken;

    for format in [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Depth24PlusStencil8,
        TextureFormat::Depth32Float,
        TextureFormat::Rgba32Float,
    ] {
        let (w, h) = (256u32, 256u32);

        let mut as_surface = sink_on(&GlobalLedger::unbounded());
        as_surface
            .submit(&[Cmd::CreateSurface(
                1,
                SurfaceDesc {
                    width: w,
                    height: h,
                    format,
                    token: SurfaceToken::new(1).unwrap(),
                },
            )])
            .expect("surface create");
        let surface_bytes = as_surface.session().residency_bytes();

        let mut as_texture = sink_on(&GlobalLedger::unbounded());
        let created = as_texture.submit(&[Cmd::CreateTexture(
            1,
            TextureDesc {
                width: w,
                height: h,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: hl_gpu::protocol::model::enums::TextureDim::D2,
                format,
                usage: texture_usage::RENDER_TARGET,
                label: String::new(),
            },
        )]);
        // A format the texture path refuses outright carries no comparison; the surface path is the one
        // under test and it accepted the format, which is the asymmetry worth knowing about either way.
        if created.is_err() {
            continue;
        }
        let texture_bytes = as_texture.session().residency_bytes();

        assert_eq!(
            surface_bytes, texture_bytes,
            "{format:?} at {w}x{h} is charged {surface_bytes} bytes as a surface and {texture_bytes} \
             as a texture; one of the two paths is inventing a footprint for a format the other \
             describes",
        );
    }
}
