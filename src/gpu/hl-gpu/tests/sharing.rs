//! The export registry's invariants, edge by edge from `src/gpu/hl-gpu/SHARING.md`.
//!
//! Every refusal here is paired with a positive control through the same path. A refusal proves nothing
//! without a path that otherwise works: a test asserting a bad input is rejected can be measuring its own
//! broken setup, where a valid call and an invalid one are refused identically and both assertions pass
//! while establishing nothing.
//!
//! Each assertion was run against a deliberately reverted registry to confirm it DETECTS rather than
//! decorates. Eight rules were broken one at a time and every one was caught by the test that guards it:
//!
//! | rule reverted | test that failed |
//! |---|---|
//! | `ExportId`s recycled from a length counter | `export_ids_are_never_reused` |
//! | owner release frees immediately instead of retaining | `the_owner_freeing_retains_the_storage_and_moves_the_charge` |
//! | the charge never moves to the importer | that test, and `a_handle_from_a_departed_owner_still_serves_its_importer` |
//! | the guard always permits | `a_mapped_resource_is_refused_to_every_other_session` |
//! | double map allowed | `double_map_and_foreign_unmap_are_refused` |
//! | `release_import` does not verify the caller holds a reference | `releasing_an_import_you_do_not_hold_is_refused` |
//! | `forget_session` drops importers when the owner leaves | `a_handle_from_a_departed_owner_still_serves_its_importer` |
//! | export not idempotent | `export_is_idempotent_per_resource` |
//!
//! A test whose reversion was never observed to fail is a claim, not evidence; every row above was run.

use std::sync::Arc;

use hl_gpu::runtime::model::sharing::{ExportId, Exports, ResourceKey, SessionId};

const GL: SessionId = SessionId(1);
const CUDA: SessionId = SessionId(2);
const OTHER: SessionId = SessionId(3);

fn key(session: SessionId, id: u32) -> ResourceKey {
    ResourceKey {
        session,
        kind: "buffer",
        id,
    }
}

fn resource(tag: u32) -> Arc<dyn std::any::Any + Send + Sync> {
    Arc::new(tag)
}

fn exported(exports: &Exports, bytes: u64) -> ExportId {
    exports
        .export(key(GL, 7), resource(0xAB), bytes)
        .expect("the positive control must export; every refusal below is vacuous otherwise")
}

// --- identity ---------------------------------------------------------------------------------

#[test]
fn export_ids_are_never_reused() {
    let exports = Exports::new();
    let first = exported(&exports, 1024);
    exports.owner_release(first).unwrap();
    assert!(
        !exports.is_live(first),
        "with no importers the entry must be collected on owner release"
    );
    let second = exports
        .export(key(GL, 7), resource(0xCD), 1024)
        .expect("re-exporting the same slot must work");
    assert_ne!(
        first, second,
        "an ExportId was recycled. That is the one thing this registry must never do: an import naming \
         the dead handle would then silently succeed against an unrelated resource, which is a wrong \
         answer where the design owes an error."
    );
    // And the dead handle must still be dead, not resurrected by the new entry.
    assert!(
        exports.import(CUDA, first).is_err(),
        "the retired handle resolved to the new entry"
    );
    assert!(
        exports.import(CUDA, second).is_ok(),
        "positive control: the live handle must import through the same path"
    );
}

#[test]
fn export_is_idempotent_per_resource() {
    let exports = Exports::new();
    let first = exports.export(key(GL, 7), resource(1), 512).unwrap();
    let again = exports.export(key(GL, 7), resource(1), 512).unwrap();
    assert_eq!(
        first, again,
        "one entry per resource; two entries mean two refcounts and one of them will be wrong"
    );
    let different = exports.export(key(GL, 8), resource(2), 512).unwrap();
    assert_ne!(
        first, different,
        "positive control: a different resource must get its own entry"
    );
}

#[test]
fn the_importer_cannot_widen_the_length() {
    let exports = Exports::new();
    let id = exported(&exports, 4096);
    let (_, bytes) = exports.import(CUDA, id).unwrap();
    assert_eq!(
        bytes, 4096,
        "the export entry carries the authoritative length; a size the two sides disagree about is an \
         out-of-bounds kernel no bounds check catches"
    );
}

#[test]
fn a_zero_length_export_is_refused_and_a_real_one_is_not() {
    let exports = Exports::new();
    assert!(
        exports.export(key(GL, 7), resource(1), 0).is_err(),
        "the sibling creation paths refuse a zero size; the export path must not be the one that does not"
    );
    assert!(
        exports.export(key(GL, 7), resource(1), 1).is_ok(),
        "positive control: a one-byte export must succeed through the same path"
    );
}

#[test]
fn a_session_cannot_import_its_own_export() {
    let exports = Exports::new();
    let id = exported(&exports, 256);
    assert!(
        exports.import(GL, id).is_err(),
        "a self-alias breaks every refcount rule, which all assume distinct sessions"
    );
    assert!(
        exports.import(CUDA, id).is_ok(),
        "positive control: another session must import through the same path"
    );
}

#[test]
fn duplicate_import_in_one_session_is_refused() {
    let exports = Exports::new();
    let id = exported(&exports, 256);
    assert!(exports.import(CUDA, id).is_ok(), "the first import must work");
    assert!(
        exports.import(CUDA, id).is_err(),
        "matches what a real driver does with a double-registered buffer, and the duplicate-create rule \
         the resource table already enforces"
    );
    assert!(
        exports.import(OTHER, id).is_ok(),
        "positive control: a different session must still import"
    );
}

// --- edge 1: owner frees while an importer still references -------------------------------------

#[test]
fn the_owner_freeing_retains_the_storage_and_moves_the_charge() {
    let exports = Exports::new();
    let id = exported(&exports, 2048);
    let (handle, _) = exports.import(CUDA, id).unwrap();

    assert_eq!(
        exports.bytes_charged_to(GL),
        2048,
        "before release the owner pays"
    );
    assert_eq!(exports.bytes_charged_to(CUDA), 0);

    exports.owner_release(id).unwrap();

    assert!(
        exports.is_live(id),
        "the destroy must SUCCEED and the storage be retained while an importer remains — refusing \
         breaks legal application behaviour"
    );
    assert_eq!(
        exports.bytes_charged_to(GL),
        0,
        "the owner's ledger must be credited"
    );
    assert_eq!(
        exports.bytes_charged_to(CUDA),
        2048,
        "the charge follows the last live reference; silent retention would be an invisible leak"
    );
    assert_eq!(
        *handle.downcast_ref::<u32>().unwrap(),
        0xAB,
        "the importer's data must survive the owner's destroy"
    );

    exports.release_import(CUDA, id).unwrap();
    assert!(
        !exports.is_live(id),
        "once the last reference drops the object is actually freed"
    );
    assert_eq!(exports.bytes_charged_to(CUDA), 0, "and the ledger returns to baseline");
}

// --- edge 2: a handle from a connection that has gone away --------------------------------------

#[test]
fn a_handle_from_a_departed_owner_still_serves_its_importer() {
    let exports = Exports::new();
    let id = exported(&exports, 512);
    exports.import(CUDA, id).unwrap();
    exports.forget_session(GL);
    assert!(
        exports.is_live(id),
        "session teardown must not free an object that still has importers"
    );
    assert_eq!(exports.bytes_charged_to(CUDA), 512);
    exports.release_import(CUDA, id).unwrap();
    assert!(!exports.is_live(id));
}

#[test]
fn an_importer_departing_last_actually_frees_the_object() {
    let exports = Exports::new();
    let id = exported(&exports, 512);
    exports.import(CUDA, id).unwrap();
    exports.forget_session(GL);
    exports.forget_session(CUDA);
    assert!(
        !exports.is_live(id),
        "with both parties gone the object must be freed, not retained forever"
    );
}

#[test]
fn a_stale_export_id_is_a_typed_error_and_never_a_default() {
    let exports = Exports::new();
    let live = exported(&exports, 512);
    let stale = ExportId(9_999_999);
    assert!(
        exports.import(CUDA, stale).is_err(),
        "an unknown handle must fail; returning a default would make 'could not reach the subject' \
         indistinguishable from 'here is your buffer'"
    );
    assert!(
        exports.check_access(CUDA, stale).is_err(),
        "and the guard must refuse it too rather than defaulting to permitted"
    );
    assert!(
        exports.import(CUDA, live).is_ok(),
        "positive control: a live handle imports through the same path"
    );
    assert!(
        exports.check_access(CUDA, live).is_ok(),
        "positive control: the guard permits an unmapped live resource"
    );
}

// --- edge 3b: the data race, as a state machine -------------------------------------------------

#[test]
fn a_mapped_resource_is_refused_to_every_other_session() {
    let exports = Exports::new();
    let id = exported(&exports, 512);
    exports.import(CUDA, id).unwrap();
    exports.import(OTHER, id).unwrap();

    assert!(
        exports.check_access(GL, id).is_ok(),
        "positive control: before any map, every party may touch it"
    );
    assert!(exports.check_access(CUDA, id).is_ok());

    exports.map(CUDA, id).unwrap();
    assert!(
        exports.check_access(CUDA, id).is_ok(),
        "the holder must retain access — a guard that locks everyone out is not a guard, it is a wedge"
    );
    assert!(
        exports.check_access(GL, id).is_err(),
        "the owner must be refused while another session holds the map"
    );
    assert!(
        exports.check_access(OTHER, id).is_err(),
        "and so must a third party"
    );

    exports.unmap(CUDA, id).unwrap();
    assert!(
        exports.check_access(GL, id).is_ok(),
        "positive control: access returns after unmap, so the refusal above was the map and not a \
         permanently broken path"
    );
}

#[test]
fn double_map_and_foreign_unmap_are_refused() {
    let exports = Exports::new();
    let id = exported(&exports, 512);
    exports.import(CUDA, id).unwrap();
    exports.map(CUDA, id).unwrap();
    assert!(exports.map(GL, id).is_err(), "mapping an already-mapped resource");
    assert!(
        exports.map(CUDA, id).is_err(),
        "including by the holder — a recursive map would make the unmap count ambiguous"
    );
    assert!(
        exports.unmap(GL, id).is_err(),
        "a session that does not hold the map must not release it"
    );
    assert!(
        exports.unmap(CUDA, id).is_ok(),
        "positive control: the holder can release it"
    );
    assert!(
        exports.map(GL, id).is_ok(),
        "positive control: and it can then be mapped by someone else"
    );
}

#[test]
fn a_non_party_cannot_map() {
    let exports = Exports::new();
    let id = exported(&exports, 512);
    assert!(
        exports.map(OTHER, id).is_err(),
        "a session that neither owns nor imports this resource has no business claiming it"
    );
    assert!(
        exports.map(GL, id).is_ok(),
        "positive control: the owner can map through the same path"
    );
}

#[test]
fn releasing_an_import_while_mapped_is_defined_as_an_implicit_unmap() {
    let exports = Exports::new();
    let id = exported(&exports, 512);
    exports.import(CUDA, id).unwrap();
    exports.map(CUDA, id).unwrap();
    exports.release_import(CUDA, id).unwrap();
    assert!(
        exports.check_access(GL, id).is_ok(),
        "leaving this undefined is how a resource ends up permanently mapped by a session that has gone"
    );
}

#[test]
fn releasing_an_import_you_do_not_hold_is_refused() {
    let exports = Exports::new();
    let id = exported(&exports, 512);
    exports.import(CUDA, id).unwrap();
    assert!(
        exports.release_import(OTHER, id).is_err(),
        "or one session could decrement another's reference and free a live resource"
    );
    assert!(
        exports.release_import(CUDA, id).is_ok(),
        "positive control: the real importer can release"
    );
}

// --- edge 3a: the table race --------------------------------------------------------------------

#[test]
fn concurrent_import_and_release_leave_a_consistent_refcount() {
    let exports = Exports::new();
    let id = exported(&exports, 512);
    let mut threads = Vec::new();
    for n in 0..8u64 {
        let exports = exports.clone();
        threads.push(std::thread::spawn(move || {
            let session = SessionId(100 + n);
            // Each thread takes and drops a reference many times. Any lost update in the refcount frees
            // the object while others hold it, and `is_live` below catches it.
            for _ in 0..200 {
                exports.import(session, id).unwrap();
                exports.check_access(session, id).ok();
                exports.release_import(session, id).unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().expect("no thread may panic; a poisoned mutex is a defect here");
    }
    assert!(
        exports.is_live(id),
        "the owner never released, so the entry must survive every importer coming and going"
    );
    assert_eq!(
        exports.bytes_charged_to(GL),
        512,
        "and the charge must be back with the owner alone"
    );
}
