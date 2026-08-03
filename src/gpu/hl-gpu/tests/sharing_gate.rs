//! The gate: a mapped resource is refused **through a real command path**, not merely by a predicate.
//!
//! `SHARING.md` states this as a condition rather than a preference — "if the check cannot be placed at
//! that single lookup point, this capability must not ship" — because a mapping rule scattered across
//! command handlers is complete the day it is written and incomplete a month later. This file is what
//! turns that from a claim into a test.
//!
//! ## Why the placement is total, and not merely "we checked the handlers"
//!
//! The argument is a closed enumeration over ONE file, not a codebase-wide grep:
//!
//! 1. `ResourceTable::live` is a **private** field, so no code outside `protocol/model/id.rs` can reach a
//!    slot except through that type's public API.
//! 2. Of that API, exactly two methods hand out a reference to the native object: `get` and `get_mut`.
//!    (`insert`, `remove`, `contains`, `generation`, `len`, `is_empty` and the transaction methods do
//!    not.) `iter` also did, and had **zero callers anywhere in the repository**, so it was deleted
//!    rather than guarded — a dead method that yields an unguarded native is a hole waiting for its
//!    first caller, and the enumeration is only closed once it is gone.
//! 3. The guard lives in the generic `ResourceTable<T>`, so all eight resource kinds get it by
//!    construction rather than by eight separate edits that could each be forgotten.
//!
//! That is a type-and-privacy argument, which is why it is worth more than an audit: a new command added
//! next year cannot reach a native object without passing the check, because there is no other way to
//! reach one. The 233 `get`/`get_mut` call sites across the two executors are all downstream of it.
//!
//! ## Fail-first
//!
//! Unlike slice 1, this one was genuinely available fail-first and was done that way: the guard plumbing
//! (`Access`, `set_guard`, the `Slot` field) was landed with the check ABSENT from `get`/`get_mut`, and
//! `a_mapped_buffer_is_refused_through_a_real_command` was watched failing — the command succeeded
//! against a resource another session had mapped. The check was then added and it passed. The refusal is
//! therefore known to come from the check and not from some other part of the path.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use hl_gpu::protocol::model::capability::{shader_payload, Capabilities, COLOR_FORMATS};
use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::error::GpuError;
use hl_gpu::protocol::model::id::Access;
use hl_gpu::{Cmd, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION};

/// Session identities as the guard encodes them. `Access::new` stores `session + 1`, so `0` stays free
/// to mean "unmapped" and the check is one compare against a single atomic.
const GL: u64 = 1;
const CUDA: u64 = 2;

fn sink() -> InProcessCommandSink<CpuExecutor> {
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    sink.negotiate(&FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: Capabilities::command_bits(&[]),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
        ..FeatureRequest::default()
    })
    .expect("negotiate");
    sink
}

/// Create two buffers and return the shared map-state cell the guard on `guarded` watches.
fn buffers(sink: &mut InProcessCommandSink<CpuExecutor>) -> Arc<AtomicU64> {
    sink.submit(&[
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 256,
                usage: 0,
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            2,
            BufferDesc {
                size: 256,
                usage: 0,
                label: String::new(),
            },
        ),
    ])
    .expect("the positive control must create; every refusal below is vacuous otherwise");
    let state = Arc::new(AtomicU64::new(0));
    sink.resources_mut()
        .buffers
        .set_guard(1, Access::new(Arc::clone(&state), GL))
        .expect("attach the guard to the shared buffer");
    state
}

/// One real command that touches buffer 1. `WriteBuffer` is the smallest top-level command that
/// resolves a buffer id to its native object, which is exactly the path the gate sits on.
fn touch(sink: &mut InProcessCommandSink<CpuExecutor>) -> Result<(), GpuError> {
    sink.submit(&[Cmd::WriteBuffer {
        id: 1,
        offset: 0,
        data: vec![0xAB; 64],
    }])
    .map(|_| ())
}

#[test]
fn a_mapped_buffer_is_refused_through_a_real_command() {
    let mut sink = sink();
    let state = buffers(&mut sink);

    // Positive control FIRST. A refusal proves nothing without a path that otherwise works, and this
    // exact test shape has previously passed against a setup where every submission failed.
    touch(&mut sink)
        .expect("unmapped: the command must succeed, or the refusal below means nothing");

    // CUDA takes the map. GL's table must now refuse to resolve buffer 1.
    state.store(CUDA + 1, std::sync::atomic::Ordering::Release);
    let refused = touch(&mut sink);
    assert!(
        matches!(refused, Err(GpuError::MappedElsewhere { kind: "buffer", id: 1 })),
        "a command touching a resource mapped by another connection must be refused at the resolution \
         point, and must name the resource. Got {refused:?}"
    );

    // And it comes back. A guard that never releases is a wedge, not a gate.
    state.store(0, std::sync::atomic::Ordering::Release);
    touch(&mut sink).expect("after unmap the same command on the same path must succeed again");
}

#[test]
fn the_holder_itself_is_never_locked_out() {
    let mut sink = sink();
    let state = buffers(&mut sink);
    // This table belongs to GL, so GL holding the map must not block GL.
    state.store(GL + 1, std::sync::atomic::Ordering::Release);
    touch(&mut sink).expect("the session holding the map must retain access to its own resource");
}

#[test]
fn an_unguarded_resource_is_never_affected_by_anyone_elses_map() {
    let mut sink = sink();
    let state = buffers(&mut sink);
    state.store(CUDA + 1, std::sync::atomic::Ordering::Release);
    // Buffer 2 carries no guard, so a map on buffer 1 must not touch it.
    sink.submit(&[Cmd::WriteBuffer {
        id: 2,
        offset: 0,
        data: vec![0xCD; 64],
    }])
    .expect("an unshared resource must be unaffected by a map on a different resource");
}

/// The gate must cover a resource used as a WRITE destination, not only as a read source. A guard
/// checked on reads and not on writes is exactly the "three of four paths learned the new case" shape,
/// and it fails in the worst direction: silently wrong data rather than an error.
#[test]
fn the_gate_covers_a_resource_used_as_a_destination() {
    let mut sink = sink();
    sink.submit(&[
        Cmd::CreateBuffer(
            3,
            BufferDesc {
                size: 256,
                usage: 0,
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            4,
            BufferDesc {
                size: 256,
                usage: 0,
                label: String::new(),
            },
        ),
    ])
    .expect("create");
    let state = Arc::new(AtomicU64::new(0));
    sink.resources_mut()
        .buffers
        .set_guard(4, Access::new(Arc::clone(&state), GL))
        .expect("guard the DESTINATION");

    let write = |sink: &mut InProcessCommandSink<CpuExecutor>| {
        sink.submit(&[Cmd::WriteBuffer {
            id: 4,
            offset: 0,
            data: vec![0xEF; 64],
        }])
        .map(|_| ())
    };
    write(&mut sink).expect("positive control: the write must work while unmapped");
    state.store(CUDA + 1, std::sync::atomic::Ordering::Release);
    assert!(
        matches!(
            write(&mut sink),
            Err(GpuError::MappedElsewhere {
                kind: "buffer",
                id: 4
            })
        ),
        "a mapped resource must be refused as a destination, not only as a source"
    );
}

/// What the gate costs on the hottest path in the service.
///
/// Not a distribution — the coordinator's bar for this slice was "know whether you have made every
/// command slower, and say so". It reports the unguarded case (what every existing command pays, which
/// is the number that matters, since sharing is rare and non-sharing is universal) beside the guarded
/// one, at two table sizes so a per-call cost that scales with the table would show up as divergence.
///
/// `cargo test -p hl-gpu --test sharing_gate -- --ignored --nocapture cost`
#[test]
#[ignore]
fn cost_of_the_guard() {
    use std::time::Instant;

    for size in [10u32, 10_000] {
        let mut sink = sink();
        let creates: Vec<Cmd> = (1..=size)
            .map(|i| {
                Cmd::CreateBuffer(
                    i,
                    BufferDesc {
                        size: 256,
                        usage: 0,
                        label: String::new(),
                    },
                )
            })
            .collect();
        sink.submit(&creates).expect("create");

        // Measured on `get` DIRECTLY rather than through `submit`. The first attempt timed whole
        // submissions and reported the guarded case as FASTER at both table sizes — a negative delta,
        // which is not a speedup but proof the instrument had no resolution: the submit path (a Vec
        // allocation, a transaction over eight tables) dwarfs one resolve. A number whose sign is
        // impossible is a measurement of the harness, so the loop below times the thing under test.
        let rounds = 2_000_000u32;
        let table = &sink.resources().buffers;
        let start = Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(table.get(std::hint::black_box(1)).unwrap());
        }
        let unguarded = start.elapsed();

        let state = Arc::new(AtomicU64::new(0));
        sink.resources_mut()
            .buffers
            .set_guard(1, Access::new(Arc::clone(&state), GL))
            .expect("guard");
        let table = &sink.resources().buffers;
        let start = Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(table.get(std::hint::black_box(1)).unwrap());
        }
        let guarded = start.elapsed();

        println!(
            "table={size:>6}  unguarded={:>6.2}ns/resolve  guarded={:>6.2}ns/resolve  delta={:+.2}ns",
            unguarded.as_nanos() as f64 / rounds as f64,
            guarded.as_nanos() as f64 / rounds as f64,
            (guarded.as_nanos() as f64 - unguarded.as_nanos() as f64) / rounds as f64,
        );
    }
}

/// A host emitting the new acknowledgement to a guest that PREDATES it must land on `Unstated`.
///
/// This is the property that makes `ACK_MAPPED_ELSEWHERE` additive rather than breaking, and it was a
/// claim about a wildcard arm until it was measured. A wildcard doing the right thing is true only until
/// someone adds a match arm above it, so the old guest is reconstructed literally below — the exact
/// `from_ack` body as it stood before the code existed — and fed the new byte.
#[test]
fn an_older_guest_reads_a_newer_code_as_unstated() {
    use hl_gpu::transport::model::header::*;

    /// `RefusalKind::from_ack` exactly as it was before `ACK_MAPPED_ELSEWHERE` existed. Copied rather
    /// than referenced on purpose: the point is to run the OLD decoder against the NEW byte.
    fn from_ack_before(ack: u8) -> RefusalKind {
        match ack {
            ACK_UNSUPPORTED => RefusalKind::Unsupported,
            ACK_RESOURCE_LIMIT => RefusalKind::ResourceLimit,
            ACK_INVALID => RefusalKind::Invalid,
            ACK_OUT_OF_BOUNDS => RefusalKind::OutOfBounds,
            ACK_UNKNOWN_ID => RefusalKind::UnknownId,
            ACK_KERNEL => RefusalKind::Kernel,
            _ => RefusalKind::Unstated,
        }
    }

    // The measurement: the old decoder, the new byte.
    assert_eq!(
        from_ack_before(ACK_MAPPED_ELSEWHERE),
        RefusalKind::Unstated,
        "an older guest must read the new code as an unclassified refusal — still a refusal, and still \
         recoverable. Anything else means this wire change was not additive."
    );

    // The reconstruction is only faithful if it still agrees with the real decoder everywhere else. Two
    // copies of a match that have silently diverged would make the assertion above meaningless.
    for ack in [
        ACK_UNSUPPORTED,
        ACK_RESOURCE_LIMIT,
        ACK_INVALID,
        ACK_OUT_OF_BOUNDS,
        ACK_UNKNOWN_ID,
        ACK_KERNEL,
        ACK_FAIL,
        200,
    ] {
        assert_eq!(
            from_ack_before(ack),
            RefusalKind::from_ack(ack),
            "the reconstructed old decoder disagrees with the current one on {ack}, so it is not a \
             faithful stand-in for the old guest and the assertion above proves nothing"
        );
    }

    // And the new byte is genuinely new: the current decoder must NOT read it as Unstated.
    assert_eq!(
        RefusalKind::from_ack(ACK_MAPPED_ELSEWHERE),
        RefusalKind::MappedElsewhere,
        "a positive control — if the current decoder also said Unstated, the test above would pass \
         while the feature did nothing"
    );
    assert_eq!(
        RefusalKind::MappedElsewhere.ack(),
        ACK_MAPPED_ELSEWHERE,
        "round trip"
    );
}
