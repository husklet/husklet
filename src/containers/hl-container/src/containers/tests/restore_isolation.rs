use super::*;

#[tokio::test]
async fn restore_isolates_launch_failures_and_keeps_every_failed_process_retryable() {
    let repository = Arc::new(Memory::default());
    let mut initial_runtime = FakeRuntime::new(ExitStatus::Code(0));
    initial_runtime.delay = Duration::from_secs(60);
    let containers = test_containers(Arc::clone(&repository), Arc::new(initial_runtime))
        .await
        .unwrap();
    containers.create(spec("workspace")).await.unwrap();
    containers.start("workspace").await.unwrap();

    let programs = [
        "/missing/executable",
        "/blocked/by-missing-volume",
        "/blocked/by-unavailable-network",
        "/healthy/process",
    ];
    let mut executions = Vec::new();
    for program in programs {
        let execution = containers
            .executions()
            .create("workspace", ExecSpec::new(Process::new(program)))
            .await
            .unwrap();
        let _session = containers.executions().start(&execution.id).await.unwrap();
        executions.push(execution);
    }
    containers.checkpoint_all(Duration::from_secs(1)).await.unwrap();
    drop(containers);

    let mut recovery_runtime = FakeRuntime::new(ExitStatus::Code(0));
    recovery_runtime.delay = Duration::from_secs(60);
    let recovery_runtime = Arc::new(recovery_runtime);
    recovery_runtime.launch_failures.lock().unwrap().extend([
        ("/missing/executable".into(), "executable does not exist".into()),
        (
            "/blocked/by-missing-volume".into(),
            "required volume is unavailable".into(),
        ),
        (
            "/blocked/by-unavailable-network".into(),
            "required network is unavailable".into(),
        ),
    ]);
    let reopened = test_containers(repository, recovery_runtime.clone()).await.unwrap();
    reopened.start("workspace").await.unwrap();

    let failures = reopened.executions().restore_checkpoints().await.unwrap();
    assert_eq!(failures.len(), 3);
    for expected in [
        "executable does not exist",
        "required volume is unavailable",
        "required network is unavailable",
    ] {
        assert!(
            failures.iter().any(|(_, error)| error.to_string().contains(expected)),
            "missing reported recovery failure: {expected}"
        );
    }
    for (index, execution) in executions.iter().enumerate() {
        let recovered = reopened.executions().inspect(&execution.id).await.unwrap();
        if index == 3 {
            assert!(matches!(recovered.state, ExecState::Running { .. }));
            assert!(recovered.checkpoint.is_none());
        } else {
            assert_eq!(recovered.state, ExecState::Created);
            assert!(recovered.checkpoint.is_some());
        }
    }

    recovery_runtime.launch_failures.lock().unwrap().clear();
    assert!(reopened.executions().restore_checkpoints().await.unwrap().is_empty());
    for execution in executions {
        let recovered = reopened.executions().inspect(&execution.id).await.unwrap();
        assert!(matches!(recovered.state, ExecState::Running { .. }));
        assert!(recovered.checkpoint.is_none());
    }
}
