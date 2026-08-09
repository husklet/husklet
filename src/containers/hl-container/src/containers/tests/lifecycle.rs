use super::*;

#[tokio::test]
async fn runtime_wait_diagnostic_is_durable_and_shared_by_all_waiters() {
    let repository = Arc::new(Memory::default());
    let runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.fail_wait.store(true, Ordering::SeqCst);
    let containers = test_containers(repository.clone(), Arc::new(runtime)).await.unwrap();
    containers.create(spec("runtime-diagnostic")).await.unwrap();
    containers.start("runtime-diagnostic").await.unwrap();
    let left = {
        let containers = containers.clone();
        tokio::spawn(async move { containers.wait("runtime-diagnostic").await })
    };
    let right = {
        let containers = containers.clone();
        tokio::spawn(async move { containers.wait("runtime-diagnostic").await })
    };

    for result in [left.await.unwrap(), right.await.unwrap()] {
        assert!(matches!(result, Err(Error::Runtime(message)) if message == "runtime failed: injected wait failure"));
    }
    drop(containers);

    let recovered = test_containers(repository, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap();
    assert!(matches!(
        recovered.wait("runtime-diagnostic").await,
        Err(Error::Runtime(message)) if message == "runtime failed: injected wait failure"
    ));

    recovered.start("runtime-diagnostic").await.unwrap();
    assert_eq!(recovered.wait("runtime-diagnostic").await.unwrap(), ExitStatus::Code(0));
}

#[tokio::test]
async fn lifecycle_events_follow_committed_automatic_restart_order_once() {
    let containers = test_containers(
        Arc::new(Memory::default()),
        Arc::new(FakeRuntime::new(ExitStatus::Code(7))),
    )
    .await
    .unwrap();
    let events = Arc::new(Recorded::default());
    containers.observe(events.clone());
    containers
        .create(spec("event-restart").restart(RestartPolicy::OnFailure { maximum: Some(1) }))
        .await
        .unwrap();
    containers.start("event-restart").await.unwrap();
    assert_eq!(containers.wait("event-restart").await.unwrap(), ExitStatus::Code(7));
    assert_eq!(
        *events.0.lock().unwrap(),
        [
            LifecycleAction::Create,
            LifecycleAction::Start,
            LifecycleAction::Die,
            LifecycleAction::Restart,
            LifecycleAction::Start,
            LifecycleAction::Die,
        ]
    );
    let replay = Arc::new(Recorded::default());
    containers.observe(replay.clone());
    assert_eq!(*replay.0.lock().unwrap(), *events.0.lock().unwrap());
}

#[tokio::test]
async fn lifecycle_has_single_owner_and_supports_many_waiters() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(7)));
    let containers = service(runtime).await;
    let created = containers.create(spec("job")).await.unwrap();
    containers.start("job").await.unwrap();
    let running = containers.inspect("job").await.unwrap();
    assert!(matches!(running.state, ContainerState::Running { process_id: 40, .. }));
    assert!(matches!(
        containers.remove("job").await,
        Err(Error::InvalidState { .. })
    ));
    let left = {
        let containers = containers.clone();
        tokio::spawn(async move { containers.wait("job").await })
    };
    let right = {
        let containers = containers.clone();
        tokio::spawn(async move { containers.wait("job").await })
    };
    assert_eq!(left.await.unwrap().unwrap(), ExitStatus::Code(7));
    assert_eq!(right.await.unwrap().unwrap(), ExitStatus::Code(7));
    assert_eq!(containers.wait("job").await.unwrap(), ExitStatus::Code(7));
    assert_eq!(
        containers.logs("job").await.unwrap(),
        crate::Logs {
            stdout: b"fake-out\n".to_vec(),
            stderr: b"fake-err\n".to_vec()
        }
    );
    assert!(matches!(
        containers.inspect("job").await.unwrap().state,
        ContainerState::Exited {
            result: ExitStatus::Code(7),
            ..
        }
    ));
    assert_eq!(containers.remove("job").await.unwrap().id, created.id);
}

#[tokio::test]
async fn process_exit_does_not_wait_for_inherited_log_writers() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    runtime.hold_logs.store(true, Ordering::SeqCst);
    let containers = service(runtime).await;
    containers.create(spec("inherited-logs")).await.unwrap();
    containers.start("inherited-logs").await.unwrap();

    assert_eq!(
        tokio::time::timeout(Duration::from_millis(200), containers.wait("inherited-logs"))
            .await
            .expect("process exit must not depend on log-pipe EOF")
            .unwrap(),
        ExitStatus::Code(0)
    );
    assert_eq!(containers.logs("inherited-logs").await.unwrap().stdout, b"fake-out\n");
}

#[tokio::test]
async fn on_failure_restarts_exactly_to_limit_and_wait_spans_backoff() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(7)));
    let containers = service(Arc::clone(&runtime)).await;
    containers
        .create(spec("retrying").restart(RestartPolicy::OnFailure { maximum: Some(2) }))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    containers.start("retrying").await.unwrap();
    assert_eq!(containers.wait("retrying").await.unwrap(), ExitStatus::Code(7));
    assert!(started.elapsed() >= Duration::from_millis(300));
    let container = containers.inspect("retrying").await.unwrap();
    assert_eq!(container.generation, 3);
    assert_eq!(container.restart.count, 2);
    assert!(!container.restart.manually_stopped);
    assert_eq!(runtime.mounts.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn manual_signal_during_backoff_cancels_restart_and_wakes_waiters() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(1)));
    let containers = service(Arc::clone(&runtime)).await;
    containers
        .create(spec("cancel-restart").restart(RestartPolicy::Always))
        .await
        .unwrap();
    containers.start("cancel-restart").await.unwrap();
    loop {
        if matches!(
            containers.inspect("cancel-restart").await.unwrap().state,
            ContainerState::Restarting { .. }
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    containers.stop("cancel-restart", Duration::ZERO).await.unwrap();
    assert_eq!(containers.wait("cancel-restart").await.unwrap(), ExitStatus::Code(1));
    tokio::time::sleep(Duration::from_millis(125)).await;
    let container = containers.inspect("cancel-restart").await.unwrap();
    assert!(container.restart.manually_stopped);
    assert_eq!(container.restart.count, 0);
    assert_eq!(runtime.mounts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn raw_signal_while_running_does_not_suppress_automatic_restart() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(1));
    runtime.delay = Duration::from_millis(40);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers
        .create(spec("raw-signal").restart(RestartPolicy::OnFailure { maximum: Some(1) }))
        .await
        .unwrap();
    containers.start("raw-signal").await.unwrap();
    containers.signal("raw-signal", Signal::USER1).await.unwrap();
    assert_eq!(containers.wait("raw-signal").await.unwrap(), ExitStatus::Code(1));
    let container = containers.inspect("raw-signal").await.unwrap();
    assert!(!container.restart.manually_stopped);
    assert_eq!(container.generation, 2);
    assert_eq!(*runtime.signals.lock().unwrap(), [Signal::USER1]);
    assert_eq!(runtime.mounts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn pause_and_unpause_are_persisted_runtime_transitions() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let events = Arc::new(Recorded::default());
    containers.observe(events.clone());
    containers.create(spec("pausable")).await.unwrap();
    containers.start("pausable").await.unwrap();

    containers.pause("pausable").await.unwrap();
    assert!(containers.inspect("pausable").await.unwrap().state.is_paused());
    assert!(matches!(
        containers.pause("pausable").await,
        Err(Error::InvalidState { .. })
    ));
    assert_eq!(*runtime.suspensions.lock().unwrap(), vec![true]);
    containers.unpause("pausable").await.unwrap();
    assert!(matches!(
        containers.inspect("pausable").await.unwrap().state,
        ContainerState::Running { .. }
    ));
    assert!(matches!(
        containers.unpause("pausable").await,
        Err(Error::InvalidState { .. })
    ));
    assert_eq!(*runtime.suspensions.lock().unwrap(), vec![true, false]);
    assert_eq!(
        *events.0.lock().unwrap(),
        [
            LifecycleAction::Create,
            LifecycleAction::Start,
            LifecycleAction::Pause,
            LifecycleAction::Unpause,
        ]
    );

    containers.remove_force("pausable").await.unwrap();
}

/// Docker 29.1.3 resumes a paused container before delivering *any* signal, so `State.Paused`
/// is false afterwards. Measured for USR1, CONT, TERM, KILL, STOP and WINCH on this host.
#[tokio::test]
async fn signalling_a_paused_container_resumes_it_first_like_docker() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let events = Arc::new(Recorded::default());
    containers.observe(events.clone());
    containers.create(spec("signalled")).await.unwrap();
    containers.start("signalled").await.unwrap();

    // A running container is signalled directly: no suspension transition may be fabricated.
    containers.signal("signalled", Signal::USER1).await.unwrap();
    assert!(runtime.suspensions.lock().unwrap().is_empty());

    containers.pause("signalled").await.unwrap();
    let continue_signal = Signal::new(18).unwrap();
    containers.signal("signalled", continue_signal).await.unwrap();
    assert!(matches!(
        containers.inspect("signalled").await.unwrap().state,
        ContainerState::Running { .. }
    ));
    assert_eq!(*runtime.suspensions.lock().unwrap(), vec![true, false]);
    assert_eq!(*runtime.signals.lock().unwrap(), vec![Signal::USER1, continue_signal]);
    assert_eq!(
        *events.0.lock().unwrap(),
        [
            LifecycleAction::Create,
            LifecycleAction::Start,
            LifecycleAction::Pause,
            LifecycleAction::Unpause,
        ]
    );

    containers.remove_force("signalled").await.unwrap();
}

/// A guest stopped by SIGSTOP is still `running` to the daemon, exactly as Docker reports it,
/// and `stop` must still reach the KILL fallback rather than hanging on the graceful signal.
#[tokio::test]
async fn a_stop_signalled_container_stays_running_and_still_tears_down() {
    // Long enough that the graceful signal cannot be what ends the guest, short enough that it
    // stays clear of `FORCE_STOP_TIMEOUT`, which this raced exactly when both were 30 seconds.
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("stop-signalled")).await.unwrap();
    containers.start("stop-signalled").await.unwrap();

    let stop_signal = Signal::new(19).unwrap();
    containers.signal("stop-signalled", stop_signal).await.unwrap();
    let container = containers.inspect("stop-signalled").await.unwrap();
    assert!(matches!(container.state, ContainerState::Running { .. }));
    assert!(!container.state.is_paused());
    assert!(runtime.suspensions.lock().unwrap().is_empty());

    containers.remove_force("stop-signalled").await.unwrap();
    assert_eq!(
        *runtime.signals.lock().unwrap(),
        vec![stop_signal, Signal::KILL],
        "teardown must escalate to KILL for a guest that is stopped rather than exiting"
    );
}

#[tokio::test]
async fn checkpoint_is_durable_and_start_restores_while_arming_the_next_capture() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("checkpointed")).await.unwrap();
    containers.start("checkpointed").await.unwrap();

    let checkpoint = containers
        .checkpoint("checkpointed", Duration::from_secs(1))
        .await
        .unwrap();
    let stored = containers.inspect("checkpointed").await.unwrap();
    assert!(matches!(stored.state, ContainerState::Exited { .. }));
    assert_eq!(stored.checkpoint.as_ref(), Some(&checkpoint));

    containers.start("checkpointed").await.unwrap();
    {
        let launches = runtime.checkpoints.lock().unwrap();
        assert_eq!(launches.len(), 2);
        assert_eq!(launches[0], Some(false));
        assert_eq!(launches[1], Some(true));
    }
    let restored = containers.inspect("checkpointed").await.unwrap();
    assert!(matches!(restored.state, ContainerState::Running { .. }));
    assert_eq!(restored.checkpoint, None);

    containers.remove_force("checkpointed").await.unwrap();
}

#[tokio::test]
async fn checkpoint_all_captures_running_and_paused_containers_for_later_restore() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("running")).await.unwrap();
    containers.create(spec("paused")).await.unwrap();
    containers.start("running").await.unwrap();
    containers.start("paused").await.unwrap();
    containers.pause("paused").await.unwrap();

    containers.checkpoint_all(Duration::from_secs(1)).await.unwrap();

    for name in ["running", "paused"] {
        let container = containers.inspect(name).await.unwrap();
        assert!(matches!(container.state, ContainerState::Exited { .. }));
        assert_eq!(
            container.checkpoint.as_ref().map(|value| value.namespace.as_str()),
            Some(container.id.as_str())
        );
    }
    assert_eq!(*runtime.suspensions.lock().unwrap(), vec![true, false]);
}

#[tokio::test]
async fn execution_checkpoint_restores_without_capturing_the_container_init() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("workspace")).await.unwrap();
    containers.start("workspace").await.unwrap();
    let exec = containers
        .executions()
        .create(
            "workspace",
            ExecSpec::new(Process::new("/bin/sh").console(Console::default().terminal(Size::new(24, 80).unwrap()))),
        )
        .await
        .unwrap();
    let _session = containers.executions().start(&exec.id).await.unwrap();

    containers
        .executions()
        .checkpoint_all(Duration::from_secs(1))
        .await
        .unwrap();
    assert!(matches!(
        containers.inspect("workspace").await.unwrap().state,
        ContainerState::Running { .. }
    ));
    let checkpointed = containers.executions().inspect(&exec.id).await.unwrap();
    assert_eq!(checkpointed.state, ExecState::Created);
    assert_eq!(
        checkpointed
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.namespace.as_str()),
        Some(format!("exec-{}", exec.id).as_str())
    );

    containers.shutdown(Duration::from_secs(1)).await.unwrap();
    containers.start("workspace").await.unwrap();
    let _restored = containers.executions().start(&exec.id).await.unwrap();
    assert!(matches!(
        containers.executions().inspect(&exec.id).await.unwrap().state,
        ExecState::Running { .. }
    ));
    assert_eq!(
        runtime.checkpoints.lock().unwrap().as_slice(),
        [Some(false), Some(false), Some(false), Some(true)]
    );
}

#[tokio::test]
async fn checkpoint_rejection_preserves_every_running_process() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    runtime.checkpointable.store(false, Ordering::SeqCst);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("workspace")).await.unwrap();
    containers.start("workspace").await.unwrap();
    let exec = containers
        .executions()
        .create(
            "workspace",
            ExecSpec::new(Process::new("/bin/sh").console(Console::default().terminal(Size::new(24, 80).unwrap()))),
        )
        .await
        .unwrap();
    let _session = containers.executions().start(&exec.id).await.unwrap();

    let error = containers.checkpoint_all(Duration::from_secs(1)).await.unwrap_err();

    assert!(error.to_string().contains("not checkpointable"));
    let container = containers.inspect("workspace").await.unwrap();
    assert!(matches!(container.state, ContainerState::Running { .. }));
    assert_eq!(container.checkpoint, None);
    let execution = containers.executions().inspect(&exec.id).await.unwrap();
    assert!(matches!(execution.state, ExecState::Running { .. }));
    assert_eq!(execution.checkpoint, None);
    assert!(runtime.suspensions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_launch_does_not_publish_running_state() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    runtime.fail.store(true, Ordering::SeqCst);
    let containers = service(runtime).await;
    containers.create(spec("bad")).await.unwrap();
    assert!(matches!(containers.start("bad").await, Err(Error::Runtime(_))));
    assert_eq!(containers.inspect("bad").await.unwrap().state, ContainerState::Created);
}

#[tokio::test]
async fn rename_wait_removed_stop_and_force_remove_follow_owned_lifecycle() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("old")).await.unwrap();
    assert_eq!(
        containers.rename("old", "new").await.unwrap().spec.name.as_deref(),
        Some("new")
    );
    containers.create(spec("occupied")).await.unwrap();
    assert!(matches!(
        containers.rename("new", "occupied").await,
        Err(Error::NameConflict(_))
    ));

    containers.start("new").await.unwrap();
    let exit = containers.stop("new", Duration::ZERO).await.unwrap();
    assert_eq!(exit, ExitStatus::Code(0));
    assert_eq!(*runtime.signals.lock().unwrap(), vec![Signal::TERMINATE, Signal::KILL]);

    let removed = {
        let containers = containers.clone();
        tokio::spawn(async move { containers.wait_for("new", WaitCondition::Removed).await })
    };
    tokio::task::yield_now().await;
    containers.remove("new").await.unwrap();
    assert_eq!(removed.await.unwrap().unwrap(), Some(ExitStatus::Code(0)));

    containers.start("occupied").await.unwrap();
    containers.remove_force("occupied").await.unwrap();
    assert!(matches!(containers.inspect("occupied").await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn wait_removed_rejects_a_container_that_was_never_observed() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;

    assert!(matches!(
        containers.wait_for("missing", WaitCondition::Removed).await,
        Err(Error::NotFound(reference)) if reference == "missing"
    ));
}

#[tokio::test]
async fn wait_removed_completes_without_status_after_observed_created_container_is_removed() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    containers.create(spec("created")).await.unwrap();
    let removed = {
        let containers = containers.clone();
        tokio::spawn(async move { containers.wait_for("created", WaitCondition::Removed).await })
    };
    tokio::task::yield_now().await;

    containers.remove("created").await.unwrap();

    assert_eq!(removed.await.unwrap().unwrap(), None);
}

#[tokio::test]
async fn wait_removed_stays_pinned_when_the_observed_name_is_reused() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    let original = containers.create(spec("reused")).await.unwrap();
    let removed = {
        let containers = containers.clone();
        tokio::spawn(async move { containers.wait_for("reused", WaitCondition::Removed).await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while !containers.service.has_waiter(&original.id).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("waiter registration timed out");

    let replacement = tokio::task::unconstrained(async {
        containers.remove("reused").await.unwrap();
        containers.create(spec("reused")).await.unwrap()
    })
    .await;

    assert_ne!(replacement.id, original.id);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), removed)
            .await
            .expect("wait remained attached to the replacement")
            .unwrap()
            .unwrap(),
        None
    );
    assert_eq!(containers.inspect("reused").await.unwrap().id, replacement.id);
}

#[tokio::test]
async fn next_exit_observes_a_restart_policy_exit_without_waiting_for_final_stop() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(7));
    runtime.delay = Duration::from_millis(20);
    let containers = service(Arc::new(runtime)).await;
    containers
        .create(spec("next-restart").restart(RestartPolicy::OnFailure { maximum: Some(1) }))
        .await
        .unwrap();
    containers.start("next-restart").await.unwrap();

    assert_eq!(
        containers
            .wait_for("next-restart", WaitCondition::NextExit)
            .await
            .unwrap(),
        Some(ExitStatus::Code(7))
    );
    assert_eq!(containers.wait("next-restart").await.unwrap(), ExitStatus::Code(7));
    assert_eq!(containers.inspect("next-restart").await.unwrap().restart.count, 1);
}

#[tokio::test]
async fn next_exit_on_an_exited_container_waits_for_a_later_generation() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(5));
    runtime.delay = Duration::from_millis(20);
    let containers = service(Arc::new(runtime)).await;
    containers.create(spec("next-generation")).await.unwrap();
    containers.start("next-generation").await.unwrap();
    assert_eq!(containers.wait("next-generation").await.unwrap(), ExitStatus::Code(5));

    let waiting = {
        let containers = containers.clone();
        tokio::spawn(async move { containers.wait_for("next-generation", WaitCondition::NextExit).await })
    };
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    containers.start("next-generation").await.unwrap();

    assert_eq!(waiting.await.unwrap().unwrap(), Some(ExitStatus::Code(5)));
}

#[tokio::test(start_paused = true)]
async fn shutdown_stops_every_active_container_and_preserves_inactive_records() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    for name in ["first", "second", "inactive"] {
        containers.create(spec(name)).await.unwrap();
    }
    containers.start("first").await.unwrap();
    containers.start("second").await.unwrap();

    containers.shutdown(Duration::ZERO).await.unwrap();

    for name in ["first", "second"] {
        assert!(matches!(
            containers.inspect(name).await.unwrap().state,
            ContainerState::Exited { .. }
        ));
    }
    assert_eq!(
        containers.inspect("inactive").await.unwrap().state,
        ContainerState::Created
    );
    assert_eq!(*runtime.signals.lock().unwrap(), [Signal::TERMINATE, Signal::KILL]);
}

#[tokio::test(start_paused = true)]
async fn force_removal_reaps_attached_executions_before_returning() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(80);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("force-tree")).await.unwrap();
    containers.start("force-tree").await.unwrap();
    let execution = containers
        .executions()
        .create("force-tree", ExecSpec::new(Process::new("fake")))
        .await
        .unwrap();
    let _session = containers.executions().start(&execution.id).await.unwrap();

    tokio::time::timeout(Duration::from_secs(1), containers.remove_force("force-tree"))
        .await
        .expect("force removal must remain bounded")
        .unwrap();

    assert!(containers.executions().list().await.unwrap().is_empty());
    assert!(matches!(
        containers.executions().inspect(&execution.id).await,
        Err(Error::ExecNotFound(_))
    ));
    assert!(runtime.signals.lock().unwrap().contains(&Signal::KILL));
}

/// A guest that records the stop signal and keeps running is exactly the teardown hang this
/// bound exists for: without it `remove_force` waits on `WaitCondition::NotRunning` forever.
#[tokio::test(start_paused = true)]
async fn force_removal_of_a_guest_that_ignores_the_stop_signal_is_bounded() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(3_600);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("refuses")).await.unwrap();
    containers.start("refuses").await.unwrap();

    let error = containers.remove_force("refuses").await.unwrap_err();

    // Naming the condition is the point: a widened tolerance that reported any failure would
    // hide the difference between a refusing guest and an ordinary removal error.
    assert!(
        matches!(&error, Error::StopTimeout { seconds, .. } if *seconds == 30),
        "{error:?}"
    );
    assert!(runtime.signals.lock().unwrap().contains(&Signal::KILL));
}
