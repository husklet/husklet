use super::support::{descriptor, docker_save_archive, fixture, tar_file};
use hl_images::{
    Digest, Error, ImageStore, Images, LeaseStore, Platform, RuntimeConfig,
    content::Store,
    format::docker::{Archive, Limits},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};

#[tokio::test]
async fn docker_archive_round_trip_preserves_runtime_and_rootfs() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(None);
    let source_images = Images::open(temp.path().join("source")).unwrap();
    let image = source_images
        .pull(
            &source,
            "example.test/app:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let mut archive = Vec::new();
    Archive::save(&mut archive, &source_images, std::slice::from_ref(&image)).unwrap();

    let imported = Images::open(temp.path().join("imported")).unwrap();
    let loaded = Archive::load(&archive[..], &imported, Limits::default()).unwrap();
    assert_eq!(loaded.len(), 1);
    let unpacked = imported.unpack(&loaded[0], &Platform::linux_arm64()).unwrap();
    assert_eq!(unpacked.runtime().argv(), vec!["/bin/app", "serve"]);
    let root = imported.rootfs(&unpacked).unwrap();
    assert_eq!(
        std::fs::read_to_string(imported.roots().open(&root).unwrap().path().join("etc/release")).unwrap(),
        "husklet\n"
    );
}

#[test]
fn docker_save_preserves_imported_layer_tar_bytes_without_repacking() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let layer = tar_file("O'Brien/sparse-file", b"payload");
    let image = images
        .import(
            layer.as_slice(),
            &RuntimeConfig {
                entrypoint: Vec::new(),
                command: vec!["/bin/true".into()],
                environment: BTreeMap::new(),
                working_directory: "/".into(),
                user: String::new(),
            },
            &Platform::linux_arm64(),
            &"example.test/archive:v1".parse().unwrap(),
        )
        .unwrap();
    let mut archive = Vec::new();
    Archive::save(&mut archive, &images, &[image]).unwrap();
    let mut saved_layer = None;
    for entry in tar::Archive::new(archive.as_slice()).entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().ends_with("layer.tar") {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            saved_layer = Some(bytes);
        }
    }
    assert_eq!(saved_layer.unwrap(), layer);
}

#[test]
fn docker_archive_rejects_links_and_bounds_before_import() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    assert!(matches!(
        Archive::load(&b"not a tar archive"[..], &images, Limits::default()),
        Err(Error::MalformedOci(_))
    ));
    let mut archive = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name("/etc/passwd").unwrap();
        header.set_cksum();
        builder.append_data(&mut header, "manifest.json", &b""[..]).unwrap();
        builder.finish().unwrap();
    }
    assert!(matches!(
        Archive::load(&archive[..], &images, Limits::default()),
        Err(Error::UnsafeArchive { .. })
    ));

    let archive = tar_file("manifest.json", b"[]");
    let limits = Limits {
        max_entries: 1,
        max_total_bytes: 1,
        max_metadata_bytes: 1,
    };
    assert!(matches!(
        Archive::load(&archive[..], &images, limits),
        Err(Error::MalformedOci(_))
    ));
    assert!(images.metadata().list().unwrap().is_empty());
}

#[test]
fn docker_archive_accepts_oci_layout_directories() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let mut archive = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive);
        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_size(0);
        directory.set_mode(0o755);
        directory.set_cksum();
        builder.append_data(&mut directory, "blobs/", std::io::empty()).unwrap();
        let manifest = b"[]";
        let mut file = tar::Header::new_gnu();
        file.set_size(manifest.len() as u64);
        file.set_mode(0o644);
        file.set_cksum();
        builder.append_data(&mut file, "manifest.json", &manifest[..]).unwrap();
        builder.finish().unwrap();
    }
    assert!(
        Archive::load(&archive[..], &images, Limits::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn docker_archive_accepts_nullable_docker_runtime_maps() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let layer = tar_file("etc/release", b"ubuntu\n");
    let diff = Digest::sha256(&layer).to_string();
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "config": {
            "Cmd": ["/bin/bash"],
            "OnBuild": null,
            "ExposedPorts": null,
            "Volumes": null
        },
        "rootfs": {"type": "layers", "diff_ids": [diff]}
    }))
    .unwrap();
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json",
        "RepoTags": ["ubuntu:20.04"],
        "Layers": ["layer.tar"]
    }]))
    .unwrap();
    let mut archive = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive);
        for (name, bytes) in [
            ("manifest.json", manifest.as_slice()),
            ("config.json", config.as_slice()),
            ("layer.tar", layer.as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, bytes).unwrap();
        }
        builder.finish().unwrap();
    }

    let loaded = Archive::load(&archive[..], &images, Limits::default()).unwrap();
    let unpacked = images.unpack(&loaded[0], &Platform::linux_arm64()).unwrap();
    assert_eq!(unpacked.runtime().argv(), vec!["/bin/bash"]);
}

#[test]
fn concurrent_docker_archive_loads_preserve_every_image() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let workers = (0..8)
        .map(|index| {
            let images = images.clone();
            std::thread::spawn(move || {
                let tag = format!("example.test/concurrent:v{index}");
                let archive = docker_save_archive(&tag, "linux", &serde_json::json!({}));
                Archive::load(&archive[..], &images, Limits::default()).unwrap();
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(images.metadata().list().unwrap().len(), 8);
}

#[test]
fn docker_archive_restores_platform_and_labels_from_oci_config() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let archive = docker_save_archive(
        "example.test/metadata:v1",
        "linux",
        &serde_json::json!({"owner": "husklet"}),
    );
    let loaded = Archive::load(&archive[..], &images, Limits::default()).unwrap();
    let details = images.details(&loaded[0], &Platform::linux_arm64()).unwrap();
    assert_eq!(details.platform, Platform::linux_arm64());
    assert_eq!(details.labels["owner"], "husklet");
}

#[test]
fn repeated_docker_archive_load_atomically_replaces_same_name() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let name = "example.test/replaced:v1";
    let mut replaced = None;
    for value in ["first", "second"] {
        let archive = docker_save_archive(name, "linux", &serde_json::json!({"version": value}));
        Archive::load(&archive[..], &images, Limits::default()).unwrap();
        if value == "first" {
            replaced = Some(
                images
                    .resolve(&name.parse().unwrap())
                    .unwrap()
                    .unwrap()
                    .target
                    .digest()
                    .to_string(),
            );
        }
    }
    let image = images.resolve(&name.parse().unwrap()).unwrap().unwrap();
    assert_eq!(
        images.details(&image, &Platform::linux_arm64()).unwrap().labels["version"],
        "second"
    );
    let report = images.prune_graphs(&BTreeSet::from([replaced.unwrap()])).unwrap();
    assert!(report.content_removed > 0);
    assert_eq!(images.metadata().list().unwrap().len(), 1);
}

#[test]
fn docker_archive_keeps_unsupported_platform_typed_until_selection() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let archive = docker_save_archive("example.test/windows:v1", "windows", &serde_json::json!({}));
    let loaded = Archive::load(&archive[..], &images, Limits::default()).unwrap();
    assert!(matches!(
        images.unpack(&loaded[0], &Platform::linux_arm64()),
        Err(Error::UnsupportedPlatform { .. })
    ));
}

#[test]
fn malformed_docker_manifest_is_rejected_without_names() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let archive = tar_file("manifest.json", b"{not json");
    assert!(matches!(
        Archive::load(&archive[..], &images, Limits::default()),
        Err(Error::MalformedOci(_))
    ));
    assert!(images.metadata().list().unwrap().is_empty());
}

#[test]
fn docker_archive_layers_apply_whiteouts_in_order() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let lower = tar_file("data/removed", b"lower");
    let upper = tar_file("data/.wh.removed", b"");
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64", "os": "linux",
        "config": {"Cmd": ["/bin/true"]},
        "rootfs": {"type": "layers", "diff_ids": [
            Digest::sha256(&lower).to_string(), Digest::sha256(&upper).to_string()
        ]}
    }))
    .unwrap();
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json", "RepoTags": ["example.test/whiteout:v1"],
        "Layers": ["lower.tar", "upper.tar"]
    }]))
    .unwrap();
    let mut archive = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive);
        for (name, contents) in [
            ("manifest.json", manifest.as_slice()),
            ("config.json", config.as_slice()),
            ("lower.tar", lower.as_slice()),
            ("upper.tar", upper.as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, contents).unwrap();
        }
        builder.finish().unwrap();
    }
    let loaded = Archive::load(&archive[..], &images, Limits::default()).unwrap();
    let unpacked = images.unpack(&loaded[0], &Platform::linux_arm64()).unwrap();
    let root = images.rootfs(&unpacked).unwrap();
    assert!(!images.roots().open(&root).unwrap().path().join("data/removed").exists());
}

#[test]
fn gc_respects_content_leases() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let bytes = b"orphan";
    let descriptor = descriptor("application/octet-stream", bytes);
    let digest = descriptor.digest().to_string();
    let mut ingest = images.content().ingest("orphan").unwrap();
    ingest.write(bytes).unwrap();
    ingest.commit(&descriptor).unwrap();
    let lease = images.leases().create(BTreeMap::new()).unwrap();
    images.leases().add(lease.id(), format!("content:{digest}")).unwrap();
    assert_eq!(images.gc().unwrap().content_kept, 1);
    images.leases().delete(lease.id()).unwrap();
    let report = images.gc().unwrap();
    assert_eq!(report.content_removed, 1);
    assert_eq!(report.content_bytes_removed, bytes.len() as u64);
}

#[test]
fn failed_multi_image_archive_publishes_no_names() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let layer = tar_file("ok", b"ok");
    let diff = Digest::sha256(&layer).to_string();
    let config = serde_json::to_vec(&serde_json::json!({"rootfs":{"type":"layers","diff_ids":[diff]}})).unwrap();
    let manifest = serde_json::to_vec(&serde_json::json!([
        {"Config":"config.json","RepoTags":["example.test/ok:v1"],"Layers":["layer.tar"]},
        {"Config":"missing.json","RepoTags":["example.test/bad:v1"],"Layers":["layer.tar"]}
    ]))
    .unwrap();
    let mut archive = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive);
        for (name, bytes) in [
            ("manifest.json", manifest.as_slice()),
            ("config.json", config.as_slice()),
            ("layer.tar", layer.as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, bytes).unwrap();
        }
        builder.finish().unwrap();
    }
    assert!(Archive::load(&archive[..], &images, Limits::default()).is_err());
    assert!(images.metadata().list().unwrap().is_empty());
    assert!(!images.content().digests().unwrap().is_empty());
    assert!(images.gc().unwrap().content_removed > 0);
}
