use super::{Capture, CaseResult, PhaseTiming, classify, combine, diagnostic, execute_phases, verify};
use crate::scenario::definition::ForkDiagnostics;
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
    assert!(matches!(classify(Ok(String::new()), true), CaseResult::UnexpectedPass));
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
        stdout_stream_regex: None,
        fork_diagnostics: None,
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
        stdout_stream_regex: None,
        fork_diagnostics: None,
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
fn fork_diagnostics_accept_zero_or_structured_records_and_reject_counterfeits() {
    const NATIVE: &str = "hl-native: crossings=1 translations=1 hits=0 fallbacks=0 sites=1 services=0\n";
    const VALID: &str = "hl-fork-failure: stage=host-fork result_errno=11 ambient_errno=0 syscall=220 flags=0x11 guest_pc=0x1 guest_sp=0x2 guest_tid=3 host_pid=4 host_ppid=1 route=local worker_pid=-1 sentry_pid=-1 guest_children=-1 worker_threads=-1 ring=-1 host_snapshot_status=1 host_threads=3 host_children=0 children_truncated=0 local_tasks=1 pids_total=5 pids_max=64 open_fds=6 nofile_cur=1024 nofile_max=1024 nofile_status=0 nproc_cur=512 nproc_max=512 nproc_status=0 mem_charged=4096 mem_max=8192 snapshot_stage=completed ofd_count=1 ofd_bytes=64 ofd_capacity=2 ofd_capacity_bytes=128 ofd_watermark=1 reserved_fds=2 watch_count=0 watch_bytes=0 watch_capacity=0 watch_capacity_bytes=0 fdvis_count=1 fdvis_bytes=16 watch_prepared=1 private_prepared=1 fdvis_prepared=1 seq_prepared=1\n";
    let directory = tempfile::tempdir().unwrap();
    let expression = directory.path().join("stdout.regex");
    std::fs::write(&expression, r"\AOK\n\z").unwrap();
    let case = Sample {
        id: "example/fork-diagnostics".into(),
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
        stdout_stream_regex: Some(expression),
        fork_diagnostics: Some(ForkDiagnostics { maximum_records: 2 }),
        output_empty: false,
    };
    assert!(verify(&case, ExitStatus::Code(0), b"OK\n", b"").is_err());
    assert_eq!(
        verify(&case, ExitStatus::Code(0), b"OK\n", NATIVE.as_bytes()).unwrap(),
        "native_records=1 fork_records=0 fork_retryable=0 fork_stages="
    );
    let observed = format!("{NATIVE}{VALID}");
    assert_eq!(
        verify(&case, ExitStatus::Code(0), b"OK\n", observed.as_bytes()).unwrap(),
        "native_records=1 fork_records=1 fork_retryable=1 fork_stages=host-fork:1"
    );
    assert!(verify(&case, ExitStatus::Code(0), b"prefix\nOK\n", observed.as_bytes()).is_err());
    assert!(verify(&case, ExitStatus::Code(0), b"OK\nsuffix\n", observed.as_bytes()).is_err());
    assert!(
        verify(
            &case,
            ExitStatus::Code(0),
            b"OK\n",
            format!("{NATIVE}{}", VALID.repeat(3)).as_bytes()
        )
        .is_err()
    );

    let mutations = [
        VALID.replace("stage=host-fork", "stage=imaginary"),
        VALID.replace("result_errno=11", "result_errno=eleven"),
        VALID.replace("syscall=220", "syscall=0x220"),
        VALID.replace("ambient_errno=0", "result_errno=0"),
        VALID.replace("ambient_errno=0", "bare-token"),
        VALID.replace(" snapshot_stage=completed", ""),
        VALID.replace(" seq_prepared=1", " unknown=1 seq_prepared=1"),
        VALID.replace("watch_prepared=1", "watch_prepared=2"),
    ];
    for malformed in mutations {
        assert!(
            verify(
                &case,
                ExitStatus::Code(0),
                b"OK\n",
                format!("{NATIVE}{malformed}").as_bytes()
            )
            .is_err(),
            "accepted malformed record: {malformed}"
        );
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
        stdout_stream_regex: None,
        fork_diagnostics: None,
        output_empty: true,
    };
    assert!(verify(&case, ExitStatus::Code(0), b"", b"").is_ok());
    assert!(verify(&case, ExitStatus::Code(0), b"unexpected", b"").is_err());
    assert!(verify(&case, ExitStatus::Code(0), b"", b"unexpected").is_err());
}

#[test]
fn cleanup_errors_are_not_lost_after_primary_failure() {
    let error = combine::<()>(Err("execution failed".into()), Err("cleanup failed".into())).unwrap_err();
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
