use super::{Capture, CaseResult, PhaseTiming, classify, combine, diagnostic, execute_phases, verify};
use crate::{
    scenario::definition::{Class, Sample, Step},
    suite::{Execution, Target},
};
use hl_container::ExitStatus;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[test]
fn expected_failures_remain_visible() {
    assert!(matches!(
        classify(Err("refused".into()), true),
        CaseResult::ExpectedFailure(reason) if reason == "refused"
    ));
    assert!(matches!(classify(Ok(()), true), CaseResult::UnexpectedPass));
}

#[test]
fn combined_capture_is_bounded() {
    assert!(crate::suite::Capture::bounded(Capture::LIMIT - 1, 1).is_ok());
    assert!(crate::suite::Capture::bounded(Capture::LIMIT, 1).is_err());
    assert!(crate::suite::Capture::bounded(usize::MAX, 1).is_err());
}

#[test]
fn durable_diagnostics_are_bounded() {
    assert_eq!(diagnostic(&"x".repeat(5000)).len(), 4096);
    assert_eq!(diagnostic("line\tone\nline two"), "line one line two");
    assert!(diagnostic(&"🙂".repeat(5000)).len() <= 4096);
}

#[test]
fn verification_combines_stdout_and_stderr() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("marker.txt");
    std::fs::write(&marker, b"from-stderr").unwrap();
    let case = Sample {
        id: "example/output".into(),
        image: "fixture".into(),
        execution: Execution::default(),
        class: Class::Quick,
        targets: vec![Target::Arm64],
        expected_failures: Vec::new(),
        resources: Vec::new(),
        environment: BTreeMap::new(),
        working_directory: "/".into(),
        actions: vec![Step::Shell("true".into())],
        fixtures: Vec::new(),
        readiness: None,
        timeout: 1,
        warmups: 0,
        repetitions: 1,
        exit: 0,
        stdout_contains: vec![marker],
        stdout_exact: None,
        stdout_regex: None,
        output_empty: false,
    };
    assert!(verify(&case, ExitStatus::Code(0), b"stdout", b"from-stderr").is_ok());
}

#[test]
fn regular_expression_output_rejects_prefixes_suffixes_and_malformed_timings() {
    let directory = tempfile::tempdir().unwrap();
    let expression = directory.path().join("output.regex");
    std::fs::write(
        &expression,
        r"\AAPT_STAGE_TIMING update_seconds=[0-9]+ install_seconds=[0-9]+\nHTOP_INSTALLED_AND_RUNNABLE\n\z",
    )
    .unwrap();
    let case = Sample {
        id: "example/regex".into(),
        image: "fixture".into(),
        execution: Execution::default(),
        class: Class::Quick,
        targets: vec![Target::Arm64],
        expected_failures: Vec::new(),
        resources: Vec::new(),
        environment: BTreeMap::new(),
        working_directory: "/".into(),
        actions: vec![Step::Shell("true".into())],
        fixtures: Vec::new(),
        readiness: None,
        timeout: 1,
        warmups: 0,
        repetitions: 1,
        exit: 0,
        stdout_contains: Vec::new(),
        stdout_exact: None,
        stdout_regex: Some(expression),
        output_empty: false,
    };
    let valid = b"APT_STAGE_TIMING update_seconds=4 install_seconds=2\nHTOP_INSTALLED_AND_RUNNABLE\n";
    assert!(verify(&case, ExitStatus::Code(0), valid, b"").is_ok());
    for invalid in [
        b"warning\nAPT_STAGE_TIMING update_seconds=4 install_seconds=2\nHTOP_INSTALLED_AND_RUNNABLE\n".as_slice(),
        b"APT_STAGE_TIMING update_seconds=many install_seconds=2\nHTOP_INSTALLED_AND_RUNNABLE\n".as_slice(),
        b"APT_STAGE_TIMING update_seconds=4 install_seconds=2\nHTOP_INSTALLED_AND_RUNNABLE\ntrailing\n".as_slice(),
    ] {
        assert!(verify(&case, ExitStatus::Code(0), invalid, b"").is_err());
    }
}

#[test]
fn empty_output_assertion_rejects_either_stream() {
    let case = Sample {
        id: "example/quiet".into(),
        image: "fixture".into(),
        execution: Execution::default(),
        class: Class::Quick,
        targets: vec![Target::Arm64],
        expected_failures: Vec::new(),
        resources: Vec::new(),
        environment: BTreeMap::new(),
        working_directory: "/".into(),
        actions: vec![Step::Shell("true".into())],
        fixtures: Vec::new(),
        readiness: None,
        timeout: 1,
        warmups: 0,
        repetitions: 1,
        exit: 0,
        stdout_contains: Vec::new(),
        stdout_exact: None,
        stdout_regex: None,
        output_empty: true,
    };
    assert!(verify(&case, ExitStatus::Code(0), b"", b"").is_ok());
    assert!(verify(&case, ExitStatus::Code(0), b"unexpected", b"").is_err());
    assert!(verify(&case, ExitStatus::Code(0), b"", b"unexpected").is_err());
}

#[test]
fn cleanup_errors_are_not_lost_after_primary_failure() {
    let error = combine(Err("execution failed".into()), Err("cleanup failed".into())).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("execution failed"));
    assert!(message.contains("cleanup failed"));
}

#[tokio::test]
async fn injected_provider_boundaries_are_ordered_and_isolated() {
    let mut timing = PhaseTiming::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let setup_events = Arc::clone(&events);
    let action_events = Arc::clone(&events);
    let cleanup_events = Arc::clone(&events);
    let (action, cleanup) = execute_phases(
        &mut timing,
        async move {
            tokio::time::sleep(Duration::from_millis(12)).await;
            setup_events.lock().await.push("create-start");
            Ok::<(), &'static str>(())
        },
        || async move {
            tokio::time::sleep(Duration::from_millis(24)).await;
            action_events.lock().await.push("guest-action");
            Ok::<_, String>(7)
        },
        async move {
            tokio::time::sleep(Duration::from_millis(36)).await;
            cleanup_events.lock().await.push("force-remove");
            Ok(())
        },
    )
    .await;
    assert_eq!(action.unwrap(), 7);
    assert!(cleanup.is_ok());
    assert_eq!(*events.lock().await, ["create-start", "guest-action", "force-remove"]);
    assert!(timing.setup_us >= 10_000);
    assert!(timing.execution_us >= 20_000);
    assert!(timing.teardown_us >= 30_000);
}
