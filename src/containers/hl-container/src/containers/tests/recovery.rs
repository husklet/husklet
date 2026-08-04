use super::*;

#[tokio::test]
async fn policy_update_cancels_a_durable_pending_restart() {
    let containers = test_containers(
        Arc::new(Memory::default()),
        Arc::new(FakeRuntime::new(ExitStatus::Code(23))),
    )
    .await
    .unwrap();
    containers
        .create(spec("cancel-policy").restart(RestartPolicy::Always))
        .await
        .unwrap();
    containers.start("cancel-policy").await.unwrap();
    while !matches!(
        containers.inspect("cancel-policy").await.unwrap().state,
        ContainerState::Restarting { .. }
    ) {
        tokio::task::yield_now().await;
    }
    containers
        .update(
            "cancel-policy",
            crate::Update {
                restart: Some(RestartPolicy::Never),
                ..crate::Update::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(containers.wait("cancel-policy").await.unwrap(), ExitStatus::Code(23));
    let container = containers.inspect("cancel-policy").await.unwrap();
    assert!(matches!(container.state, ContainerState::Exited { .. }));
    assert_eq!(container.generation, 1);
}

#[tokio::test]
async fn automatic_removal_preserves_wait_result_and_reclaims_owned_volume() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(17));
    runtime.delay = Duration::from_millis(20);
    let containers = test_containers(Arc::new(Memory::default()), Arc::new(runtime))
        .await
        .unwrap();
    let volume = containers
        .volumes()
        .create_anonymous(std::iter::empty::<(&str, &str)>())
        .await
        .unwrap();
    containers
        .create(
            spec("ephemeral")
                .mount(Mount::anonymous_read_write(&volume, "/data"))
                .removal(crate::RemovalPolicy::Automatic),
        )
        .await
        .unwrap();
    containers.start("ephemeral").await.unwrap();
    assert_eq!(containers.wait("ephemeral").await.unwrap(), ExitStatus::Code(17));
    assert!(matches!(containers.inspect("ephemeral").await, Err(Error::NotFound(_))));
    assert!(matches!(
        containers.volumes().inspect(&volume.name).await,
        Err(Error::VolumeNotFound(_))
    ));
}

#[tokio::test]
async fn startup_finishes_interrupted_automatic_removal_and_preserves_exit_result() {
    let repository = Arc::new(Memory::default());
    let id = crate::ContainerId::new();
    repository
        .insert(&Container {
            id: id.clone(),
            spec: spec("interrupted-removal").removal(crate::RemovalPolicy::Automatic),
            state: ContainerState::Exited {
                result: ExitStatus::Code(19),
                finished_at_ms: 2,
            },
            created_at_ms: 1,
            generation: 1,
            restart: crate::Restart::default(),
            health: None,
            checkpoint: None,
        })
        .await
        .unwrap();

    let containers = test_containers(repository, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap();

    assert!(matches!(
        containers.inspect(&id.to_string()).await,
        Err(Error::NotFound(_))
    ));
    assert_eq!(containers.wait(&id.to_string()).await.unwrap(), ExitStatus::Code(19));
}

#[tokio::test]
async fn daemon_loss_reclaims_an_active_automatic_removal_container() {
    let repository = Arc::new(Memory::default());
    let id = crate::ContainerId::new();
    repository
        .insert(&Container {
            id: id.clone(),
            spec: spec("lost-ephemeral").removal(crate::RemovalPolicy::Automatic),
            state: ContainerState::Running {
                process_id: 99,
                started_at_ms: 1,
            },
            created_at_ms: 1,
            generation: 1,
            restart: crate::Restart::default(),
            health: None,
            checkpoint: None,
        })
        .await
        .unwrap();

    let containers = test_containers(repository, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap();

    assert!(matches!(
        containers.inspect("lost-ephemeral").await,
        Err(Error::NotFound(_))
    ));
    assert_eq!(
        containers.wait(&id.to_string()).await.unwrap(),
        ExitStatus::Fault { status: -1, detail: 0 }
    );
}

#[tokio::test]
async fn startup_reconciles_unowned_running_records() {
    let repository = Arc::new(Memory::default());
    let mut record = Container {
        id: crate::ContainerId::new(),
        spec: spec("orphan"),
        state: ContainerState::Running {
            process_id: 99,
            started_at_ms: 1,
        },
        created_at_ms: 1,
        generation: 1,
        restart: crate::Restart::default(),
        health: None,
        checkpoint: None,
    };
    repository.insert(&record).await.unwrap();
    let containers = test_containers(repository, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap();
    record = containers.inspect("orphan").await.unwrap();
    assert!(matches!(
        record.state,
        ContainerState::Exited {
            result: ExitStatus::Fault { status: -1, .. },
            ..
        }
    ));
}

#[tokio::test]
async fn daemon_loss_does_not_apply_on_failure_policy() {
    let repository = Arc::new(Memory::default());
    let mut record = Container {
        id: crate::ContainerId::new(),
        spec: spec("daemon-loss").restart(RestartPolicy::OnFailure { maximum: None }),
        state: ContainerState::Running {
            process_id: 99,
            started_at_ms: 1,
        },
        created_at_ms: 1,
        generation: 1,
        restart: crate::Restart::default(),
        health: None,
        checkpoint: None,
    };
    repository.insert(&record).await.unwrap();

    let runtime = Arc::new(FakeRuntime::new(ExitStatus::Code(0)));
    let runtime_service: Arc<dyn Runtime> = runtime.clone();
    let containers = test_containers(repository, runtime_service).await.unwrap();
    record = containers.inspect("daemon-loss").await.unwrap();

    assert!(matches!(record.state, ContainerState::Exited { .. }));
    assert_eq!(record.generation, 1);
    assert!(runtime.mounts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn daemon_loss_restarts_always_policy() {
    let repository = Arc::new(Memory::default());
    let id = crate::ContainerId::new();
    repository
        .insert(&Container {
            id: id.clone(),
            spec: spec("daemon-always").restart(RestartPolicy::Always),
            state: ContainerState::Running {
                process_id: 99,
                started_at_ms: 1,
            },
            created_at_ms: 1,
            generation: 1,
            restart: crate::Restart::default(),
            health: None,
            checkpoint: None,
        })
        .await
        .unwrap();

    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let containers = test_containers(repository, Arc::new(runtime)).await.unwrap();
    let recovered = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let container = containers.inspect("daemon-always").await.unwrap();
            if container.generation == 2 {
                break container;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(recovered.generation, 2);
    assert!(matches!(recovered.state, ContainerState::Running { .. }));
    containers.stop("daemon-always", Duration::ZERO).await.unwrap();
}

#[tokio::test]
async fn startup_preserves_an_exec_that_has_a_committed_checkpoint() {
    let repository = Arc::new(Memory::default());
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let containers = test_containers(Arc::clone(&repository), Arc::new(runtime))
        .await
        .unwrap();
    containers.create(spec("workspace")).await.unwrap();
    containers.start("workspace").await.unwrap();
    let exec = containers
        .executions()
        .create("workspace", ExecSpec::new(Process::new("/bin/sleep")))
        .await
        .unwrap();
    let _session = containers.executions().start(&exec.id).await.unwrap();
    containers.checkpoint_all(Duration::from_secs(1)).await.unwrap();
    drop(containers);

    let reopened = test_containers(repository, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap();
    let recovered = reopened.executions().inspect(&exec.id).await.unwrap();

    assert_eq!(recovered.state, ExecState::Created);
    assert!(recovered.checkpoint.is_some());
}

#[tokio::test]
async fn startup_resumes_durable_restart_backoff() {
    let repository = Arc::new(Memory::default());
    let id = crate::ContainerId::new();
    repository
        .insert(&Container {
            id: id.clone(),
            spec: spec("recovering").restart(RestartPolicy::OnFailure { maximum: None }),
            state: ContainerState::Restarting {
                result: ExitStatus::Code(1),
                finished_at_ms: 1,
                ready_at_ms: 1,
            },
            created_at_ms: 1,
            generation: 1,
            restart: crate::Restart::default(),
            health: None,
            checkpoint: None,
        })
        .await
        .unwrap();
    let containers = test_containers(repository, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap();
    assert_eq!(containers.wait("recovering").await.unwrap(), ExitStatus::Code(0));
    let recovered = containers.inspect("recovering").await.unwrap();
    assert_eq!(recovered.id, id);
    assert_eq!(recovered.generation, 2);
    assert_eq!(recovered.restart.count, 1);
}
