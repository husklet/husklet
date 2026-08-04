use super::*;

#[tokio::test]
async fn image_rootfs_lease_survives_reopen_and_releases_on_remove() {
    let temporary = tempfile::tempdir().unwrap();
    let snapshots_path = temporary.path().join("images/snapshots");
    let leases_path = temporary.path().join("images/leases");
    let snapshots = hl_images::snapshot::Snapshots::open(&snapshots_path).unwrap();
    let snapshot = hl_images::snapshot::Id::new("base").unwrap();
    let active = snapshots
        .prepare(hl_images::snapshot::Id::new("prepare").unwrap(), None)
        .unwrap();
    std::fs::write(active.path().join("marker"), b"rootfs").unwrap();
    active.commit(snapshot.clone()).unwrap();
    let manager = hl_images::rootfs::Roots::new(snapshots, hl_images::Leases::open(&leases_path).unwrap());
    let reference = manager.pin(&snapshot).unwrap();
    let containers = build_with(
        Arc::new(Disk::open(temporary.path().to_owned()).await.unwrap()),
        Arc::new(FakeRuntime::new(ExitStatus::Code(0))),
        Some(manager.clone()),
        None,
        temporary.path().join("volumes"),
        temporary.path().join("runtime"),
    )
    .await
    .unwrap();
    containers
        .create(
            ContainerSpec::new(reference.clone(), Process::new("/bin/true"))
                .image("example.test/library/base:latest".parse().unwrap())
                .name("image"),
        )
        .await
        .unwrap();
    drop(containers);

    let reopened_manager = hl_images::rootfs::Roots::new(
        hl_images::snapshot::Snapshots::open(&snapshots_path).unwrap(),
        hl_images::Leases::open(&leases_path).unwrap(),
    );
    assert_eq!(
        std::fs::read(reopened_manager.open(&reference).unwrap().path().join("marker")).unwrap(),
        b"rootfs"
    );
    let reopened = build_with(
        Arc::new(Disk::open(temporary.path().to_owned()).await.unwrap()),
        Arc::new(FakeRuntime::new(ExitStatus::Code(0))),
        Some(reopened_manager.clone()),
        None,
        temporary.path().join("volumes"),
        temporary.path().join("runtime"),
    )
    .await
    .unwrap();
    assert_eq!(
        reopened
            .inspect("image")
            .await
            .unwrap()
            .spec
            .image
            .as_ref()
            .unwrap()
            .to_string(),
        "example.test/library/base:latest"
    );
    reopened.remove("image").await.unwrap();
    assert!(reopened_manager.open(&reference).is_err());
}
