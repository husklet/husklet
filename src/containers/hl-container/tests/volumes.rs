use hl_container::{
    Access, Config, ContainerSpec, Containers, Error, Limits, Mount, MountSource, Persistence, Process, VolumeSource,
    VolumeSpec,
};
use std::collections::BTreeMap;

#[tokio::test]
async fn named_and_anonymous_volumes_are_validated_idempotent_and_ordered() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()).persistence(Persistence::Memory))
        .build()
        .await
        .unwrap();
    let volumes = containers.volumes();
    let zebra = volumes
        .create(VolumeSpec::new("zebra").label("scope", "test"))
        .await
        .unwrap();
    assert_eq!(
        volumes
            .create(VolumeSpec::new("zebra").label("scope", "test"))
            .await
            .unwrap(),
        zebra
    );
    assert!(matches!(
        volumes
            .create(VolumeSpec::new("zebra").label("scope", "other"))
            .await,
        Err(Error::VolumeConflict(name)) if name == "zebra"
    ));
    let anonymous = volumes.create_anonymous([("scope", "anonymous")]).await.unwrap();
    assert_eq!(anonymous.name.len(), 32);
    assert_eq!(anonymous.labels["scope"], "anonymous");
    assert_eq!(
        Mount::anonymous(&anonymous, "/anonymous", Access::ReadOnly).source,
        MountSource::Anonymous(anonymous.name.clone())
    );

    let listed = volumes.list().await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed[0].name < listed[1].name);
    assert_eq!(volumes.inspect("zebra").await.unwrap(), zebra);

    for unsafe_name in ["", ".", "..", "../escape", "/absolute", "bad name", "_bad"] {
        assert!(matches!(
            volumes.create(VolumeSpec::new(unsafe_name)).await,
            Err(Error::InvalidVolume(_))
        ));
    }
}

#[tokio::test]
async fn conditional_remove_rejects_recreated_and_legacy_generations_atomically() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path()).persistence(Persistence::Memory))
        .build().await.unwrap();
    let volumes = containers.volumes();
    let first = volumes.create(VolumeSpec::new("stable-name")).await.unwrap();
    assert_eq!(first.generation.len(), 32);
    volumes.remove_if_generation(&first.name, &first.generation).await.unwrap();
    let second = volumes.create(VolumeSpec::new("stable-name")).await.unwrap();
    assert_ne!(first.generation, second.generation);
    assert!(matches!(
        volumes.remove_if_generation(&second.name, &first.generation).await,
        Err(Error::VolumeConflict(name)) if name == "stable-name"
    ));
    assert_eq!(volumes.inspect("stable-name").await.unwrap(), second);
    assert!(matches!(
        volumes.remove_if_generation("stable-name", "").await,
        Err(Error::VolumeConflict(name)) if name == "stable-name"
    ));
}

#[tokio::test]
async fn bind_backed_volume_canonicalizes_and_never_owns_host_data() {
    let root = tempfile::tempdir().unwrap();
    let device = root.path().join("device");
    std::fs::create_dir(&device).unwrap();
    std::fs::write(device.join("kept"), b"host-owned").unwrap();
    let containers = Containers::builder(Config::new(root.path().join("state")))
        .build()
        .await
        .unwrap();

    let volume = containers
        .volumes()
        .create(
            VolumeSpec::new("host-data")
                .option("type", "none")
                .option("o", "bind,ro")
                .option("device", device.to_string_lossy())
                .bind(&device, true),
        )
        .await
        .unwrap();
    assert_eq!(volume.path(), std::fs::canonicalize(&device).unwrap());
    assert!(matches!(volume.source, VolumeSource::Bind { read_only: true, .. }));
    assert_eq!(containers.volumes().size("host-data").await.unwrap(), 0);

    containers.volumes().remove("host-data").await.unwrap();
    assert_eq!(std::fs::read(device.join("kept")).unwrap(), b"host-owned");
}

#[tokio::test]
async fn bind_backed_volume_rejects_relative_missing_and_file_devices() {
    let root = tempfile::tempdir().unwrap();
    let containers = Containers::builder(Config::new(root.path().join("state")))
        .build()
        .await
        .unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, b"not a directory").unwrap();

    for (name, device) in [
        ("relative", std::path::PathBuf::from("relative")),
        ("missing", root.path().join("missing")),
        ("file", file),
    ] {
        assert!(matches!(
            containers
                .volumes()
                .create(VolumeSpec::new(name).bind(device, false))
                .await,
            Err(Error::InvalidVolume(_))
        ));
    }
    assert!(containers.volumes().list().await.unwrap().is_empty());
}

#[tokio::test]
async fn container_owned_volumes_follow_explicit_remove_policy() {
    let root = tempfile::tempdir().unwrap();
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let containers = Containers::builder(Config::new(root.path())).build().await.unwrap();
    let anonymous = containers
        .volumes()
        .create_anonymous(std::iter::empty::<(&str, &str)>())
        .await
        .unwrap();
    containers
        .create(
            ContainerSpec::from_directory(&rootfs, Process::new("/bin/true"))
                .name("keep-data")
                .mount(Mount::anonymous(&anonymous, "/data", Access::ReadWrite)),
        )
        .await
        .unwrap();
    containers.remove("keep-data").await.unwrap();
    assert!(containers.volumes().inspect(&anonymous.name).await.is_ok());

    containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/bin/true"))
                .name("remove-data")
                .mount(Mount::anonymous(&anonymous, "/data", Access::ReadWrite)),
        )
        .await
        .unwrap();
    containers.remove_volumes("remove-data", false).await.unwrap();
    assert!(matches!(
        containers.volumes().inspect(&anonymous.name).await,
        Err(Error::VolumeNotFound(_))
    ));
}

#[tokio::test]
async fn file_metadata_and_nonempty_data_survive_reopen_then_remove_together() {
    let root = tempfile::tempdir().unwrap();
    let config = Config::new(root.path());
    let containers = Containers::builder(config.clone()).build().await.unwrap();
    let volume = containers
        .volumes()
        .create(VolumeSpec::new("durable").option("kind", "local"))
        .await
        .unwrap();
    std::fs::create_dir(volume.path().join("nested")).unwrap();
    std::fs::write(volume.path().join("nested/payload"), b"persistent").unwrap();
    drop(containers);

    let reopened = Containers::builder(config).build().await.unwrap();
    let inspected = reopened.volumes().inspect("durable").await.unwrap();
    assert_eq!(
        std::fs::read(inspected.path().join("nested/payload")).unwrap(),
        b"persistent"
    );
    assert_eq!(inspected.options["kind"], "local");

    reopened.volumes().remove("durable").await.unwrap();
    assert!(!inspected.path().exists());
    drop(reopened);
    let final_service = Containers::builder(Config::new(root.path())).build().await.unwrap();
    assert!(matches!(
        final_service.volumes().inspect("durable").await,
        Err(Error::VolumeNotFound(_))
    ));
}

#[tokio::test]
async fn prune_skips_every_volume_referenced_by_persisted_containers() {
    let root = tempfile::tempdir().unwrap();
    let config = Config::new(root.path());
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let containers = Containers::builder(config.clone()).build().await.unwrap();
    let held = containers.volumes().create(VolumeSpec::new("held")).await.unwrap();
    let free = containers.volumes().create(VolumeSpec::new("free")).await.unwrap();
    std::fs::write(held.path().join("owned"), b"container data").unwrap();
    containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/bin/true"))
                .name("owner")
                .mount(Mount::volume("held", "/data", Access::ReadWrite)),
        )
        .await
        .unwrap();
    assert_eq!(
        containers.inspect("owner").await.unwrap().spec.mounts[0].source,
        MountSource::Volume("held".into())
    );
    drop(containers);
    let containers = Containers::builder(config).build().await.unwrap();

    assert!(matches!(
        containers.volumes().remove("held").await,
        Err(Error::VolumeInUse(name)) if name == "held"
    ));
    assert_eq!(containers.volumes().prune().await.unwrap(), vec![free.clone()]);
    assert_eq!(containers.volumes().inspect("held").await.unwrap(), held);
    assert!(!free.path().exists());

    assert_eq!(containers.volumes().remove_force("held").await.unwrap(), held);
    assert!(!held.path().exists());
    assert!(matches!(
        containers.volumes().inspect("held").await,
        Err(Error::VolumeNotFound(name)) if name == "held"
    ));
    containers.remove("owner").await.unwrap();
}

#[tokio::test]
async fn reference_counts_scan_once_and_count_each_container_once_per_volume() {
    let root = tempfile::tempdir().unwrap();
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let containers = Containers::builder(Config::new(root.path())).build().await.unwrap();
    let held = containers.volumes().create(VolumeSpec::new("held")).await.unwrap();
    let free = containers.volumes().create(VolumeSpec::new("free")).await.unwrap();
    let anonymous = containers
        .volumes()
        .create_anonymous(std::iter::empty::<(&str, &str)>())
        .await
        .unwrap();
    containers
        .create(
            ContainerSpec::from_directory(&rootfs, Process::new("/bin/true"))
                .name("first-owner")
                .mount(Mount::volume("held", "/first", Access::ReadWrite))
                .mount(Mount::volume("held", "/second", Access::ReadOnly))
                .mount(Mount::anonymous(&anonymous, "/private", Access::ReadWrite)),
        )
        .await
        .unwrap();
    containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/bin/true"))
                .name("second-owner")
                .mount(Mount::volume("held", "/data", Access::ReadWrite)),
        )
        .await
        .unwrap();

    let counts = containers
        .volumes()
        .reference_counts(&[held.clone(), free, anonymous.clone()])
        .await
        .unwrap();
    assert_eq!(
        counts,
        BTreeMap::from([(anonymous.name.clone(), 1), ("free".into(), 0), ("held".into(), 2),])
    );

    let selected = containers
        .volumes()
        .reference_counts(std::slice::from_ref(&held))
        .await
        .unwrap();
    assert_eq!(selected, BTreeMap::from([("held".into(), 2)]));

    containers.remove("second-owner").await.unwrap();
    let after_removal = containers.volumes().reference_counts(&[held, anonymous]).await.unwrap();
    assert_eq!(after_removal.values().copied().collect::<Vec<_>>(), [1, 1]);
}

#[tokio::test]
async fn container_creation_rejects_missing_semantic_volume_without_persisting_it() {
    let root = tempfile::tempdir().unwrap();
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let containers = Containers::builder(Config::new(root.path())).build().await.unwrap();
    assert!(matches!(
        containers
            .create(
                ContainerSpec::from_directory(rootfs, Process::new("/bin/true"))
                    .name("dangling")
                    .mount(Mount::volume(
                        "missing",
                        "/data",
                        Access::ReadWrite,
                    )),
            )
            .await,
        Err(Error::VolumeNotFound(name)) if name == "missing"
    ));
    assert!(containers.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn filesystem_resolves_volume_names_and_preserves_read_only_access() {
    let root = tempfile::tempdir().unwrap();
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let containers = Containers::builder(Config::new(root.path())).build().await.unwrap();
    let volume = containers.volumes().create(VolumeSpec::new("documents")).await.unwrap();
    std::fs::write(volume.path().join("message"), b"semantic source").unwrap();
    containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/bin/true"))
                .name("reader")
                .mount(Mount::volume("documents", "/documents", Access::ReadOnly)),
        )
        .await
        .unwrap();

    let filesystem = containers.filesystem("reader").await.unwrap();
    assert_eq!(filesystem.stat("/documents/message").unwrap().size, 15);
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(1);
    header.set_mode(0o600);
    header.set_cksum();
    archive.append_data(&mut header, "new", &b"x"[..]).unwrap();
    archive.finish().unwrap();
    let bytes = archive.into_inner().unwrap();
    assert!(matches!(
        filesystem.extract("/documents", &bytes[..], Limits::default()),
        Err(Error::ReadOnly(_))
    ));
}

#[tokio::test]
async fn volume_subpaths_resolve_existing_directories_without_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let rootfs = root.path().join("rootfs");
    std::fs::create_dir(&rootfs).unwrap();
    let containers = Containers::builder(Config::new(root.path())).build().await.unwrap();
    let volume = containers.volumes().create(VolumeSpec::new("subpaths")).await.unwrap();
    std::fs::create_dir_all(volume.path().join("tenants/alpha")).unwrap();
    std::fs::write(volume.path().join("tenants/alpha/value"), b"alpha").unwrap();
    let valid = Mount::volume("subpaths", "/data", Access::ReadOnly)
        .subpath("tenants/alpha")
        .unwrap();
    containers
        .create(
            ContainerSpec::from_directory(&rootfs, Process::new("/bin/true"))
                .name("subpath-valid")
                .mount(valid),
        )
        .await
        .unwrap();
    assert_eq!(
        containers
            .filesystem("subpath-valid")
            .await
            .unwrap()
            .stat("/data/value")
            .unwrap()
            .size,
        5
    );

    for (name, subpath) in [("subpath-missing", "missing"), ("subpath-parent", "../escape")] {
        let mount = Mount::volume("subpaths", "/data", Access::ReadOnly).subpath(subpath);
        if let Ok(mount) = mount {
            containers
                .create(
                    ContainerSpec::from_directory(&rootfs, Process::new("/bin/true"))
                        .name(name)
                        .mount(mount),
                )
                .await
                .unwrap();
            assert!(containers.filesystem(name).await.is_err());
        }
    }

    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, volume.path().join("escape")).unwrap();
    let escaped = Mount::volume("subpaths", "/data", Access::ReadOnly)
        .subpath("escape")
        .unwrap();
    containers
        .create(
            ContainerSpec::from_directory(rootfs, Process::new("/bin/true"))
                .name("subpath-symlink")
                .mount(escaped),
        )
        .await
        .unwrap();
    assert!(containers.filesystem("subpath-symlink").await.is_err());
}

#[tokio::test]
async fn reconciliation_preserves_unrecognized_directories() {
    let root = tempfile::tempdir().unwrap();
    let unknown = root.path().join("volumes/external");
    std::fs::create_dir_all(&unknown).unwrap();
    std::fs::write(unknown.join("keep"), b"not owned").unwrap();
    let containers = Containers::builder(Config::new(root.path())).build().await.unwrap();
    assert_eq!(std::fs::read(unknown.join("keep")).unwrap(), b"not owned");
    assert_eq!(containers.volumes().list().await.unwrap(), Vec::new());
}

#[test]
fn public_volume_types_remain_plain_composable_entities() {
    let spec = VolumeSpec::new("cache")
        .label("purpose", "build")
        .option("copy", "false");
    assert_eq!(spec.name, "cache");
    assert_eq!(spec.labels, BTreeMap::from([("purpose".into(), "build".into())]));
}
