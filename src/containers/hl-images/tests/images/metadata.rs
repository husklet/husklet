use super::support::fixture;
use hl_images::{content::Store, Digest, Images, Platform, Reference};
use std::collections::BTreeSet;

#[tokio::test]
async fn local_tag_is_metadata_only_and_content_remains_shared() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(None);
    let images = Images::open(temp.path()).unwrap();
    let image = images
        .pull(
            &source,
            "example.test/app:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let alias: Reference = "example.test/app:stable".parse().unwrap();
    let tagged = images.tag(&image, alias.clone()).unwrap();
    assert_eq!(tagged.target.digest(), image.target.digest());
    assert!(images.resolve(&alias).unwrap().is_some());
    images.remove(&alias).unwrap();
    assert!(images.resolve(&alias).unwrap().is_none());
    assert!(images.resolve(&image.name).unwrap().is_some());
}

#[tokio::test]
async fn forced_removal_untags_every_alias_but_preserves_other_targets() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(None);
    let images = Images::open(temp.path()).unwrap();
    let image = images
        .pull(
            &source,
            "example.test/forced:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let alias: Reference = "example.test/forced:stable".parse().unwrap();
    images.tag(&image, alias.clone()).unwrap();

    let mut removed = images
        .force_remove(&image)
        .unwrap()
        .into_iter()
        .map(|image| image.name.to_string())
        .collect::<Vec<_>>();
    removed.sort();
    assert_eq!(
        removed,
        vec![
            "example.test/forced:stable".to_string(),
            "example.test/forced:v1".to_string(),
        ]
    );
    assert!(images.resolve(&image.name).unwrap().is_none());
    assert!(images.resolve(&alias).unwrap().is_none());
    let digest: Digest = image.target.digest().to_string().parse().unwrap();
    assert!(images.content().contains(&digest).unwrap());
}

#[tokio::test]
async fn graph_catalog_survives_untagging_and_targeted_gc_is_exact() {
    let temp = tempfile::tempdir().unwrap();
    let (source, _) = fixture(None);
    let images = Images::open(temp.path()).unwrap();
    let image = images
        .pull(
            &source,
            "example.test/prune:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let descriptors =
        hl_images::DescriptorGraph::walk(image.target.clone(), images.content()).unwrap();
    let expected_count = descriptors.len() as u64;
    let expected = descriptors
        .into_iter()
        .map(|descriptor| descriptor.size())
        .sum::<u64>();
    let digest = image.target.digest().to_string();
    let graph = images
        .metadata()
        .graphs()
        .unwrap()
        .into_iter()
        .find(|graph| graph.target.digest() == image.target.digest())
        .unwrap();
    assert!(graph.filterable());
    images.remove(&image.name).unwrap();
    drop(images);

    let reopened = Images::open(temp.path()).unwrap();
    let graph = reopened
        .metadata()
        .graphs()
        .unwrap()
        .into_iter()
        .find(|graph| graph.target.digest().to_string() == digest)
        .unwrap();
    assert!(graph.names.is_empty());
    let report = reopened
        .prune_graphs(&[digest].into_iter().collect())
        .unwrap();
    assert_eq!(report.content_bytes_removed, expected);
    assert_eq!(report.content_removed, expected_count);
    assert!(
        reopened
            .prune_graphs(&BTreeSet::default())
            .unwrap()
            .content_bytes_removed
            == 0
    );
}
