use super::*;

#[tokio::test]
async fn rename_is_validated_unique_and_durable() {
    let temporary = tempfile::tempdir().unwrap();
    let config = Config::new(temporary.path());
    let repository = Arc::new(Disk::open(config.root.clone()).await.unwrap());
    let containers = test_containers(repository, Arc::new(FakeRuntime::new(ExitStatus::Code(0))))
        .await
        .unwrap();
    containers.create(spec("before")).await.unwrap();
    containers.create(spec("occupied")).await.unwrap();
    containers
        .networks()
        .create(NetworkSpec::bridge(
            "rename-dns",
            crate::Subnet::new("172.30.0.0".parse().unwrap(), 24).unwrap(),
        ))
        .await
        .unwrap();
    let generated = containers
        .networks()
        .connect("rename-dns", "before", EndpointSpec::default())
        .await
        .unwrap();
    let explicit = containers
        .networks()
        .connect("rename-dns", "occupied", EndpointSpec::default().name("fixed-dns"))
        .await
        .unwrap();
    assert!(generated.generated_name);
    assert!(!explicit.generated_name);
    containers.rename("before", "after").await.unwrap();
    assert!(matches!(
        containers.rename("after", "bad name").await,
        Err(Error::InvalidSpec(_))
    ));
    assert!(matches!(
        containers.rename("after", "occupied").await,
        Err(Error::NameConflict(_))
    ));
    drop(containers);

    let reopened = test_containers(
        Arc::new(Disk::open(config.root).await.unwrap()),
        Arc::new(FakeRuntime::new(ExitStatus::Code(0))),
    )
    .await
    .unwrap();
    assert!(matches!(reopened.inspect("before").await, Err(Error::NotFound(_))));
    assert_eq!(
        reopened.inspect("after").await.unwrap().spec.name.as_deref(),
        Some("after")
    );
    let network = reopened.networks().inspect("rename-dns").await.unwrap();
    assert_eq!(network.endpoints.get(&generated.container).unwrap().name, "after");
    assert_eq!(network.endpoints.get(&explicit.container).unwrap().name, "fixed-dns");
}

#[tokio::test(start_paused = true)]
async fn updates_persist_and_active_resources_require_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let config = Config::new(temporary.path());
    let repository = Arc::new(Disk::open(config.root.clone()).await.unwrap());
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(200);
    let containers = test_containers(repository, Arc::new(runtime)).await.unwrap();
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
    let reopened = test_containers(Arc::new(Disk::open(config.root).await.unwrap()), Arc::new(runtime))
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
