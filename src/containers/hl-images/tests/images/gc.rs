use super::support::{fixture, tar_file, FaultPersistence};
use hl_images::{
    content::Store, Descriptor, Digest, History, Images, Metadata, Platform, RuntimeConfig,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[tokio::test]
async fn failed_metadata_stage_preserves_graph_and_retry_reclaims_it() {
    let temp = tempfile::tempdir().unwrap();
    let persistence = Arc::new(FaultPersistence::default());
    let images = Images::open_with(temp.path(), persistence.clone()).unwrap();
    let (source, _) = fixture(None);
    let image = images
        .pull(
            &source,
            "example.test/fault:metadata".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let graph = hl_images::DescriptorGraph::walk(image.target.clone(), images.content()).unwrap();
    let expected = graph.iter().map(Descriptor::size).sum::<u64>();
    images.remove(&image.name).unwrap();
    persistence.fail_metadata_in(1);
    let selected = BTreeSet::from([image.target.digest().to_string()]);

    assert!(images.prune_graphs(&selected).is_err());
    assert!(images
        .metadata()
        .graphs()
        .unwrap()
        .iter()
        .any(|graph| graph.target.digest() == image.target.digest()));
    for descriptor in &graph {
        let digest: Digest = descriptor.digest().to_string().parse().unwrap();
        assert!(images.content().contains(&digest).unwrap());
    }
    assert_eq!(
        images
            .prune_graphs(&selected)
            .unwrap()
            .content_bytes_removed,
        expected
    );
}

#[tokio::test]
async fn failed_blob_delete_leaves_a_durable_retryable_prune() {
    let temp = tempfile::tempdir().unwrap();
    let persistence = Arc::new(FaultPersistence::default());
    let images = Images::open_with(temp.path(), persistence.clone()).unwrap();
    let (source, _) = fixture(None);
    let image = images
        .pull(
            &source,
            "example.test/fault:blob".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let graph = hl_images::DescriptorGraph::walk(image.target.clone(), images.content()).unwrap();
    let expected = graph.iter().map(Descriptor::size).sum::<u64>();
    images.remove(&image.name).unwrap();
    persistence.fail_blob_in(1);
    let selected = BTreeSet::from([image.target.digest().to_string()]);

    assert!(images.prune_graphs(&selected).is_err());
    drop(images);
    let recovered = Images::open_with(temp.path(), persistence).unwrap();
    let report = recovered.prune_graphs(&selected).unwrap();
    assert_eq!(report.content_removed, graph.len() as u64);
    assert_eq!(report.content_bytes_removed, expected);
    assert!(recovered.metadata().graphs().unwrap().is_empty());
    assert_eq!(
        recovered.prune_graphs(&selected).unwrap().content_removed,
        0
    );
}

#[tokio::test]
async fn failed_prune_completion_is_idempotent_after_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let persistence = Arc::new(FaultPersistence::default());
    let images = Images::open_with(temp.path(), persistence.clone()).unwrap();
    let (source, _) = fixture(None);
    let image = images
        .pull(
            &source,
            "example.test/fault:completion".parse().unwrap(),
            &Platform::linux_arm64(),
        )
        .await
        .unwrap();
    let graph = hl_images::DescriptorGraph::walk(image.target.clone(), images.content()).unwrap();
    images.remove(&image.name).unwrap();
    // The first replacement stages the pending transaction; the second fails its completion.
    persistence.fail_metadata_in(2);
    let selected = BTreeSet::from([image.target.digest().to_string()]);
    assert!(images.prune_graphs(&selected).is_err());
    drop(images);

    let recovered = Images::open_with(temp.path(), persistence).unwrap();
    assert_eq!(
        recovered.prune_graphs(&selected).unwrap().content_removed,
        0
    );
    assert!(recovered.metadata().graphs().unwrap().is_empty());
    for descriptor in graph {
        let digest: Digest = descriptor.digest().to_string().parse().unwrap();
        assert!(!recovered.content().contains(&digest).unwrap());
    }
}

#[test]
fn pruning_one_untagged_graph_retains_shared_layers_and_exact_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let layer = tar_file("shared", b"one shared filesystem layer");
    let runtime = RuntimeConfig {
        entrypoint: vec!["/bin/true".into()],
        command: Vec::new(),
        environment: BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let metadata = |label: &str| Metadata {
        author: None,
        platform: Platform::linux_arm64(),
        created: Some("2024-01-01T00:00:00Z".into()),
        labels: BTreeMap::from([("variant".into(), label.into())]),
        history: vec![History::default()],
        runtime: runtime.clone(),
        onbuild: Vec::new(),
        exposed_ports: BTreeSet::new(),
        volumes: BTreeSet::new(),
        healthcheck: None,
        stop_signal: None,
    };
    let first = images
        .build(
            &layer,
            &"example.test/shared:first".parse().unwrap(),
            &metadata("first"),
        )
        .unwrap();
    let second = images
        .build(
            &layer,
            &"example.test/shared:second".parse().unwrap(),
            &metadata("second"),
        )
        .unwrap();
    let first_graph = hl_images::DescriptorGraph::walk(first.target.clone(), images.content())
        .unwrap()
        .into_iter()
        .map(|descriptor| (descriptor.digest().to_string(), descriptor.size()))
        .collect::<BTreeMap<_, _>>();
    let second_graph = hl_images::DescriptorGraph::walk(second.target.clone(), images.content())
        .unwrap()
        .into_iter()
        .map(|descriptor| descriptor.digest().to_string())
        .collect::<BTreeSet<_>>();
    let expected = first_graph
        .iter()
        .filter(|(digest, _)| !second_graph.contains(*digest))
        .map(|(_, size)| size)
        .sum::<u64>();
    let expected_count = first_graph
        .keys()
        .filter(|digest| !second_graph.contains(*digest))
        .count() as u64;
    images.remove(&first.name).unwrap();
    images.remove(&second.name).unwrap();

    let report = images
        .prune_graphs(&BTreeSet::from([first.target.digest().to_string()]))
        .unwrap();
    assert_eq!(report.content_bytes_removed, expected);
    assert_eq!(report.content_removed, expected_count);
    assert_eq!(
        report.content_kept,
        u64::try_from(first_graph.len()).unwrap() - expected_count
    );
    assert_eq!(
        images.size(&second).unwrap(),
        second_graph
            .iter()
            .map(|digest| {
                let digest: Digest = digest.parse().unwrap();
                images.content().info(&digest).unwrap().size
            })
            .sum::<u64>()
    );
}
