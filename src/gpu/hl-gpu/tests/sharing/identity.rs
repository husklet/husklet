use super::*;
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
fn prepared_owner_release_with_no_importers_collects_the_export() {
    let exports = Exports::new();
    let id = exports
        .debug_export_accounted(key(GL, 7), resource(1), 64, Account::new())
        .unwrap();
    let plan = exports.debug_prepare_owner_release(GL, id).unwrap();
    plan.commit();
    assert!(
        !exports.is_live(id),
        "the zero-import owner release must remove the registry entry"
    );
    assert!(
        exports.import(CUDA, id).is_err(),
        "the retired capability must remain stale"
    );
}

#[test]
fn zero_import_owner_release_refunds_the_exact_global_charge() {
    let exports = Exports::new();
    let global = hl_gpu::GlobalLedger::new(1024, 8);
    let baseline = global.snapshot();
    let owner = Account::new();
    let mut charged = hl_gpu::Ledger::default();
    charged.live.insert((hl_gpu::runtime::KIND_BUFFER, 7), 64);
    charged.totals = hl_gpu::runtime::Totals {
        bytes: 64,
        objects: 1,
        compiled_bytes: 0,
    };
    owner
        .debug_commit_ledger(
            hl_gpu::runtime::Totals::default(),
            charged,
            1024,
            8,
            &global,
        )
        .unwrap();
    let id = exports
        .debug_export_accounted_with_global(key(GL, 7), resource(1), 64, owner.clone(), &global)
        .unwrap();

    exports
        .debug_prepare_owner_release(GL, id)
        .unwrap()
        .commit();

    assert_eq!(global.snapshot(), baseline);
    assert_eq!(owner.ledger().totals, hl_gpu::runtime::Totals::default());
    assert!(!exports.is_live(id));
}

#[test]
fn a_shared_registry_rejects_a_second_global_before_import_publication() {
    let exports = Exports::new();
    let global_a = hl_gpu::GlobalLedger::new(1024, 8);
    let global_b = hl_gpu::GlobalLedger::new(1024, 8);
    let baseline_a = global_a.snapshot();
    let baseline_b = global_b.snapshot();
    let owner = Account::new();
    let mut charged = hl_gpu::Ledger::default();
    charged.live.insert((hl_gpu::runtime::KIND_BUFFER, 7), 64);
    charged.totals = hl_gpu::runtime::Totals {
        bytes: 64,
        objects: 1,
        compiled_bytes: 0,
    };
    owner
        .debug_commit_ledger(
            hl_gpu::runtime::Totals::default(),
            charged,
            1024,
            8,
            &global_a,
        )
        .unwrap();
    let id = exports
        .debug_export_accounted_with_global(key(GL, 7), resource(1), 64, owner, &global_a)
        .unwrap();

    assert!(matches!(
        exports.debug_prepare_import_with_global(CUDA, id, Account::new(), &global_b),
        Err(hl_gpu::GpuError::Invalid(
            "sharing registry reused with different global authority"
        ))
    ));
    exports
        .debug_prepare_owner_release(GL, id)
        .unwrap()
        .commit();

    assert_eq!(global_a.snapshot(), baseline_a);
    assert_eq!(global_b.snapshot(), baseline_b);
    assert!(!exports.is_live(id));
}

#[test]
fn concurrent_import_cannot_enter_after_owner_destroy_is_prepared() {
    let exports = Exports::new();
    let id = exports
        .debug_export_accounted(key(GL, 7), resource(1), 64, Account::new())
        .unwrap();
    let destroy = exports.debug_prepare_owner_release(GL, id).unwrap();
    let contender = exports.clone();
    let refusal = std::thread::spawn(move || contender.import(CUDA, id))
        .join()
        .unwrap();
    assert!(matches!(
        refusal,
        Err(hl_gpu::GpuError::MappedElsewhere { .. })
    ));
    destroy.commit();
    assert!(!exports.is_live(id));
    assert!(
        exports.import(CUDA, id).is_err(),
        "commit retired the export unconditionally"
    );
}

#[test]
fn teardown_cancels_a_prepared_plan_at_its_bounded_deadline() {
    let exports = Exports::new();
    let owner = Account::new();
    let importer = Account::new();
    let global = hl_gpu::GlobalLedger::new(1024, 8);
    let mut owner_ledger = hl_gpu::Ledger::default();
    owner_ledger
        .live
        .insert((hl_gpu::runtime::KIND_BUFFER, 7), 64);
    owner_ledger.totals = hl_gpu::runtime::Totals {
        bytes: 64,
        objects: 1,
        compiled_bytes: 0,
    };
    owner
        .debug_commit_ledger(
            hl_gpu::runtime::Totals::default(),
            owner_ledger,
            1024,
            8,
            &global,
        )
        .unwrap();
    let id = exports
        .debug_export_accounted_with_global(key(GL, 7), resource(1), 64, owner, &global)
        .unwrap();
    exports.import(CUDA, id).unwrap();
    importer.debug_reserve(id.0, 64, 1024, 8).unwrap();
    exports
        .attach_import_account(CUDA, 9, id, importer.clone())
        .unwrap();
    let pending = exports.debug_prepare_owner_release(GL, id).unwrap();
    let waiter_exports = exports.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        waiter_exports.forget_session(GL);
        done_tx.send(()).unwrap();
    });
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("teardown cancels a merely prepared lease at its deadline");
    pending.commit(); // stale cancelled plan is an idempotent no-op
    waiter.join().unwrap();
    assert!(
        exports.is_live(id),
        "the importer keeps the storage live after owner teardown"
    );
    assert_eq!(
        importer.reserved_bytes(),
        0,
        "teardown transfers the reservation into the live payer charge"
    );
    assert_eq!(
        global.residency_bytes(),
        64,
        "one physical charge survives owner teardown"
    );
    exports.forget_session(CUDA);
    assert_eq!(
        global.residency_bytes(),
        0,
        "last-importer teardown refunds the physical charge"
    );
    assert!(!exports.is_live(id));
}

#[test]
fn stale_plan_drop_cannot_clear_its_successor_token() {
    let exports = Exports::new();
    let id = exports
        .debug_export_accounted(key(GL, 7), resource(1), 64, Account::new())
        .unwrap();
    let stale = exports.debug_prepare_owner_release(GL, id).unwrap();
    exports.settle_transition(id, Duration::ZERO);
    let successor = exports.debug_prepare_owner_release(GL, id).unwrap();
    drop(stale);
    assert!(
        matches!(
            exports.import(CUDA, id),
            Err(hl_gpu::GpuError::MappedElsewhere { .. })
        ),
        "stale Drop must not clear the successor's token"
    );
    drop(successor);
    assert!(exports.import(CUDA, id).is_ok());
}

#[test]
fn one_account_authority_cannot_back_two_session_ids() {
    let exports = Exports::new();
    let account = Account::new();
    let id = exports
        .debug_export_accounted(key(GL, 7), resource(1), 64, account.clone())
        .unwrap();
    exports.import(CUDA, id).unwrap();
    account.debug_reserve(id.0, 64, 1024, 8).unwrap();
    assert!(matches!(
        exports.attach_import_account(CUDA, 9, id, account.clone()),
        Err(hl_gpu::GpuError::Invalid(
            "account authority reused across sharing sessions"
        ))
    ));
    account.debug_release_reservation(id.0).unwrap();
    exports.release_import(CUDA, id).unwrap();
    let plan = exports.debug_prepare_owner_release(GL, id).unwrap();
    plan.commit();
    assert!(
        !exports.is_live(id),
        "rejection leaves no aliased lock graph or leaked reference"
    );
}

#[test]
fn owner_release_observes_no_importer_at_every_prepublication_stage() {
    for stage in 0..3 {
        let exports = Exports::new();
        let owner = Account::new();
        let importer = Account::new();
        let id = exports
            .debug_export_accounted(key(GL, 7), resource(1), 64, owner)
            .unwrap();
        let plan = exports
            .debug_prepare_import(CUDA, id, importer.clone())
            .unwrap();
        if stage >= 1 {
            importer.debug_reserve(id.0, 64, 1024, 8).unwrap();
        }
        if stage >= 2 {
            let _guard = plan.access();
            let _native = plan.resource();
        }
        assert!(
            matches!(
                exports.debug_prepare_owner_release(GL, id),
                Err(hl_gpu::GpuError::MappedElsewhere { .. })
            ),
            "stage {stage}: owner release must block, never observe an unaccounted importer"
        );
        drop(plan);
        if stage >= 1 {
            importer.debug_release_reservation(id.0).unwrap();
        }
        let owner_release = exports.debug_prepare_owner_release(GL, id).unwrap();
        owner_release.commit();
        assert!(
            !exports.is_live(id),
            "stage {stage}: cancelled import was never a payer/reference"
        );
    }
}

#[test]
fn cross_export_account_alias_is_rejected_before_publication() {
    let exports = Exports::new();
    let shared = Account::new();
    let first = exports
        .debug_export_accounted(key(GL, 7), resource(1), 64, shared.clone())
        .unwrap();
    let second = exports
        .debug_export_accounted(key(OTHER, 8), resource(2), 64, Account::new())
        .unwrap();
    assert!(exports.debug_prepare_import(CUDA, second, shared).is_err());
    let release = exports.debug_prepare_owner_release(GL, first).unwrap();
    release.commit();
    let release = exports.debug_prepare_owner_release(OTHER, second).unwrap();
    release.commit();
    assert!(!exports.is_live(first) && !exports.is_live(second));
}

#[test]
fn one_session_id_cannot_bind_two_distinct_account_authorities() {
    let exports = Exports::new();
    let first = exports
        .debug_export_accounted(key(GL, 7), resource(1), 64, Account::new())
        .unwrap();
    assert!(matches!(
        exports.debug_export_accounted(key(GL, 8), resource(2), 64, Account::new()),
        Err(hl_gpu::GpuError::Invalid(
            "session id reused with different account authority"
        ))
    ));
    let release = exports.debug_prepare_owner_release(GL, first).unwrap();
    release.commit();
    assert!(!exports.is_live(first));
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
    assert!(
        exports.import(CUDA, id).is_ok(),
        "the first import must work"
    );
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
