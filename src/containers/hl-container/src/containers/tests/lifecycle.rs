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
async fn checkpoint_restore_replaces_stdin_authority_before_the_new_process_starts() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let mut process = Process::new("fake");
    process.console.stdin = true;
    containers
        .create(ContainerSpec::from_directory("/", process).name("generation-input"))
        .await
        .unwrap();

    let stale = containers.attach("generation-input").await.unwrap().input();
    containers.start("generation-input").await.unwrap();
    stale.write(b"before-checkpoint\n".to_vec()).await.unwrap();
    containers
        .checkpoint("generation-input", Duration::from_secs(1))
        .await
        .unwrap();
    assert!(
        stale.write(b"stale-after-checkpoint\n".to_vec()).await.is_err(),
        "checkpointed stdin retained write authority"
    );

    let restored = containers.attach("generation-input").await.unwrap().input();
    containers.start("generation-input").await.unwrap();
    restored.write(b"restored-generation\n".to_vec()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;

    let inputs = runtime.inputs.lock().unwrap();
    assert!(inputs.iter().any(|(_, bytes)| bytes == b"before-checkpoint\n"));
    assert!(inputs.iter().any(|(_, bytes)| bytes == b"restored-generation\n"));
    assert!(!inputs.iter().any(|(_, bytes)| bytes == b"stale-after-checkpoint\n"));
    drop(inputs);
    containers.remove_force("generation-input").await.unwrap();
}

#[tokio::test]
async fn container_restore_waits_for_old_output_generation_before_opening_the_new_one() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("container-output-generation")).await.unwrap();
    let mut old = containers.attach("container-output-generation").await.unwrap();
    let (old_waiting, release_old) = runtime.delay_next_log(b"container-old-initial\n", b"container-old-late\n");
    containers.start("container-output-generation").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), old_waiting)
        .await
        .expect("old container log owner did not enter wait")
        .unwrap();

    let exits = runtime.checkpoint_exits.load(Ordering::SeqCst);
    let checkpoint_containers = containers.clone();
    let checkpoint = tokio::spawn(async move {
        checkpoint_containers
            .checkpoint("container-output-generation", Duration::from_secs(1))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.checkpoint_exits.load(Ordering::SeqCst) == exits {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("container checkpoint never requested process exit");
    assert!(
        !checkpoint.is_finished(),
        "checkpoint escaped before the old output owner drained"
    );
    release_old.send(()).unwrap();
    checkpoint.await.unwrap().unwrap();

    let old_entries = [old.next().await.unwrap().unwrap(), old.next().await.unwrap().unwrap()];
    assert_eq!(old_entries[0].bytes, b"container-old-initial\n");
    assert_eq!(old_entries[1].bytes, b"container-old-late\n");
    assert!(old.next().await.unwrap().is_none());

    let mut restored = containers.attach("container-output-generation").await.unwrap();
    assert!(restored.history().await.unwrap().is_empty());
    containers.start("container-output-generation").await.unwrap();
    let new_entries = [
        restored.next().await.unwrap().unwrap(),
        restored.next().await.unwrap().unwrap(),
    ];
    assert_eq!(new_entries[0].bytes, b"fake-out\n");
    assert_eq!(new_entries[1].bytes, b"fake-err\n");
    assert!(
        new_entries
            .iter()
            .all(|entry| !entry.bytes.starts_with(b"container-old-"))
    );
    containers.remove_force("container-output-generation").await.unwrap();
}

#[tokio::test]
async fn checkpoint_all_rolls_back_a_captured_container_when_its_output_owner_panics() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    runtime.panic_wait.store(true, Ordering::SeqCst);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("capture-output-panic")).await.unwrap();
    containers.start("capture-output-panic").await.unwrap();
    let launches = runtime.programs.lock().unwrap().len();

    let error = containers.checkpoint_all(Duration::from_secs(1)).await.unwrap_err();
    assert!(error.to_string().contains("output owner exited"));
    let restored = containers.inspect("capture-output-panic").await.unwrap();
    assert!(matches!(restored.state, ContainerState::Running { .. }));
    assert!(restored.checkpoint.is_none());
    assert_eq!(runtime.programs.lock().unwrap().len(), launches + 1);
}

#[tokio::test]
async fn checkpoint_all_aborts_a_wedged_output_owner_before_rollback() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(10);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("wedged-output-owner")).await.unwrap();
    let (waiting, release) = runtime.delay_next_log(b"before-timeout\n", b"after-timeout\n");
    containers.start("wedged-output-owner").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("output owner did not reach the injected wedge")
        .unwrap();
    let launches = runtime.programs.lock().unwrap().len();

    let error = containers.checkpoint_all(Duration::from_millis(10)).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("timed out waiting for process output ownership")
    );
    let restored = containers.inspect("wedged-output-owner").await.unwrap();
    assert!(matches!(restored.state, ContainerState::Running { .. }));
    assert!(restored.checkpoint.is_none());
    assert_eq!(runtime.programs.lock().unwrap().len(), launches + 1);
    assert!(
        release.send(()).is_err(),
        "timed-out output owner was still alive after rollback"
    );
}

#[tokio::test]
async fn checkpoint_all_rolls_back_a_later_capture_after_an_earlier_failure() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("a-checkpoint-rejected")).await.unwrap();
    containers.create(spec("b-output-owner-panics")).await.unwrap();
    let order = containers.service.checkpoint_order().await.unwrap();
    assert_eq!(order.len(), 2);
    let rejected_process = runtime.next.load(Ordering::SeqCst);
    containers.start(order[0].as_str()).await.unwrap();
    runtime.panic_wait.store(true, Ordering::SeqCst);
    containers.start(order[1].as_str()).await.unwrap();
    runtime.panic_wait.store(false, Ordering::SeqCst);
    runtime.fail_checkpoint.store(rejected_process, Ordering::SeqCst);
    let launches = runtime.programs.lock().unwrap().len();

    let error = containers.checkpoint_all(Duration::from_secs(1)).await.unwrap_err();
    assert!(
        error.to_string().contains("injected checkpoint failure"),
        "unexpected primary checkpoint error: {error}"
    );
    for name in ["a-checkpoint-rejected", "b-output-owner-panics"] {
        let container = containers.inspect(name).await.unwrap();
        assert!(matches!(container.state, ContainerState::Running { .. }), "{name}");
        assert!(container.checkpoint.is_none(), "{name}");
    }
    assert_eq!(runtime.programs.lock().unwrap().len(), launches + 1);
}

#[tokio::test]
async fn discarded_checkpoint_preserves_container_and_forces_a_fresh_start() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let created = containers.create(spec("discard-checkpoint")).await.unwrap();
    containers.start("discard-checkpoint").await.unwrap();
    containers
        .checkpoint("discard-checkpoint", Duration::from_secs(1))
        .await
        .unwrap();

    let discarded = containers.discard_checkpoint("discard-checkpoint").await.unwrap();
    assert_eq!(discarded.id, created.id);
    assert_eq!(discarded.checkpoint, None);
    containers.start("discard-checkpoint").await.unwrap();
    assert_eq!(*runtime.checkpoints.lock().unwrap(), [Some(false), Some(false)]);

    containers.remove_force("discard-checkpoint").await.unwrap();
}

#[tokio::test]
async fn active_container_checkpoint_cannot_be_discarded() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    containers.create(spec("active-checkpoint")).await.unwrap();
    containers.start("active-checkpoint").await.unwrap();

    assert!(matches!(
        containers.discard_checkpoint("active-checkpoint").await,
        Err(Error::InvalidState { .. })
    ));

    containers.remove_force("active-checkpoint").await.unwrap();
}

#[tokio::test]
async fn failed_restore_retains_the_checkpoint_for_retry() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("restore-retry")).await.unwrap();
    containers.start("restore-retry").await.unwrap();
    let checkpoint = containers
        .checkpoint("restore-retry", Duration::from_secs(1))
        .await
        .unwrap();

    runtime.fail.store(true, Ordering::SeqCst);
    assert!(containers.start("restore-retry").await.is_err());
    let failed = containers.inspect("restore-retry").await.unwrap();
    assert!(matches!(failed.state, ContainerState::Exited { .. }));
    assert_eq!(failed.checkpoint.as_ref(), Some(&checkpoint));

    runtime.fail.store(false, Ordering::SeqCst);
    containers.start("restore-retry").await.unwrap();
    assert_eq!(containers.inspect("restore-retry").await.unwrap().checkpoint, None);
    containers.remove_force("restore-retry").await.unwrap();
}

#[tokio::test]
async fn failed_restore_terminates_its_reserved_session_and_retry_gets_a_fresh_one() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let mut process = Process::new("fake");
    process.console.stdin = true;
    containers
        .create(ContainerSpec::from_directory("/", process).name("restore-session-retry"))
        .await
        .unwrap();
    containers.start("restore-session-retry").await.unwrap();
    containers
        .checkpoint("restore-session-retry", Duration::from_secs(1))
        .await
        .unwrap();

    let failed_generation = containers.attach("restore-session-retry").await.unwrap().input();
    runtime.fail.store(true, Ordering::SeqCst);
    assert!(containers.start("restore-session-retry").await.is_err());
    assert!(failed_generation.write(b"orphaned\n".to_vec()).await.is_err());

    runtime.fail.store(false, Ordering::SeqCst);
    let retry = containers.attach("restore-session-retry").await.unwrap().input();
    containers.start("restore-session-retry").await.unwrap();
    retry.write(b"retry\n".to_vec()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
    let inputs = runtime.inputs.lock().unwrap();
    assert!(inputs.iter().any(|(_, bytes)| bytes == b"retry\n"));
    assert!(!inputs.iter().any(|(_, bytes)| bytes == b"orphaned\n"));
    drop(inputs);
    containers.remove_force("restore-session-retry").await.unwrap();
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
async fn checkpoint_all_holds_one_operation_guard_from_exec_preflight_through_capture() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("atomic-checkpoint-parent")).await.unwrap();
    containers.start("atomic-checkpoint-parent").await.unwrap();
    let exec = containers
        .executions()
        .create("atomic-checkpoint-parent", ExecSpec::new(Process::new("/bin/sh")))
        .await
        .unwrap();
    let launches = runtime.programs.lock().unwrap().len();
    let attempts = containers.service.exec_start_attempts();
    let (preflight, release) = containers.service.gate_checkpoint_all().await;

    let checkpoint_containers = containers.clone();
    let checkpoint = tokio::spawn(async move { checkpoint_containers.checkpoint_all(Duration::from_secs(1)).await });
    tokio::time::timeout(Duration::from_secs(1), preflight)
        .await
        .expect("checkpoint_all did not reach the post-preflight barrier")
        .unwrap();

    let start_containers = containers.clone();
    let start_id = exec.id.clone();
    let start = tokio::spawn(async move { start_containers.executions().start(&start_id).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while containers.service.exec_start_attempts() == attempts {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("concurrent exec start did not reach the operation guard");
    assert!(
        !start.is_finished(),
        "exec start crossed checkpoint_all's operation boundary"
    );
    assert_eq!(runtime.programs.lock().unwrap().len(), launches);

    release.send(()).unwrap();
    checkpoint.await.unwrap().unwrap();
    let error = start
        .await
        .unwrap()
        .err()
        .expect("concurrent exec start unexpectedly crossed the checkpoint boundary");
    assert!(matches!(error, Error::InvalidState { .. }));
    assert!(matches!(
        containers.inspect("atomic-checkpoint-parent").await.unwrap().state,
        ContainerState::Exited { .. }
    ));
    assert_eq!(runtime.programs.lock().unwrap().len(), launches);
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
    let mut session = containers.executions().start(&exec.id).await.unwrap();
    let delivered = session.next().await.unwrap().unwrap();
    assert_eq!(delivered.bytes, b"fake-out\n");
    session.acknowledge(delivered.sequence);
    let wait_id = exec.id.clone();
    let wait_containers = containers.clone();
    let mut wait = tokio::spawn(async move { wait_containers.executions().wait(&wait_id).await });

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
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut wait)
            .await
            .is_err(),
        "checkpointed execution wait returned before restore"
    );

    containers.shutdown(Duration::from_secs(1)).await.unwrap();
    containers.start("workspace").await.unwrap();
    let mut restored = containers.executions().start(&exec.id).await.unwrap();
    let mut resumed_output = restored.history().await.unwrap();
    while resumed_output.len() < 3 {
        resumed_output.push(
            tokio::time::timeout(Duration::from_millis(250), restored.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(
        resumed_output
            .iter()
            .map(|entry| entry.bytes.as_slice())
            .collect::<Vec<_>>(),
        [
            b"fake-err\n".as_slice(),
            b"fake-out\n".as_slice(),
            b"fake-err\n".as_slice()
        ],
        "delivered pre-checkpoint output is skipped while queued output and post-restore output are replayed"
    );
    for entry in &resumed_output {
        restored.acknowledge(entry.sequence);
    }
    drop(restored);
    let mut attached = containers.executions().attach(&exec.id, None).await.unwrap();
    assert!(attached.history().await.unwrap().is_empty());
    drop(attached);
    assert!(matches!(
        containers.executions().inspect(&exec.id).await.unwrap().state,
        ExecState::Running { .. }
    ));
    assert_eq!(
        runtime.checkpoints.lock().unwrap().as_slice(),
        [Some(false), Some(false), Some(false), Some(true)]
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .expect("restored execution wait timed out")
            .unwrap()
            .unwrap(),
        ExitStatus::Code(0)
    );
}

#[tokio::test]
async fn successful_exec_restore_replaces_stdin_authority() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("exec-generation-parent")).await.unwrap();
    containers.start("exec-generation-parent").await.unwrap();
    let exec = containers
        .executions()
        .create(
            "exec-generation-parent",
            ExecSpec::new(Process::new("/bin/sh")).streams(Streams {
                stdin: true,
                stdout: true,
                stderr: true,
            }),
        )
        .await
        .unwrap();
    let stale = containers.executions().start(&exec.id).await.unwrap();

    containers
        .executions()
        .checkpoint_all(Duration::from_secs(1))
        .await
        .unwrap();
    assert!(stale.write(b"stale-exec-generation\n".to_vec()).await.is_err());

    let restored = containers.executions().start(&exec.id).await.unwrap();
    restored.write(b"restored-exec-generation\n".to_vec()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
    let inputs = runtime.inputs.lock().unwrap();
    assert!(inputs.iter().any(|(_, bytes)| bytes == b"restored-exec-generation\n"));
    assert!(!inputs.iter().any(|(_, bytes)| bytes == b"stale-exec-generation\n"));
}

#[tokio::test]
async fn exhausted_exec_io_generation_fails_before_launch() {
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let containers = service(Arc::clone(&runtime)).await;
    containers
        .create(spec("exec-generation-exhausted-parent"))
        .await
        .unwrap();
    containers.start("exec-generation-exhausted-parent").await.unwrap();
    let exec = containers
        .executions()
        .create(
            "exec-generation-exhausted-parent",
            ExecSpec::new(Process::new("/bin/sh")),
        )
        .await
        .unwrap();
    let launches = runtime.programs.lock().unwrap().len();
    containers.service.exhaust_io_generations();

    let error = containers
        .executions()
        .start(&exec.id)
        .await
        .err()
        .expect("exhausted generation unexpectedly launched an exec");
    assert!(error.to_string().contains("I/O generation space is exhausted"));
    assert_eq!(runtime.programs.lock().unwrap().len(), launches);
}

#[tokio::test]
async fn exec_restore_fences_late_old_output_and_replays_it_only_as_durable_history() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(10);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("exec-output-parent")).await.unwrap();
    containers.start("exec-output-parent").await.unwrap();
    let exec = containers
        .executions()
        .create("exec-output-parent", ExecSpec::new(Process::new("/bin/sh")))
        .await
        .unwrap();
    let (old_waiting, release_old) = runtime.delay_next_log(b"exec-old-initial\n", b"exec-old-late\n");
    let mut old = containers.executions().start(&exec.id).await.unwrap();
    let old_input = old.input();
    tokio::time::timeout(Duration::from_secs(1), old_waiting)
        .await
        .expect("old exec log owner did not enter wait")
        .unwrap();

    let exits = runtime.checkpoint_exits.load(Ordering::SeqCst);
    let checkpoint_containers = containers.clone();
    let checkpoint = tokio::spawn(async move {
        checkpoint_containers
            .executions()
            .checkpoint_all(Duration::from_secs(1))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.checkpoint_exits.load(Ordering::SeqCst) == exits {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exec checkpoint never requested process exit");
    assert!(
        !checkpoint.is_finished(),
        "exec checkpoint escaped before the old output owner drained"
    );
    release_old.send(()).unwrap();
    checkpoint.await.unwrap().unwrap();

    let old_entries = [old.next().await.unwrap().unwrap(), old.next().await.unwrap().unwrap()];
    assert_eq!(old_entries[0].bytes, b"exec-old-initial\n");
    assert_eq!(old_entries[1].bytes, b"exec-old-late\n");
    assert!(old_input.write(b"stale\n".to_vec()).await.is_err());

    let mut restored = containers.executions().start(&exec.id).await.unwrap();
    let history = restored.history().await.unwrap();
    assert_eq!(
        history.iter().map(|entry| entry.bytes.as_slice()).collect::<Vec<_>>(),
        [b"exec-old-initial\n".as_slice(), b"exec-old-late\n".as_slice()],
        "unacknowledged old output must replay as history, never masquerade as live replacement output"
    );
    let new_entries = [
        restored.next().await.unwrap().unwrap(),
        restored.next().await.unwrap().unwrap(),
    ];
    assert_eq!(new_entries[0].bytes, b"fake-out\n");
    assert_eq!(new_entries[1].bytes, b"fake-err\n");
    assert!(new_entries.iter().all(|entry| !entry.bytes.starts_with(b"exec-old-")));
}

#[tokio::test]
async fn exec_checkpoint_rolls_back_a_capture_when_its_output_owner_panics() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("exec-capture-output-parent")).await.unwrap();
    containers.start("exec-capture-output-parent").await.unwrap();
    let exec = containers
        .executions()
        .create("exec-capture-output-parent", ExecSpec::new(Process::new("/bin/sh")))
        .await
        .unwrap();
    runtime.panic_wait.store(true, Ordering::SeqCst);
    containers.executions().start(&exec.id).await.unwrap();
    let launches = runtime.programs.lock().unwrap().len();

    let error = containers
        .executions()
        .checkpoint_all(Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("output owner exited"));
    let restored = containers.executions().inspect(&exec.id).await.unwrap();
    assert!(matches!(restored.state, ExecState::Running { .. }));
    assert!(restored.checkpoint.is_none());
    assert_eq!(runtime.programs.lock().unwrap().len(), launches + 1);
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
async fn container_checkpoint_failure_does_not_checkpoint_the_terminal_execution() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(31));
    runtime.delay = Duration::from_millis(50);
    runtime.fail_checkpoint.store(40, Ordering::SeqCst);
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

    assert!(error.to_string().contains("injected checkpoint failure"));
    assert!(matches!(
        containers.inspect("workspace").await.unwrap().state,
        ContainerState::Running { .. }
    ));
    let execution = containers.executions().inspect(&exec.id).await.unwrap();
    assert!(matches!(execution.state, ExecState::Running { .. }));
    assert_eq!(execution.checkpoint, None);
    assert_eq!(
        containers.executions().wait(&exec.id).await.unwrap(),
        ExitStatus::Code(31)
    );
}

#[tokio::test]
async fn checkpoint_all_restores_earlier_captures_after_a_later_failure() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    runtime.fail_checkpoint.store(41, Ordering::SeqCst);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("first")).await.unwrap();
    containers.create(spec("second")).await.unwrap();
    containers.start("first").await.unwrap();
    containers.start("second").await.unwrap();

    let error = containers.checkpoint_all(Duration::from_secs(1)).await.unwrap_err();

    assert!(error.to_string().contains("injected checkpoint failure"));
    for name in ["first", "second"] {
        let container = containers.inspect(name).await.unwrap();
        assert!(matches!(container.state, ContainerState::Running { .. }));
        assert_eq!(container.checkpoint, None);
    }
    assert_eq!(
        runtime.checkpoints.lock().unwrap().as_slice(),
        [Some(false), Some(false), Some(true)]
    );
}

#[tokio::test]
async fn checkpoint_execs_restores_an_earlier_terminal_after_a_later_failure() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(200);
    runtime.restore_delay = Some(Duration::from_secs(2));
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("workspace")).await.unwrap();
    containers.start("workspace").await.unwrap();
    let first = containers
        .executions()
        .create(
            "workspace",
            ExecSpec::new(Process::new("/bin/sh").console(Console::default().terminal(Size::new(24, 80).unwrap())))
                .streams(Streams {
                    stdin: true,
                    stdout: true,
                    stderr: true,
                }),
        )
        .await
        .unwrap();
    let first_session = containers.executions().start(&first.id).await.unwrap();
    let second = containers
        .executions()
        .create(
            "workspace",
            ExecSpec::new(Process::new("/bin/sh").console(Console::default().terminal(Size::new(24, 80).unwrap())))
                .streams(Streams {
                    stdin: true,
                    stdout: true,
                    stderr: true,
                }),
        )
        .await
        .unwrap();
    let second_session = containers.executions().start(&second.id).await.unwrap();

    // Stored executions are visited by id. Runtime process ids are allocated in launch order:
    // workspace=40, first=41, second=42. Fail the later stored terminal so the earlier one is
    // destructively captured before the injected refusal.
    let failing_runtime_id = if first.id.to_string() < second.id.to_string() {
        42
    } else {
        41
    };
    runtime.fail_checkpoint.store(failing_runtime_id, Ordering::SeqCst);

    let error = containers
        .executions()
        .checkpoint_all(Duration::from_secs(1))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("injected checkpoint failure"));
    for execution in [&first, &second] {
        let restored = containers.executions().inspect(&execution.id).await.unwrap();
        assert!(matches!(restored.state, ExecState::Running { .. }));
        assert_eq!(restored.checkpoint, None);
    }
    let surviving_session = if first.id.to_string() < second.id.to_string() {
        &first_session
    } else {
        &second_session
    };
    assert!(
        surviving_session
            .write(b"stale-after-rollback\n".to_vec())
            .await
            .is_err(),
        "the captured generation retained authority over its rollback replacement"
    );
    let surviving = if first.id.to_string() < second.id.to_string() {
        &first.id
    } else {
        &second.id
    };
    let replacement = containers.executions().attach(surviving, None).await.unwrap();
    replacement.write(b"after rollback\n".to_vec()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        runtime
            .inputs
            .lock()
            .unwrap()
            .iter()
            .any(|(process, bytes)| *process >= 43 && bytes == b"after rollback\n"),
        "the replacement session did not reach the restored process"
    );
    assert!(
        !runtime
            .inputs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, bytes)| bytes == b"stale-after-rollback\n")
    );
    assert!(
        matches!(
            containers.executions().inspect(surviving).await.unwrap().state,
            ExecState::Running { process_id, .. } if process_id >= 43
        ),
        "the captured process's stale completion clobbered its restored replacement"
    );
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
async fn failed_start_publication_retires_reserved_io_and_retry_allocates_fresh_authority() {
    let storage = Arc::new(Memory::default());
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(10);
    let runtime = Arc::new(runtime);
    let containers = test_containers(Arc::clone(&storage), runtime.clone()).await.unwrap();
    let mut process = Process::new("fake");
    process.console.stdin = true;
    containers
        .create(ContainerSpec::from_directory("/", process).name("publication-retry"))
        .await
        .unwrap();
    let stale = containers.attach("publication-retry").await.unwrap().input();
    storage.fail_next_container_replace();

    assert!(containers.start("publication-retry").await.is_err());
    assert!(stale.write(b"stale-publication\n".to_vec()).await.is_err());
    let retry = containers.attach("publication-retry").await.unwrap().input();
    containers.start("publication-retry").await.unwrap();
    retry.write(b"retry-publication\n".to_vec()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;

    let inputs = runtime.inputs.lock().unwrap();
    assert!(inputs.iter().any(|(_, bytes)| bytes == b"retry-publication\n"));
    assert!(!inputs.iter().any(|(_, bytes)| bytes == b"stale-publication\n"));
}

#[tokio::test]
async fn failed_start_publication_quarantines_retry_until_unpublished_process_is_reaped() {
    let storage = Arc::new(Memory::default());
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(75);
    let runtime = Arc::new(runtime);
    let containers = test_containers(Arc::clone(&storage), runtime.clone()).await.unwrap();
    containers.create(spec("publication-quarantine")).await.unwrap();
    storage.fail_next_container_replace();

    let failure = containers.start("publication-quarantine").await.unwrap_err();
    assert!(failure.to_string().contains("reap timed out"));
    let quarantined = containers.start("publication-quarantine").await.unwrap_err();
    assert!(quarantined.to_string().contains("quarantined"));

    tokio::time::sleep(Duration::from_millis(100)).await;
    containers.start("publication-quarantine").await.unwrap();
}

#[tokio::test]
async fn failed_unpublished_process_reap_permanently_poisons_retry() {
    let storage = Arc::new(Memory::default());
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(1);
    runtime.fail_wait.store(true, Ordering::SeqCst);
    let runtime = Arc::new(runtime);
    let containers = test_containers(Arc::clone(&storage), runtime).await.unwrap();
    containers.create(spec("publication-reap-failure")).await.unwrap();
    storage.fail_next_container_replace();

    let failure = containers.start("publication-reap-failure").await.unwrap_err();
    assert!(failure.to_string().contains("unpublished process reap failed"));
    let poisoned = containers.start("publication-reap-failure").await.unwrap_err();
    assert!(poisoned.to_string().contains("cleanup is poisoned"));
}

#[tokio::test]
async fn panicked_unpublished_reap_task_permanently_poisons_retry() {
    let storage = Arc::new(Memory::default());
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(1);
    runtime.panic_wait.store(true, Ordering::SeqCst);
    let runtime = Arc::new(runtime);
    let containers = test_containers(Arc::clone(&storage), runtime).await.unwrap();
    containers.create(spec("publication-reap-panic")).await.unwrap();
    storage.fail_next_container_replace();

    let failure = containers.start("publication-reap-panic").await.unwrap_err();
    assert!(failure.to_string().contains("unpublished reap task failed"));
    let poisoned = containers.start("publication-reap-panic").await.unwrap_err();
    assert!(poisoned.to_string().contains("cleanup is poisoned"));
}

#[tokio::test]
async fn exhausted_container_generation_fails_before_allocating_process_io() {
    let storage = Arc::new(Memory::default());
    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let containers = test_containers(Arc::clone(&storage), runtime.clone()).await.unwrap();
    let created = containers.create(spec("generation-exhausted")).await.unwrap();
    let mut exhausted = crate::storage::Containers::get(storage.as_ref(), &created.id)
        .await
        .unwrap()
        .unwrap();
    exhausted.generation = u64::MAX;
    crate::storage::Containers::replace(storage.as_ref(), &exhausted)
        .await
        .unwrap();

    assert!(containers.attach("generation-exhausted").await.is_err());
    assert!(containers.start("generation-exhausted").await.is_err());
    assert!(runtime.programs.lock().unwrap().is_empty());
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
