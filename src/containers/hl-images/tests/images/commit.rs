use super::support::tar_file;
use hl_images::{History, Images, Metadata, Platform, Reference, RuntimeConfig};
use std::collections::BTreeMap;

#[test]
fn commit_creates_unpackable_named_image() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/bin/app".into()],
        command: vec!["serve".into()],
        environment: BTreeMap::from([("A".into(), "one".into())]),
        working_directory: "/work".into(),
        user: "1000:1000".into(),
    };
    let name: Reference = "example.test/committed:v1".parse().unwrap();
    let image = images
        .commit(
            &tar_file("marker", b"current"),
            &runtime,
            &Platform::linux_arm64(),
            &name,
        )
        .unwrap();
    let resolved = images.resolve(&name).unwrap().unwrap();
    assert_eq!(resolved.name, image.name);
    assert_eq!(resolved.target.digest(), image.target.digest());
    let unpacked = images.unpack(&image, &Platform::linux_arm64()).unwrap();
    let root = images.rootfs(&unpacked).unwrap();
    let view = images.roots().open(&root).unwrap();
    assert_eq!(
        std::fs::read(view.path().join("marker")).unwrap(),
        b"current"
    );
    assert_eq!(unpacked.runtime(), &runtime);
}

#[test]
fn imported_rootfs_tar_is_unpackable_as_a_named_image() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let image = images
        .import(
            std::io::Cursor::new(tar_file("etc/imported", b"yes\n")),
            &RuntimeConfig {
                entrypoint: Vec::new(),
                command: vec!["/bin/true".into()],
                environment: BTreeMap::new(),
                working_directory: "/".into(),
                user: String::new(),
            },
            &Platform::linux_arm64(),
            &"example.test/imported:v1".parse().unwrap(),
        )
        .unwrap();
    let unpacked = images.unpack(&image, &Platform::linux_arm64()).unwrap();
    let root = images.rootfs(&unpacked).unwrap();
    assert_eq!(
        std::fs::read_to_string(
            images
                .roots()
                .open(&root)
                .unwrap()
                .path()
                .join("etc/imported")
        )
        .unwrap(),
        "yes\n"
    );
}

#[test]
fn build_persists_labels_runtime_and_instruction_history() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/bin/sh".into(), "-c".into()],
        command: vec!["echo built".into()],
        environment: BTreeMap::from([("VALUE".into(), "built".into())]),
        working_directory: "/work".into(),
        user: "12:34".into(),
    };
    let labels = BTreeMap::from([("org.example.stage".into(), "test".into())]);
    let history = vec![
        History {
            created_by: Some("FROM alpine".into()),
            empty_layer: true,
            ..History::default()
        },
        History {
            created_by: Some("RUN echo built".into()),
            ..History::default()
        },
    ];
    let metadata = Metadata {
        author: None,
        platform: Platform::linux_arm64(),
        created: None,
        labels: labels.clone(),
        history: history.clone(),
        runtime: runtime.clone(),
        onbuild: Vec::new(),
        exposed_ports: std::collections::BTreeSet::new(),
        volumes: std::collections::BTreeSet::new(),
        healthcheck: None,
        stop_signal: None,
    };
    let image = images
        .build(
            &tar_file("marker", b"built"),
            &"example.test/built:v1".parse().unwrap(),
            &metadata,
        )
        .unwrap();
    let details = images.details(&image, &Platform::linux_arm64()).unwrap();
    assert_eq!(details.runtime, runtime);
    assert_eq!(details.labels, labels);
    assert_eq!(details.history, history);
}
