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
use std::sync::mpsc;
use std::time::Duration;

use hl_gpu::runtime::model::resources::Account;
use hl_gpu::runtime::model::sharing::{
    ExportId, Exports, ReleaseFailpoint, ResourceKey, SessionId,
};

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

fn charged_account(global: &hl_gpu::GlobalLedger, local: u32, bytes: u64) -> Account {
    let account = Account::new();
    let mut ledger = hl_gpu::Ledger::default();
    ledger
        .live
        .insert((hl_gpu::runtime::KIND_BUFFER, local), bytes);
    ledger.totals = hl_gpu::runtime::Totals {
        bytes,
        objects: 1,
        compiled_bytes: 0,
    };
    account
        .debug_commit_ledger(Default::default(), ledger, 1024, 8, global)
        .unwrap();
    account
}

fn accounted_export(exports: &Exports, global: &hl_gpu::GlobalLedger, owner: &Account) -> ExportId {
    exports
        .debug_export_accounted_with_global(key(GL, 7), resource(0xAB), 64, owner.clone(), global)
        .unwrap()
}

fn attach_import(
    exports: &Exports,
    account: &Account,
    session: SessionId,
    local: u32,
    id: ExportId,
) {
    exports.import(session, id).unwrap();
    account.debug_reserve(id.0, 64, 1024, 8).unwrap();
    exports
        .attach_import_account(session, local, id, account.clone())
        .unwrap();
}

fn assert_release_failpoint_preserves(
    exports: &Exports,
    id: ExportId,
    point: ReleaseFailpoint,
    commit: impl FnOnce(),
    accounts: &[&Account],
    global: &hl_gpu::GlobalLedger,
) {
    let ledgers: Vec<_> = accounts.iter().map(|account| account.ledger()).collect();
    let reservations: Vec<_> = accounts
        .iter()
        .map(|account| account.reserved_bytes())
        .collect();
    let global_before = global.snapshot();
    let owner_access = exports.access(GL, id).is_ok();
    let cuda_access = exports.access(CUDA, id).is_ok();
    let other_access = exports.access(OTHER, id).is_ok();
    let mut owner_table = hl_gpu::SessionResources::new();
    let mut cuda_table = hl_gpu::SessionResources::new();
    if owner_access {
        owner_table
            .buffers
            .insert_guarded(7, Box::new(0xABu32), exports.access(GL, id).unwrap())
            .unwrap();
    }
    if cuda_access {
        cuda_table
            .buffers
            .insert_guarded(9, Box::new(0xABu32), exports.access(CUDA, id).unwrap())
            .unwrap();
    }
    exports.debug_fail_next_release(point);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(commit)).is_err());
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.ledger())
            .collect::<Vec<_>>(),
        ledgers
    );
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.reserved_bytes())
            .collect::<Vec<_>>(),
        reservations
    );
    assert_eq!(global.snapshot(), global_before);
    assert!(exports.is_live(id));
    assert_eq!(exports.access(GL, id).is_ok(), owner_access);
    assert_eq!(exports.access(CUDA, id).is_ok(), cuda_access);
    assert_eq!(exports.access(OTHER, id).is_ok(), other_access);
    assert_eq!(
        owner_table.buffers.get(7).is_ok(),
        owner_access,
        "the owner's pre-existing native-table guard changed"
    );
    assert_eq!(
        cuda_table.buffers.get(9).is_ok(),
        cuda_access,
        "the importer's pre-existing native-table guard changed"
    );
    if cuda_access {
        exports
            .map(CUDA, id)
            .expect("the failed plan must cancel its pending registry lease");
        exports.unmap(CUDA, id).unwrap();
    } else {
        assert!(exports.map(CUDA, id).is_err());
    }
    exports.settle_transition(id, Duration::ZERO);
}

fn exported(exports: &Exports, bytes: u64) -> ExportId {
    exports
        .export(key(GL, 7), resource(0xAB), bytes)
        .expect("the positive control must export; every refusal below is vacuous otherwise")
}

#[path = "sharing/failpoints.rs"]
mod failpoints;
#[path = "sharing/identity.rs"]
mod identity;
#[path = "sharing/lifecycle.rs"]
mod lifecycle;
