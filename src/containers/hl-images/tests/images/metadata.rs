use super::support::{FaultPersistence, TearingPersistence, fixture, scratch_fixture};
use hl_images::{Digest, Images, Platform, Reference, content::Store};
use std::{collections::BTreeSet, sync::Arc};

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
async fn retag_moves_one_name_between_graphs_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let (source, _) = fixture(None);
    let first = images
        .pull(
            &source,
            "example.test/source:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let second = images
        .pull(
            &scratch_fixture(),
            "example.test/moved:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();

    images.tag(&first, second.name.clone()).unwrap();

    assert_eq!(images.resolve(&second.name).unwrap().unwrap().target, first.target);
    let graphs = images.graphs().unwrap();
    let old = graphs
        .iter()
        .find(|graph| graph.target.digest() == second.target.digest())
        .unwrap();
    assert!(!old.names.contains(&second.name.to_string()));
    let new = graphs
        .iter()
        .find(|graph| graph.target.digest() == first.target.digest())
        .unwrap();
    assert!(new.names.contains(&second.name.to_string()));
}

#[tokio::test]
async fn immutable_image_id_is_selected_config_digest_across_retag_and_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let (source, manifest) = fixture(None);
    let platform = Platform::linux_arm64();
    let image = images
        .pull(&source, "example.test/identity:v1".parse().unwrap(), &platform)
        .await
        .unwrap();
    let manifest_bytes = source.blobs.get(&manifest.digest().to_string()).unwrap();
    let document: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let expected: Digest = document["config"]["digest"].as_str().unwrap().parse().unwrap();

    assert_eq!(images.image_id(&image, &platform).unwrap(), expected);
    assert_ne!(
        images.image_id(&image, &platform).unwrap().to_string(),
        image.target.digest().to_string()
    );

    let alias: Reference = "example.test/identity:stable".parse().unwrap();
    let tagged = images.tag(&image, alias.clone()).unwrap();
    assert_eq!(images.image_id(&tagged, &platform).unwrap(), expected);
    drop(images);

    let reopened = Images::open(temp.path()).unwrap();
    let resolved = reopened.resolve(&alias).unwrap().unwrap();
    assert_eq!(reopened.image_id(&resolved, &platform).unwrap(), expected);
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
async fn forced_removal_publishes_all_alias_changes_or_none() {
    let temp = tempfile::tempdir().unwrap();
    let persistence = Arc::new(FaultPersistence::default());
    let images = Images::open_with(temp.path(), persistence.clone()).unwrap();
    let (source, _) = fixture(None);
    let image = images
        .pull(
            &source,
            "example.test/atomic:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let alias: Reference = "example.test/atomic:stable".parse().unwrap();
    images.tag(&image, alias.clone()).unwrap();

    persistence.fail_metadata_in(1);
    assert!(images.force_remove(&image).is_err());
    assert!(images.resolve(&image.name).unwrap().is_some());
    assert!(images.resolve(&alias).unwrap().is_some());
}

#[tokio::test]
async fn opening_the_store_never_observes_a_partially_published_catalog() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let persistence = Arc::new(TearingPersistence::new());
    let images = Images::open_with(&root, persistence.clone()).unwrap();
    let (source, _) = fixture(None);
    let image = images
        .pull(
            &source,
            "example.test/torn:v1".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();

    let torn = persistence.tear_next();
    let alias: Reference = "example.test/torn:stable".parse().unwrap();
    let writer = std::thread::spawn(move || images.tag(&image, alias).unwrap());
    torn.recv().unwrap();
    let reopened = Images::open(&root).expect("open observed a partially published catalog");
    writer.join().unwrap();
    assert!(
        reopened
            .resolve(&"example.test/torn:v1".parse().unwrap())
            .unwrap()
            .is_some()
    );
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
    let descriptors = hl_images::DescriptorGraph::walk(image.target.clone(), images.content()).unwrap();
    let expected_count = descriptors.len() as u64;
    let expected = descriptors.into_iter().map(|descriptor| descriptor.size()).sum::<u64>();
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
    let report = reopened.prune_graphs(&[digest].into_iter().collect()).unwrap();
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
