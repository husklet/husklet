use super::support::fixture;
use hl_images::{Digest, Error, Images, Platform, Reference, format::layout::Layout};

#[tokio::test]
async fn oci_layout_export_and_import_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(None);
    let images = Images::open(temp.path().join("source")).unwrap();
    let reference: Reference = "example.test/app:v1".parse().unwrap();
    let image = images
        .pull(&source, reference.clone(), &Platform::linux_arm64())
        .await
        .unwrap();
    let layout = Layout::open(temp.path().join("layout")).await.unwrap();
    let report = layout.export(&image, images.content()).await.unwrap();
    assert_eq!(report.copied, 4);
    assert!(temp.path().join("layout/oci-layout").is_file());

    let imported = Images::open(temp.path().join("imported")).unwrap();
    let copy = imported
        .pull(&layout, reference, &Platform::linux_arm64())
        .await
        .unwrap();
    assert_eq!(copy.target.digest(), image.target.digest());
    let unpacked = imported.unpack(&copy, &Platform::linux_arm64()).unwrap();
    let root = imported.rootfs(&unpacked).unwrap();
    let handle = imported.roots().open(&root).unwrap();
    assert_eq!(
        std::fs::read_to_string(handle.path().join("etc/release")).unwrap(),
        "husklet\n"
    );
}

#[tokio::test]
async fn oci_layout_never_follows_blob_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(None);
    let images = Images::open(temp.path().join("source")).unwrap();
    let reference: Reference = "example.test/app:v1".parse().unwrap();
    let image = images
        .pull(&source, reference.clone(), &Platform::linux_arm64())
        .await
        .unwrap();
    let layout = Layout::open(temp.path().join("layout")).await.unwrap();
    layout.export(&image, images.content()).await.unwrap();
    let digest: Digest = image.target.digest().to_string().parse().unwrap();
    let blob = temp.path().join("layout/blobs/sha256").join(digest.encoded());
    std::fs::remove_file(&blob).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/passwd", &blob).unwrap();
    let imported = Images::open(temp.path().join("imported")).unwrap();
    assert!(matches!(
        imported.pull(&layout, reference, &Platform::linux_arm64()).await,
        Err(Error::InvalidMetadata(_))
    ));
}
