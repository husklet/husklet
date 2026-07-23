use super::*;

#[tokio::test]
async fn updates_persist_and_active_resources_require_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let config = Config::new(temporary.path());
    let repository = Arc::new(Disk::open(config.root.clone()).await.unwrap());
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(200);
    let containers = test_containers(repository, Arc::new(runtime))
        .await
        .unwrap();
    containers.create(spec("mutable")).await.unwrap();
    containers
        .update(
            "mutable",
            crate::Update {
                memory_bytes: Some(4096),
                process_count: Some(8),
                cpu_count: Some(2),
                restart: Some(RestartPolicy::Always),
            },
        )
        .await
        .unwrap();
    drop(containers);

    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(200);
    let reopened = test_containers(
        Arc::new(Disk::open(config.root).await.unwrap()),
        Arc::new(runtime),
    )
    .await
    .unwrap();
    let stored = reopened.inspect("mutable").await.unwrap();
    assert_eq!(stored.spec.resources.memory_bytes, 4096);
    assert_eq!(stored.spec.resources.process_count, 8);
    assert_eq!(stored.spec.resources.cpu_count, 2);
    assert_eq!(stored.spec.restart, RestartPolicy::Always);

    reopened.start("mutable").await.unwrap();
    reopened
        .update(
            "mutable",
            crate::Update {
                restart: Some(RestartPolicy::Never),
                ..crate::Update::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        reopened
            .update(
                "mutable",
                crate::Update {
                    memory_bytes: Some(8192),
                    ..crate::Update::default()
                }
            )
            .await,
        Err(Error::InvalidState { .. })
    ));
    assert_eq!(reopened.wait("mutable").await.unwrap(), ExitStatus::Code(0));
    let stored = reopened.inspect("mutable").await.unwrap();
    assert_eq!(stored.spec.resources.memory_bytes, 4096);
    assert_eq!(stored.spec.restart, RestartPolicy::Never);
}
