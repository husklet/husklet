use super::*;
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
    assert_eq!(
        exports.bytes_charged_to(CUDA),
        0,
        "and the ledger returns to baseline"
    );
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
fn released_owner_loses_every_authorization_while_importer_remains_usable() {
    let exports = Exports::new();
    let id = exported(&exports, 64);
    exports.import(CUDA, id).unwrap();
    let mut resources = hl_gpu::SessionResources::new();
    resources
        .buffers
        .insert_guarded(7, Box::new(1u32), exports.access(GL, id).unwrap())
        .unwrap();
    let mut importer_resources = hl_gpu::SessionResources::new();
    importer_resources
        .buffers
        .insert_guarded(9, Box::new(1u32), exports.access(CUDA, id).unwrap())
        .unwrap();
    exports.owner_release(id).unwrap();

    assert!(exports.access(GL, id).is_err());
    assert!(exports.check_access(GL, id).is_err());
    assert!(exports.map(GL, id).is_err());
    assert!(exports.unmap(GL, id).is_err());
    assert!(exports.release_import(GL, id).is_err());
    assert!(exports.owner_release(id).is_err());
    assert!(
        resources.buffers.get(7).is_err(),
        "pre-acquired owner guard was revoked"
    );

    assert!(exports.access(CUDA, id).is_ok());
    assert!(importer_resources.buffers.get(9).is_ok());
    assert!(exports.check_access(CUDA, id).is_ok());
    exports.map(CUDA, id).unwrap();
    assert!(exports.check_access(CUDA, id).is_ok());
    exports.unmap(CUDA, id).unwrap();
    assert!(exports.check_access(CUDA, id).is_ok());
    assert!(exports.is_live(id));
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
    assert!(
        exports.map(GL, id).is_err(),
        "mapping an already-mapped resource"
    );
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
fn owner_release_implicitly_unmaps_for_a_live_importer() {
    let exports = Exports::new();
    let id = exported(&exports, 64);
    exports.import(CUDA, id).unwrap();
    exports.map(GL, id).unwrap();

    exports.owner_release(id).unwrap();

    assert_eq!(exports.check_access(CUDA, id), Ok(()));
    exports.map(CUDA, id).unwrap();
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
        thread
            .join()
            .expect("no thread may panic; a poisoned mutex is a defect here");
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
