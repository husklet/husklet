use super::support::{
    Broken, BrokenSource, MemorySource, descriptor, fixture, invalid_config_fixture, scratch_fixture,
};
use bytes::Bytes;
use hl_images::{Digest, Error, ImageStore, Images, LeaseStore, Platform, Reference, RuntimeOverrides, content::Store};
use std::{collections::BTreeMap, sync::Arc};

#[tokio::test]
async fn pull_unpack_and_durable_rootfs_survive_restart() {
    let temp = tempfile::tempdir().unwrap();
    let (source, selected_manifest) = fixture(None);
    let reference: Reference = "example.test/team/app:v1".parse().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let pulled = images
        .pull(&source, reference.clone(), &Platform::linux_arm64())
        .await
        .unwrap();
    assert_eq!(pulled.target.digest(), selected_manifest.digest());
    assert_eq!(images.content().digests().unwrap().len(), 5);
    let expected_size = source
        .blobs
        .iter()
        .filter(|(digest, _)| *digest != &source.root.digest().to_string())
        .map(|(_, bytes)| bytes.len() as u64)
        .sum::<u64>();
    assert_eq!(images.size(&pulled).unwrap(), expected_size);
    assert_eq!(images.gc().unwrap().content_removed, 1);
    assert_eq!(images.content().digests().unwrap().len(), 4);
    drop(images);

    let images = Images::open(temp.path()).unwrap();
    let image = images.metadata().get(&reference).unwrap().unwrap();
    let unpacked = images.unpack(&image, &Platform::linux_arm64()).unwrap();
    assert_eq!(
        hl_images::snapshot::Snapshots::open(temp.path().join("snapshots"))
            .unwrap()
            .committed()
            .unwrap()
            .len(),
        1,
        "fresh multi-layer unpack must not copy and commit every prefix"
    );
    assert_eq!(unpacked.platform(), &Platform::linux_arm64());
    assert_eq!(unpacked.runtime().argv(), vec!["/bin/app", "serve"]);
    assert_eq!(unpacked.runtime().environment.get("A").unwrap(), "new");
    let merged = unpacked
        .runtime()
        .merge(RuntimeOverrides {
            command: Some(vec!["check".into()]),
            environment: BTreeMap::from([("A".into(), "container".into())]),
            working_directory: Some("/override".into()),
            ..RuntimeOverrides::default()
        })
        .unwrap();
    assert_eq!(merged.argv(), vec!["/bin/app", "check"]);
    assert_eq!(merged.environment.get("A").unwrap(), "container");
    assert_eq!(merged.working_directory, "/override");
    let root = images.rootfs(&unpacked).unwrap();
    let sibling = images.rootfs(&unpacked).unwrap();
    let root_ownership = temp
        .path()
        .join("snapshots/ownership/committed")
        .join(format!("{}.json", root.snapshot().as_str()));
    let sibling_ownership = temp
        .path()
        .join("snapshots/ownership/committed")
        .join(format!("{}.json", sibling.snapshot().as_str()));
    assert!(root_ownership.exists());
    assert!(sibling_ownership.exists());
    let root_view = images.roots().open(&root).unwrap();
    std::fs::write(root_view.path().join("etc/release"), "private\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(images.roots().open(&sibling).unwrap().path().join("etc/release")).unwrap(),
        "husklet\n"
    );
    // Both writable roots and their shared immutable baseline are pinned.
    assert_eq!(images.gc().unwrap().snapshots_kept, 3);
    images.roots().release(&sibling).unwrap();
    assert!(!sibling_ownership.exists());
    let serialized = serde_json::to_vec(&root).unwrap();
    drop(images);

    let images = Images::open(temp.path()).unwrap();
    let root = serde_json::from_slice(&serialized).unwrap();
    assert!(root_ownership.exists());
    let handle = images.roots().open(&root).unwrap();
    assert_eq!(
        std::fs::read_to_string(handle.path().join("etc/release")).unwrap(),
        "private\n"
    );
    images.roots().release(&root).unwrap();
    assert!(!root_ownership.exists());
    // The unpacked image pins its immutable chain until callers have finished
    // deriving writable roots. Releasing both the last root and that transient
    // handle leaves the baseline reclaimable after a process restart.
    drop(unpacked);
    assert_eq!(images.gc().unwrap().snapshots_removed, 1);
}

#[tokio::test]
async fn zero_layer_scratch_image_pulls_and_unpacks() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let image = images
        .pull(
            &scratch_fixture(),
            "example.test/scratch:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let unpacked = images.unpack(&image, &Platform::linux_arm64()).unwrap();
    assert_eq!(unpacked.runtime().argv(), ["/bin/true"]);
    let root = images.rootfs(&unpacked).unwrap();
    assert_eq!(
        std::fs::read_dir(images.roots().open(&root).unwrap().path())
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn invalid_oci_config_pull_publishes_no_image() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let reference: Reference = "example.test/invalid-config:v1".parse().unwrap();
    assert!(
        images
            .pull(&invalid_config_fixture(), reference.clone(), &Platform::linux_arm64(),)
            .await
            .is_err()
    );
    assert!(images.resolve(&reference).unwrap().is_none());
    assert!(images.leases().list().unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_platform_and_partial_pull_do_not_create_image_records() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(None);
    let reference: Reference = "example.test/app:v1".parse().unwrap();
    let images = Images::open(temp.path()).unwrap();
    assert!(matches!(
        images.pull(&source, reference.clone(), &Platform::linux_amd64()).await,
        Err(Error::UnsupportedPlatform { .. })
    ));
    assert!(images.metadata().get(&reference).unwrap().is_none());

    let mut missing = (*source.blobs).clone();
    let last = missing
        .keys()
        .find(|digest| **digest != source.root.digest().to_string())
        .unwrap()
        .clone();
    missing.remove(&last);
    let partial = MemorySource {
        root: source.root,
        blobs: Arc::new(missing),
    };
    assert!(
        images
            .pull(&partial, reference.clone(), &Platform::linux_arm64())
            .await
            .is_err()
    );
    assert!(images.metadata().get(&reference).unwrap().is_none());
    assert!(images.gc().unwrap().content_removed > 0);
    assert!(images.leases().list().unwrap().is_empty());
}

#[tokio::test]
async fn failed_replacement_pull_preserves_previous_image_and_rootfs() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(None);
    let reference: Reference = "example.test/stable:v1".parse().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let previous = images
        .pull(&source, reference.clone(), &Platform::linux_arm64())
        .await
        .unwrap();
    let (replacement, _) = fixture(Some(format!("sha256:{}", "0".repeat(64))));
    let mut incomplete = (*replacement.blobs).clone();
    let missing = incomplete
        .keys()
        .find(|digest| **digest != replacement.root.digest().to_string() && !source.blobs.contains_key(*digest))
        .unwrap()
        .clone();
    incomplete.remove(&missing);
    let broken = MemorySource {
        root: replacement.root,
        blobs: Arc::new(incomplete),
    };
    assert!(
        images
            .pull(&broken, reference.clone(), &Platform::linux_arm64())
            .await
            .is_err()
    );
    let retained = images.resolve(&reference).unwrap().unwrap();
    assert_eq!(retained.target, previous.target);
    let unpacked = images.unpack(&retained, &Platform::linux_arm64()).unwrap();
    let root = images.rootfs(&unpacked).unwrap();
    assert_eq!(
        std::fs::read_to_string(images.roots().open(&root).unwrap().path().join("etc/release")).unwrap(),
        "husklet\n"
    );
}

#[tokio::test]
async fn unpack_rejects_diff_id_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(Some(format!("sha256:{}", "0".repeat(64))));
    let images = Images::open(temp.path()).unwrap();
    let image = images
        .pull(
            &source,
            "example.test/app:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    assert!(matches!(
        images.unpack(&image, &Platform::linux_arm64()),
        Err(Error::DiffIdMismatch { .. })
    ));
}

#[tokio::test]
async fn streamed_fetch_rejects_overrun_and_truncation_without_visibility() {
    for kind in [Broken::Oversized, Broken::Truncated] {
        let temp = tempfile::tempdir().unwrap();
        let bytes = Bytes::from_static(b"manifest");
        let root = descriptor("application/vnd.oci.image.manifest.v1+json", &bytes);
        let digest: Digest = root.digest().to_string().parse().unwrap();
        let source = BrokenSource { root, bytes, kind };
        let images = Images::open(temp.path()).unwrap();
        assert!(matches!(
            images
                .pull(
                    &source,
                    "example.test/broken:v1".parse().unwrap(),
                    &Platform::linux_arm64()
                )
                .await,
            Err(Error::SizeMismatch { .. })
        ));
        assert!(!images.content().contains(&digest).unwrap());
    }
}

#[tokio::test]
async fn streamed_fetch_rejects_same_size_digest_corruption_without_visibility() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = Bytes::from_static(b"manifest");
    let root = descriptor("application/vnd.oci.image.manifest.v1+json", &bytes);
    let digest: Digest = root.digest().to_string().parse().unwrap();
    let source = BrokenSource {
        root,
        bytes,
        kind: Broken::Corrupt,
    };
    let images = Images::open(temp.path()).unwrap();
    assert!(matches!(
        images
            .pull(
                &source,
                "example.test/corrupt:v1".parse().unwrap(),
                &Platform::linux_arm64()
            )
            .await,
        Err(Error::DigestMismatch { .. })
    ));
    assert!(!images.content().contains(&digest).unwrap());
    assert!(images.metadata().list().unwrap().is_empty());
}
