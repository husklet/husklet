use super::*;
#[test]
fn every_release_failpoint_preserves_the_exact_precommit_state() {
    // owner -> payer
    let exports = Exports::new();
    let global = hl_gpu::GlobalLedger::new(1024, 8);
    let owner = charged_account(&global, 7, 64);
    let importer = Account::new();
    let id = accounted_export(&exports, &global, &owner);
    attach_import(&exports, &importer, CUDA, 9, id);
    let plan = exports.debug_prepare_owner_release(GL, id).unwrap();
    assert_release_failpoint_preserves(
        &exports,
        id,
        ReleaseFailpoint::OwnerToPayer,
        || plan.commit(),
        &[&owner, &importer],
        &global,
    );

    // owner final refund
    let exports = Exports::new();
    let global = hl_gpu::GlobalLedger::new(1024, 8);
    let owner = charged_account(&global, 7, 64);
    let id = accounted_export(&exports, &global, &owner);
    let plan = exports.debug_prepare_owner_release(GL, id).unwrap();
    assert_release_failpoint_preserves(
        &exports,
        id,
        ReleaseFailpoint::OwnerFinalRefund,
        || plan.commit(),
        &[&owner],
        &global,
    );

    // Establish an owner-released export with two importers for all importer-release shapes.
    for (released, point) in [
        (CUDA, ReleaseFailpoint::PayerToNext),
        (OTHER, ReleaseFailpoint::NonPayerRelease),
    ] {
        let exports = Exports::new();
        let global = hl_gpu::GlobalLedger::new(1024, 8);
        let owner = charged_account(&global, 7, 64);
        let cuda = Account::new();
        let other = Account::new();
        let id = accounted_export(&exports, &global, &owner);
        attach_import(&exports, &cuda, CUDA, 9, id);
        attach_import(&exports, &other, OTHER, 10, id);
        exports
            .debug_prepare_owner_release(GL, id)
            .unwrap()
            .commit();
        let plan = exports
            .prepare_import_release_for_test(released, id)
            .unwrap();
        assert_release_failpoint_preserves(
            &exports,
            id,
            point,
            || plan.commit(),
            &[&owner, &cuda, &other],
            &global,
        );
    }

    // final payer refund
    let exports = Exports::new();
    let global = hl_gpu::GlobalLedger::new(1024, 8);
    let owner = charged_account(&global, 7, 64);
    let importer = Account::new();
    let id = accounted_export(&exports, &global, &owner);
    attach_import(&exports, &importer, CUDA, 9, id);
    exports
        .debug_prepare_owner_release(GL, id)
        .unwrap()
        .commit();
    let plan = exports.prepare_import_release_for_test(CUDA, id).unwrap();
    assert_release_failpoint_preserves(
        &exports,
        id,
        ReleaseFailpoint::FinalPayerRefund,
        || plan.commit(),
        &[&owner, &importer],
        &global,
    );
}
