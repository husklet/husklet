//! Session identity, registry ownership, and the bridge between the export registry and the gate.
//!
//! Slices 1 and 2 left two halves that could not see each other. The registry (`Exports`) knew who held a
//! map; the gate (`Access` inside `ResourceTable::get`/`get_mut`) knew how to refuse. Nothing connected
//! them, and `SessionId` existed with nothing assigning one. This file covers the join.
//!
//! ## Why the state cell is the registry's ONLY representation of a claim
//!
//! The obvious shape is a `MapState` field for the registry's own logic plus an atomic mirrored beside it
//! for the guard to read. That is two representations of one fact, and the drift is invisible in exactly
//! one direction: the registry's own tests all read the field, so they stay green while every guard in
//! the process reads a stale cell and permits what the registry believes it refused. A capability whose
//! rule is enforced by the half nobody tests is worse than one not enforced at all, because it fails as
//! silently wrong data rather than as an error.
//!
//! So `Entry::state` IS the cell, and `MapState` is derived from it on read. There is no second copy to
//! drift.
//!
//! ## Fail-first vs. mutation
//!
//! `session_ids_are_distinct` and `a_registry_is_shared_not_per_session` were written before the code
//! that satisfies them and were watched failing (`SessionId::next` and `Session::with_exports` did not
//! exist; the tests did not compile, then failed, then passed).
//!
//! The bridge tests below were written AFTER `Exports::access`, so they get mutation instead — each rule
//! was reverted one at a time and the matrix records which test caught it. A test whose reversion was
//! never observed to fail is a claim, not evidence. This is the OBSERVED attribution, not the predicted
//! one: two rows differ from what was expected before the mutations were run, and the row for `access`
//! binding the wrong session was caught by a different test than the one written for it.
//!
//! | rule reverted | tests that failed |
//! |---|---|
//! | `map` does not record the claim in the cell | `a_registry_map_is_visible_to_the_gate`, `the_gate_reopens_when_the_registry_unmaps`, `a_departing_session_releases_its_claim` (+2 in `sharing.rs`) |
//! | `unmap` does not clear the cell | `the_gate_reopens_when_the_registry_unmaps` (+2 in `sharing.rs`) |
//! | `access` binds the guard to a session other than the caller | `the_holder_is_never_locked_out_by_its_own_claim` |
//! | `access` does not check party membership | `a_stranger_gets_no_guard_and_a_party_does` |
//! | `Session::drop` does not call `forget_session` | `a_departing_session_releases_its_claim` |
//! | `SessionId::next` returns a constant | 7 of the 9 tests here |
//! | `Session::new` defaults to a per-session `Exports` (the handoff's named trap) | `a_session_has_no_registry_until_one_is_given` |
//!
//! Note what the third row shows: a guard bound to the WRONG session is invisible to every test that
//! only asserts a refusal, because a wrongly-bound guard refuses MORE, not less. It was caught solely by
//! the positive control — the holder reaching its own resource. That is the concrete value of pairing
//! every refusal with a path that must work.

use hl_gpu::protocol::model::capability::{shader_payload, Capabilities, COLOR_FORMATS};
use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::error::GpuError;
use hl_gpu::runtime::model::sharing::{Exports, ResourceKey, SessionId};
use hl_gpu::{Cmd, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION};
use std::sync::Arc;

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

/// One real command that resolves buffer 1 to its native object — the path the gate sits on. Anything
/// that only consulted a predicate would prove nothing about a command.
fn touch(sink: &mut InProcessCommandSink<CpuExecutor>) -> Result<(), GpuError> {
    sink.submit(&[Cmd::WriteBuffer {
        id: 1,
        offset: 0,
        data: vec![0xAB; 4],
    }])
}

fn create_buffer(sink: &mut InProcessCommandSink<CpuExecutor>) {
    sink.submit(&[Cmd::CreateBuffer(
        1,
        BufferDesc {
            size: 256,
            usage: 0,
            label: String::new(),
        },
    )])
    .expect("the positive control must create; every refusal below is vacuous otherwise");
}

/// An export entry standing in for a resource `owner` offers. The registry does not care what the native
/// is, so a byte vector is an honest stand-in for the executor's real object here.
fn export(exports: &Exports, owner: SessionId) -> hl_gpu::runtime::model::sharing::ExportId {
    exports
        .export(
            ResourceKey {
                session: owner,
                kind: "buffer",
                id: 1,
            },
            Arc::new(vec![0u8; 256]) as Arc<dyn std::any::Any + Send + Sync>,
            256,
        )
        .expect("a well-formed export must succeed")
}

#[test]
fn session_ids_are_distinct() {
    // Minted, not chosen. Two connections sharing an id would each be able to unmap the other's claim.
    let ids: Vec<SessionId> = (0..1000).map(|_| SessionId::next()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "every minted session id must be distinct"
    );
    assert!(
        ids.iter().all(|s| s.0 != 0),
        "0 is reserved: the guard encodes a holder as holder+1 and reads 0 as unmapped"
    );
}

#[test]
fn a_session_id_is_not_reissued_after_its_session_drops() {
    // The same reasoning as `ExportId`: a recycled connection id makes a departed session's stale claim
    // indistinguishable from the new occupant's.
    let first = { sink().session().id };
    let second = { sink().session().id };
    assert_ne!(
        first, second,
        "a dropped session's identity must not be handed to the next connection"
    );
}

#[test]
fn a_session_has_no_registry_until_one_is_given() {
    // Fails CLOSED. The tempting alternative — default to a fresh per-session `Exports` — compiles,
    // passes every registry test, and shares nothing, because each connection would be talking to its
    // own table. `None` cannot be mistaken for a working registry.
    assert!(
        sink().session().exports.is_none(),
        "sharing must be wired deliberately by the composition root, never defaulted"
    );
}

#[test]
fn a_registry_is_shared_not_per_session() {
    // Cloning `Exports` shares one table, exactly as `GlobalLedger` does. The positive control is that
    // the SAME id is live in the other clone; a per-session registry would answer `false` here.
    let exports = Exports::new();
    let other = exports.clone();
    let id = export(&exports, SessionId::next());
    assert!(
        other.is_live(id),
        "a clone must see the same table, or every session shares with nobody"
    );
}

#[test]
fn a_registry_map_is_visible_to_the_gate() {
    // The join: a claim taken through the REGISTRY must be refused by the GATE, with no second copy of
    // the state in between. Slice 2 proved the gate refuses a hand-made atomic; this proves the registry
    // drives that same atomic.
    let exports = Exports::new();
    let owner = SessionId::next();
    let other = SessionId::next();
    let id = export(&exports, owner);
    exports.import(other, id).expect("import must succeed");

    let mut sink = sink();
    create_buffer(&mut sink);
    // The sink's session stands in for the OWNER's table here; it watches the same entry.
    let guard = exports
        .access(owner, id)
        .expect("the owner is a party and must get a guard");
    sink.resources_mut()
        .buffers
        .set_guard(1, guard)
        .expect("attach the guard");

    // POSITIVE CONTROL FIRST. A refusal below proves nothing unless this path otherwise works.
    touch(&mut sink).expect("unmapped: the owner's own command must succeed");

    exports
        .map(other, id)
        .expect("the importer claims the resource");
    let refused = touch(&mut sink).expect_err("mapped elsewhere: the owner must be refused");
    assert!(
        matches!(
            refused,
            GpuError::MappedElsewhere {
                kind: "buffer",
                id: 1
            }
        ),
        "the refusal must be the timing-class one a caller recovers from by WAITING, not an \
         invalid-argument error that tells a correct program it is wrong: got {refused:?}"
    );
}

#[test]
fn the_gate_reopens_when_the_registry_unmaps() {
    // The other half. `MappedElsewhere` is the only refusal on this wire that the identical call from
    // the same caller recovers from by waiting, so "it comes back" is part of the contract, not a
    // pleasant side effect.
    let exports = Exports::new();
    let owner = SessionId::next();
    let other = SessionId::next();
    let id = export(&exports, owner);
    exports.import(other, id).expect("import");

    let mut sink = sink();
    create_buffer(&mut sink);
    sink.resources_mut()
        .buffers
        .set_guard(1, exports.access(owner, id).expect("guard"))
        .expect("attach");

    exports.map(other, id).expect("claim");
    assert!(
        touch(&mut sink).is_err(),
        "refused while the other session holds it"
    );
    exports.unmap(other, id).expect("release the claim");
    touch(&mut sink).expect("the identical call must succeed once the holder unmaps");
}

#[test]
fn the_holder_is_never_locked_out_by_its_own_claim() {
    // A guard that refused its own holder would be a deadlock dressed as a safety rule.
    let exports = Exports::new();
    let owner = SessionId::next();
    let other = SessionId::next();
    let id = export(&exports, owner);
    exports.import(other, id).expect("import");

    let mut sink = sink();
    create_buffer(&mut sink);
    sink.resources_mut()
        .buffers
        .set_guard(1, exports.access(owner, id).expect("guard"))
        .expect("attach");

    exports.map(owner, id).expect("the owner claims it");
    touch(&mut sink).expect("the holder must still reach its own resource while it holds the map");
}

#[test]
fn a_stranger_gets_no_guard_and_a_party_does() {
    // Paired, because a refusal from a path that never works proves nothing.
    let exports = Exports::new();
    let owner = SessionId::next();
    let importer = SessionId::next();
    let stranger = SessionId::next();
    let id = export(&exports, owner);
    exports.import(importer, id).expect("import");

    assert!(exports.access(owner, id).is_ok(), "the owner is a party");
    assert!(
        exports.access(importer, id).is_ok(),
        "the importer is a party"
    );
    assert!(
        exports.access(stranger, id).is_err(),
        "a session that can never touch the resource must not be handed a guard nothing consults"
    );
}

#[test]
fn a_departing_session_releases_its_claim() {
    // Session teardown must drop references in BOTH directions. Otherwise a resource ends up permanently
    // `MappedBy` a session that has gone — the exact state `SHARING.md` names as the cost of leaving
    // "unregister while mapped" undefined.
    let exports = Exports::new();
    let owner = SessionId::next();
    let id = export(&exports, owner);

    let importer_id = {
        let sink = sink().with_exports(exports.clone());
        let importer = sink.session().id;
        exports.import(importer, id).expect("import");
        exports.map(importer, id).expect("claim");
        assert!(
            exports.check_access(owner, id).is_err(),
            "positive control: while the importer holds the claim the owner is refused"
        );
        importer
    };

    assert!(
        exports.check_access(owner, id).is_ok(),
        "the departed session's claim must be released, not left pinned forever"
    );
    assert!(
        exports.release_import(importer_id, id).is_err(),
        "the departed session's import reference must be gone too"
    );
}
