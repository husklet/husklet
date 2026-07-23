use std::collections::HashMap;

use hl_images::{Descriptor, DescriptorGraph, Digest};

fn descriptor(name: &[u8]) -> Descriptor {
    serde_json::from_value(serde_json::json!({"mediaType":"application/octet-stream", "digest":Digest::sha256(name).to_string(), "size":name.len()})).unwrap()
}

#[test]
fn graph_walk_is_parent_first_and_suppresses_cycles_and_duplicates() {
    let root = descriptor(b"root");
    let manifest = descriptor(b"manifest");
    let layer = descriptor(b"layer");
    let edges = HashMap::from([
        (
            root.digest().to_string(),
            vec![manifest.clone(), layer.clone()],
        ),
        (
            manifest.digest().to_string(),
            vec![layer.clone(), root.clone()],
        ),
    ]);
    let walked = DescriptorGraph::from_edges(root.clone(), edges).unwrap();
    assert_eq!(
        walked
            .iter()
            .map(|d| d.digest().to_string())
            .collect::<Vec<_>>(),
        vec![root, manifest, layer]
            .into_iter()
            .map(|d| d.digest().to_string())
            .collect::<Vec<_>>()
    );
}
