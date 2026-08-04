use super::*;

#[tokio::test]
async fn resolve_cid_exact_full_id() {
    let containers = resolving_store(&[(RESOLVE_A, "web")]).await;
    let id = RESOLVE_A.replace('-', "");
    assert_eq!(containers.inspect(&id).await.unwrap().id.as_str(), id);
}

#[tokio::test]
async fn resolve_cid_unique_prefix() {
    let containers = resolving_store(&[(RESOLVE_A, "web"), (RESOLVE_B, "db")]).await;
    assert_eq!(
        containers.inspect("aaaaaaaa0000").await.unwrap().spec.name,
        Some("web".into())
    );
    assert_eq!(containers.inspect("bbbb").await.unwrap().spec.name, Some("db".into()));
}

#[tokio::test]
async fn resolve_cid_ambiguous_prefix_is_none() {
    let containers = resolving_store(&[(RESOLVE_A, "web"), (RESOLVE_AMBIGUOUS, "worker")]).await;
    assert!(matches!(
        containers.inspect("aaaaaaaa").await,
        Err(Error::InvalidSpec(message)) if message.contains("ambiguous")
    ));
}

#[tokio::test]
async fn resolve_cid_name_fallback_trims_leading_slash() {
    let containers = resolving_store(&[(RESOLVE_A, "web")]).await;
    assert_eq!(
        containers.inspect("web").await.unwrap().spec.name.as_deref(),
        Some("web")
    );
    assert_eq!(
        containers.inspect("/web").await.unwrap().spec.name.as_deref(),
        Some("web")
    );
}

#[tokio::test]
async fn resolve_cid_no_match_is_none() {
    let containers = resolving_store(&[(RESOLVE_A, "web")]).await;
    assert!(matches!(
        containers.inspect("nonexistent").await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn create_list_inspect_remove_are_consistent() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    let first = containers.create(spec("first")).await.unwrap();
    let second = containers.create(spec("second")).await.unwrap();
    assert_eq!(containers.inspect("first").await.unwrap(), first);
    assert_eq!(containers.inspect(&second.id.as_str()[..12]).await.unwrap(), second);
    assert_eq!(containers.list().await.unwrap(), vec![first.clone(), second]);
    assert_eq!(containers.remove(first.id.as_str()).await.unwrap(), first);
    assert!(matches!(containers.inspect("first").await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn labels_can_be_rotated_without_changing_runtime_state() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_secs(1);
    let containers = service(Arc::new(runtime)).await;
    containers
        .create(spec("labeled").label("credential", "test-secret"))
        .await
        .unwrap();
    containers.start("labeled").await.unwrap();

    let updated = containers.set_label("labeled", "credential", "digest").await.unwrap();

    assert_eq!(
        updated.spec.labels.get("credential").map(String::as_str),
        Some("digest")
    );
    assert!(matches!(updated.state, ContainerState::Running { .. }));
    assert_eq!(
        containers
            .inspect("labeled")
            .await
            .unwrap()
            .spec
            .labels
            .get("credential")
            .map(String::as_str),
        Some("digest")
    );
    containers.remove_force("labeled").await.unwrap();
}

#[tokio::test]
async fn prune_removes_inactive_records_and_preserves_running_owners() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    let created = containers.create(spec("created")).await.unwrap();
    let running = containers.create(spec("running")).await.unwrap();
    containers.start("running").await.unwrap();

    assert_eq!(containers.prune(&crate::Prune::default()).await.unwrap(), vec![created]);
    assert!(matches!(
        containers.inspect("running").await.unwrap().state,
        ContainerState::Running { .. }
    ));

    assert_eq!(containers.wait("running").await.unwrap(), ExitStatus::Code(0));
    assert_eq!(
        containers
            .prune(&crate::Prune::default())
            .await
            .unwrap()
            .into_iter()
            .map(|container| container.id)
            .collect::<Vec<_>>(),
        vec![running.id]
    );
    assert!(containers.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn prune_selection_matches_labels_and_creation_time() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    let keep = containers
        .create(spec("keep").label("retention", "keep"))
        .await
        .unwrap();
    let remove = containers
        .create(spec("remove").label("stage", "temporary"))
        .await
        .unwrap();
    let selection = crate::Prune::default()
        .before(remove.created_at_ms.saturating_add(1))
        .label("stage=temporary")
        .without_label("retention");

    assert_eq!(containers.prune(&selection).await.unwrap(), [remove]);
    assert_eq!(containers.inspect("keep").await.unwrap(), keep);
}

#[tokio::test]
async fn rejects_invalid_specs_names_and_transitions() {
    let containers = service(Arc::new(FakeRuntime::new(ExitStatus::Code(0)))).await;
    assert!(matches!(
        containers
            .create(ContainerSpec::from_directory("relative", Process::new("x")))
            .await,
        Err(Error::InvalidSpec(_))
    ));
    assert!(matches!(
        containers
            .create(
                ContainerSpec::from_directory("/rootfs", Process::new("x"))
                    .mount(Mount::read_only("relative", "/guest"))
            )
            .await,
        Err(Error::InvalidSpec(_))
    ));
    containers.create(spec("same")).await.unwrap();
    assert!(matches!(
        containers.create(spec("same")).await,
        Err(Error::NameConflict(_))
    ));
    assert!(matches!(containers.wait("same").await, Err(Error::InvalidState { .. })));
    containers.start("same").await.unwrap();
    assert!(matches!(containers.start("same").await, Err(Error::AlreadyRunning(_))));
}
