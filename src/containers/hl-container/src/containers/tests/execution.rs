use super::*;

#[tokio::test]
async fn attachments_read_the_same_durable_order_without_stealing_logs() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    containers.create(spec("attached")).await.unwrap();
    let mut fast = containers.attach("attached").await.unwrap();
    let mut slow = containers.attach("attached").await.unwrap();
    containers.start("attached").await.unwrap();

    let fast_entries = vec![fast.next().await.unwrap().unwrap(), fast.next().await.unwrap().unwrap()];
    containers.wait("attached").await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
    let slow_entries = vec![slow.next().await.unwrap().unwrap(), slow.next().await.unwrap().unwrap()];
    assert_eq!(fast_entries, slow_entries);
    assert_eq!(fast_entries[0].sequence, 1);
    assert!(fast_entries[0].timestamp_ms > 0);
    assert_eq!(fast_entries[0].stream, crate::Stream::Stdout);
    assert_eq!(fast_entries[0].bytes, b"fake-out\n");
    assert_eq!(fast_entries[1].sequence, 2);
    assert_eq!(fast_entries[1].stream, crate::Stream::Stderr);
    assert_eq!(fast_entries[1].bytes, b"fake-err\n");
    assert!(fast.next().await.unwrap().is_none());
    assert!(slow.next().await.unwrap().is_none());
    assert_eq!(
        containers.logs("attached").await.unwrap(),
        crate::Logs {
            stdout: b"fake-out\n".to_vec(),
            stderr: b"fake-err\n".to_vec(),
        }
    );
    let mut replay = containers.follow("attached").await.unwrap();
    assert_eq!(replay.history().await.unwrap(), fast_entries);
    assert!(replay.history().await.unwrap().is_empty());
    assert!(replay.next().await.unwrap().is_none());
}

#[tokio::test]
async fn open_stdin_is_recreated_for_each_process_start() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    let mut process = Process::new("fake");
    process.console.stdin = true;
    containers
        .create(ContainerSpec::from_directory("/", process).name("restart-input"))
        .await
        .unwrap();

    containers.start("restart-input").await.unwrap();
    containers.wait("restart-input").await.unwrap();
    containers.start("restart-input").await.unwrap();
    containers.wait("restart-input").await.unwrap();
}

#[tokio::test]
async fn terminal_resize_reaches_runtime_and_updates_durable_size() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let initial = Size::new(25, 81).unwrap();
    let resized = Size::new(40, 120).unwrap();
    let process = Process::new("fake").console(Console::default().terminal(initial));
    containers
        .create(ContainerSpec::from_directory("/", process).name("terminal"))
        .await
        .unwrap();

    containers.start("terminal").await.unwrap();
    containers.resize("terminal", resized).await.unwrap();

    assert_eq!(*runtime.terminals.lock().unwrap(), vec![Some(initial)]);
    assert_eq!(*runtime.resizes.lock().unwrap(), vec![resized]);
    assert_eq!(
        containers
            .inspect("terminal")
            .await
            .unwrap()
            .spec
            .process
            .console
            .terminal,
        Some(resized)
    );
}

#[tokio::test]
async fn resize_rejects_non_terminal_and_stopped_processes() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let containers = service(Arc::new(runtime)).await;
    containers.create(spec("plain")).await.unwrap();
    let size = Size::new(30, 90).unwrap();

    assert!(matches!(
        containers.resize("plain", size).await,
        Err(Error::InvalidState { .. })
    ));
    containers.start("plain").await.unwrap();
    assert!(matches!(
        containers.resize("plain", size).await,
        Err(Error::NoTerminal(_))
    ));
    assert!(matches!(
        containers.resize("missing", size).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn automatic_restart_allocates_terminal_at_latest_size() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(7));
    runtime.delay = Duration::from_millis(40);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let initial = Size::new(24, 80).unwrap();
    let resized = Size::new(50, 132).unwrap();
    let process = Process::new("fake").console(Console::default().terminal(initial));
    containers
        .create(
            ContainerSpec::from_directory("/", process)
                .name("terminal-restart")
                .restart(RestartPolicy::OnFailure { maximum: Some(1) }),
        )
        .await
        .unwrap();

    containers.start("terminal-restart").await.unwrap();
    containers.resize("terminal-restart", resized).await.unwrap();
    containers.wait("terminal-restart").await.unwrap();

    assert_eq!(*runtime.terminals.lock().unwrap(), vec![Some(initial), Some(resized)]);
}

#[tokio::test]
async fn execution_terminal_resize_is_independent_and_durable() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("exec-terminal-parent")).await.unwrap();
    containers.start("exec-terminal-parent").await.unwrap();
    let initial = Size::new(27, 91).unwrap();
    let resized = Size::new(44, 144).unwrap();
    let process = Process::new("fake").console(Console::default().terminal(initial));
    let execution = containers
        .executions()
        .create("exec-terminal-parent", ExecSpec::new(process))
        .await
        .unwrap();

    let _session = containers.executions().start(&execution.id).await.unwrap();
    containers.executions().resize(&execution.id, resized).await.unwrap();

    assert_eq!(*runtime.terminals.lock().unwrap(), vec![None, Some(initial)]);
    assert_eq!(*runtime.resizes.lock().unwrap(), vec![resized]);
    assert_eq!(
        containers
            .executions()
            .inspect(&execution.id)
            .await
            .unwrap()
            .spec
            .process
            .console
            .terminal,
        Some(resized)
    );
}

#[tokio::test]
async fn execution_signal_targets_only_the_exec_process() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("exec-signal-parent")).await.unwrap();
    containers.start("exec-signal-parent").await.unwrap();
    let execution = containers
        .executions()
        .create("exec-signal-parent", ExecSpec::new(Process::new("fake")))
        .await
        .unwrap();
    let _session = containers.executions().start(&execution.id).await.unwrap();

    containers
        .executions()
        .signal(&execution.id, Signal::HANGUP)
        .await
        .unwrap();

    assert_eq!(*runtime.signals.lock().unwrap(), vec![Signal::HANGUP]);
    assert!(
        containers
            .inspect("exec-signal-parent")
            .await
            .unwrap()
            .state
            .is_active()
    );
}

#[tokio::test]
async fn execution_joins_the_container_domain_without_owning_it() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("exec-domain-parent")).await.unwrap();
    containers.start("exec-domain-parent").await.unwrap();
    let execution = containers
        .executions()
        .create("exec-domain-parent", ExecSpec::new(Process::new("fake")))
        .await
        .unwrap();

    let _session = containers.executions().start(&execution.id).await.unwrap();

    let domains = runtime.domains.lock().unwrap();
    assert_eq!(domains.len(), 2);
    assert_eq!(domains[0].0, domains[1].0);
    assert!(domains[0].1);
    assert!(!domains[1].1);
    drop(domains);
    assert_eq!(runtime.domain_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn killing_an_execution_force_stops_it_without_stopping_the_container() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    containers.create(spec("exec-kill-parent")).await.unwrap();
    containers.start("exec-kill-parent").await.unwrap();
    let execution = containers
        .executions()
        .create("exec-kill-parent", ExecSpec::new(Process::new("fake")))
        .await
        .unwrap();
    let _session = containers.executions().start(&execution.id).await.unwrap();

    containers
        .executions()
        .signal(&execution.id, Signal::KILL)
        .await
        .unwrap();

    assert_eq!(*runtime.signals.lock().unwrap(), [Signal::KILL]);
    assert!(containers.inspect("exec-kill-parent").await.unwrap().state.is_active());
    let domains = runtime.domains.lock().unwrap();
    assert_eq!(domains.len(), 2);
    assert_eq!(domains[0].0, domains[1].0);
    assert!(domains[0].1);
    assert!(!domains[1].1);
    drop(domains);
    assert_eq!(runtime.domain_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn completed_execution_can_be_removed_but_running_execution_cannot() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(30);
    let containers = service(Arc::new(runtime)).await;
    containers.create(spec("exec-remove-parent")).await.unwrap();
    containers.start("exec-remove-parent").await.unwrap();
    let execution = containers
        .executions()
        .create("exec-remove-parent", ExecSpec::new(Process::new("fake")))
        .await
        .unwrap();
    assert_eq!(containers.executions().list().await.unwrap(), vec![execution.clone()]);
    let mut session = containers.executions().start(&execution.id).await.unwrap();

    assert!(matches!(
        containers.executions().remove(&execution.id).await,
        Err(Error::InvalidExecState { .. })
    ));
    while session.next().await.unwrap().is_some() {}
    containers.executions().remove(&execution.id).await.unwrap();
    assert!(containers.executions().list().await.unwrap().is_empty());
    assert!(matches!(
        containers.executions().inspect(&execution.id).await,
        Err(Error::ExecNotFound(_))
    ));
}

#[tokio::test]
async fn executions_are_single_use_and_keep_independent_output() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(19));
    runtime.delay = Duration::from_millis(100);
    let containers = service(Arc::new(runtime)).await;
    containers.create(spec("exec-parent")).await.unwrap();
    assert!(matches!(
        containers
            .executions()
            .create("exec-parent", ExecSpec::new(Process::new("/bin/false")))
            .await,
        Err(Error::InvalidState { .. })
    ));
    containers.start("exec-parent").await.unwrap();

    let exec = containers
        .executions()
        .create("exec-parent", ExecSpec::new(Process::new("/bin/echo").args(["exec"])))
        .await
        .unwrap();
    let mut session = containers.executions().start(&exec.id).await.unwrap();
    assert!(matches!(
        containers.executions().start(&exec.id).await,
        Err(Error::InvalidExecState { .. })
    ));
    assert_eq!(session.next().await.unwrap().unwrap().bytes, b"fake-out\n");
    assert_eq!(session.next().await.unwrap().unwrap().bytes, b"fake-err\n");
    assert!(session.next().await.unwrap().is_none());
    let finished = containers.executions().inspect(&exec.id).await.unwrap();
    assert!(matches!(
        finished.state,
        ExecState::Exited {
            result: ExitStatus::Code(19),
            process_id: Some(41),
            ..
        }
    ));
    assert_eq!(containers.logs("exec-parent").await.unwrap().stdout, b"fake-out\n");
}

#[tokio::test]
async fn execution_wait_started_while_created_survives_start_and_returns_the_exit() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(23));
    runtime.delay = Duration::from_millis(30);
    let containers = service(Arc::new(runtime)).await;
    containers.create(spec("exec-created-wait-parent")).await.unwrap();
    containers.start("exec-created-wait-parent").await.unwrap();
    let execution = containers
        .executions()
        .create("exec-created-wait-parent", ExecSpec::new(Process::new("fake")))
        .await
        .unwrap();
    let waiting = containers.executions();
    let wait_id = execution.id.clone();
    let wait = tokio::spawn(async move { waiting.wait(&wait_id).await });
    tokio::task::yield_now().await;
    assert!(!wait.is_finished(), "created execution wait returned before start");

    let _session = containers.executions().start(&execution.id).await.unwrap();

    assert_eq!(wait.await.unwrap().unwrap(), ExitStatus::Code(23));
}

#[tokio::test]
async fn execution_waiters_are_event_driven_and_all_receive_the_terminal_result() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(23));
    runtime.delay = Duration::from_millis(30);
    let containers = service(Arc::new(runtime)).await;
    containers.create(spec("exec-wait-parent")).await.unwrap();
    containers.start("exec-wait-parent").await.unwrap();
    let execution = containers
        .executions()
        .create("exec-wait-parent", ExecSpec::new(Process::new("fake")))
        .await
        .unwrap();
    let _session = containers.executions().start(&execution.id).await.unwrap();
    let first = containers.executions();
    let second = containers.executions();
    let first_id = execution.id.clone();
    let second_id = execution.id.clone();

    let (first, second) = tokio::join!(first.wait(&first_id), second.wait(&second_id));

    assert_eq!(first.unwrap(), ExitStatus::Code(23));
    assert_eq!(second.unwrap(), ExitStatus::Code(23));
    assert_eq!(
        containers.executions().wait(&execution.id).await.unwrap(),
        ExitStatus::Code(23)
    );
}

#[tokio::test]
async fn execution_wait_reports_runtime_failure_instead_of_fabricating_an_exit_status() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(30);
    runtime.fail_wait.store(true, Ordering::SeqCst);
    let containers = service(Arc::new(runtime)).await;
    containers.create(spec("exec-wait-failure-parent")).await.unwrap();
    containers.start("exec-wait-failure-parent").await.unwrap();
    let execution = containers
        .executions()
        .create("exec-wait-failure-parent", ExecSpec::new(Process::new("fake")))
        .await
        .unwrap();
    let _session = containers.executions().start(&execution.id).await.unwrap();

    let error = containers.executions().wait(&execution.id).await.unwrap_err();

    assert!(matches!(error, Error::Runtime(message) if message == "runtime failed: injected wait failure"));
}
