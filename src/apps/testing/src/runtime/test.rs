use super::{WorkKey, display_attempt, ledger, unattempted};
use crate::journal::Attempt;
use crate::suite::Target;

#[test]
fn an_abort_records_every_unreached_case_rather_than_dropping_it() {
    let key = |id: &str| WorkKey {
        id: id.to_owned(),
        target: Target::Arm64,
    };
    let keys = std::collections::BTreeSet::from([key("runtime/a"), key("runtime/b")]);
    let directory = tempfile::tempdir().unwrap();
    let opened = ledger::Ledger::open(&directory.path().join("results.tsv"), "stamp", &keys, false).unwrap();
    opened
        .ledger
        .record(ledger::Row {
            attempt: Attempt {
                key: key("runtime/a"),
                status: ledger::PASS,
                elapsed_ms: 1,
            },
            host_load: "0.10/8".to_owned(),
            diagnostic: String::new(),
        })
        .unwrap();
    let rows = unattempted(&opened.ledger, Some(&"row limit".into())).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempt.key.id, "runtime/b");
    assert_eq!(rows[0].attempt.status, ledger::NOT_RUN);
    assert!(rows[0].diagnostic.contains("row limit"), "{}", rows[0].diagnostic);
}

#[test]
fn attempt_display_does_not_mutate_the_case_identity() {
    let id = String::from("runtime/soak");
    assert_eq!(display_attempt(&id, None), "runtime/soak");
    assert_eq!(display_attempt(&id, Some(7)), "runtime/soak#attempt-7");
    assert_eq!(id, "runtime/soak");
}
