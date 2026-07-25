use super::support::descriptor;
use hl_images::{FsImageStore, Image, ImageStore, Reference};
use std::collections::BTreeSet;

#[test]
#[ignore = "subprocess entry point for the cross-process catalog regression"]
fn image_catalog_writer_process() {
    let Some(root) = std::env::var_os("HL_IMAGE_WRITER_ROOT") else {
        return;
    };
    let writer = std::env::var("HL_IMAGE_WRITER_ID").unwrap();
    let store = FsImageStore::open(&root).unwrap();
    std::fs::write(
        std::path::Path::new(&root).join(format!("ready-{writer}")),
        b"",
    )
    .unwrap();
    while !std::path::Path::new(&root).join("go").exists() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    store
        .put(Image {
            name: format!("example.test/process:v{writer}").parse().unwrap(),
            target: descriptor("application/vnd.oci.image.manifest.v1+json", b"shared"),
        })
        .unwrap();
}

#[test]
fn corrupt_catalog_fails_closed_and_is_never_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let metadata = temp.path().join("metadata");
    std::fs::create_dir_all(&metadata).unwrap();
    let path = metadata.join("images.json");
    let truncated = br#"{"version":1,"images":{"#;
    std::fs::write(&path, truncated).unwrap();
    assert!(FsImageStore::open(&metadata).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), truncated);

    std::fs::remove_file(&path).unwrap();
    let store = FsImageStore::open(&metadata).unwrap();
    std::fs::write(&path, truncated).unwrap();
    let image = Image {
        name: "example.test/no-overwrite:v1".parse().unwrap(),
        target: descriptor("application/vnd.oci.image.manifest.v1+json", b"target"),
    };
    assert!(store.put(image).is_err());
    assert_eq!(std::fs::read(path).unwrap(), truncated);
}

#[test]
fn independently_opened_concurrent_writers_preserve_every_name() {
    let temp = tempfile::tempdir().unwrap();
    let metadata = temp.path().join("metadata");
    let target = descriptor("application/vnd.oci.image.manifest.v1+json", b"shared");
    let mut threads = Vec::new();
    for index in 0..24 {
        let metadata = metadata.clone();
        let target = target.clone();
        threads.push(std::thread::spawn(move || {
            let store = FsImageStore::open(metadata).unwrap();
            store
                .put(Image {
                    name: format!("example.test/concurrent:v{index}").parse().unwrap(),
                    target,
                })
                .unwrap();
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let store = FsImageStore::open(metadata).unwrap();
    assert_eq!(store.list().unwrap().len(), 24);
    assert_eq!(store.graphs().unwrap().len(), 1);
    assert_eq!(store.graphs().unwrap()[0].names.len(), 24);
}

#[test]
fn catalog_keeps_colliding_legacy_path_spellings_distinct_without_path_projection() {
    let temp = tempfile::tempdir().unwrap();
    let store = FsImageStore::open(temp.path()).unwrap();
    let first = Image {
        name: "example.test/a_b/c:v1".parse().unwrap(),
        target: descriptor("application/vnd.oci.image.manifest.v1+json", b"first"),
    };
    let second = Image {
        name: "example.test/a/b_c:v1".parse().unwrap(),
        target: descriptor("application/vnd.oci.image.manifest.v1+json", b"second"),
    };
    store.put_all([first.clone(), second.clone()]).unwrap();
    assert_eq!(
        store.get(&first.name).unwrap().unwrap().target,
        first.target
    );
    assert_eq!(
        store.get(&second.name).unwrap().unwrap().target,
        second.target
    );
    assert!("../outside:latest".parse::<Reference>().is_err());
    assert!("host/repository/../../outside:latest"
        .parse::<Reference>()
        .is_err());
}

#[test]
fn concurrent_processes_preserve_every_catalog_name() {
    const WRITERS: usize = 8;
    let temp = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut children = (0..WRITERS)
        .map(|writer| {
            std::process::Command::new(&executable)
                .args([
                    "--ignored",
                    "--exact",
                    "suite::catalog::image_catalog_writer_process",
                ])
                .env("HL_IMAGE_WRITER_ROOT", temp.path())
                .env("HL_IMAGE_WRITER_ID", writer.to_string())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while (0..WRITERS).any(|writer| !temp.path().join(format!("ready-{writer}")).exists()) {
        assert!(
            std::time::Instant::now() < deadline,
            "writers did not become ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::fs::write(temp.path().join("go"), b"").unwrap();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }

    let store = FsImageStore::open(temp.path()).unwrap();
    assert_eq!(store.list().unwrap().len(), WRITERS);
    assert_eq!(store.graphs().unwrap().len(), 1);
}

#[test]
fn build_cache_graphs_are_prunable_independently_of_ordinary_tags() {
    let temp = tempfile::tempdir().unwrap();
    let store = FsImageStore::open(temp.path()).unwrap();
    let ordinary = Image {
        name: "example.test/app:v1".parse().unwrap(),
        target: descriptor("application/vnd.oci.image.manifest.v1+json", b"ordinary"),
    };
    let cache = Image {
        name: "hl-build-cache/step:v1".parse().unwrap(),
        target: descriptor("application/vnd.oci.image.manifest.v1+json", b"cache"),
    };
    store.put_all([ordinary.clone(), cache.clone()]).unwrap();
    let selected = BTreeSet::from([
        ordinary.target.digest().to_string(),
        cache.target.digest().to_string(),
    ]);
    let removed = store.remove_graphs(&selected).unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].digest(), cache.target.digest());
    assert!(store.get(&ordinary.name).unwrap().is_some());
    assert!(store.get(&cache.name).unwrap().is_none());

    let shared = descriptor("application/vnd.oci.image.manifest.v1+json", b"mixed");
    let ordinary_alias = Image {
        name: "example.test/mixed:v1".parse().unwrap(),
        target: shared.clone(),
    };
    let cache_alias = Image {
        name: "hl-build-cache/mixed:v1".parse().unwrap(),
        target: shared,
    };
    store
        .put_all([cache_alias.clone(), ordinary_alias.clone()])
        .unwrap();
    assert!(store
        .remove_graphs(&BTreeSet::from([ordinary_alias
            .target
            .digest()
            .to_string()]))
        .unwrap()
        .is_empty());
    assert!(store.get(&ordinary_alias.name).unwrap().is_some());
    assert!(store.get(&cache_alias.name).unwrap().is_some());
}
