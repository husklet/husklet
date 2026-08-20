use super::{
    Blob, ConfigDocument, History, Images, IndexDocument, MediaType, Metadata, RuntimeConfig, RuntimeOverrides,
};
use crate::{ImageStore, LeaseStore, Platform, Reference};

fn layer(path: &str, bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut archive = tar::Builder::new(&mut result);
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes).unwrap();
    archive.finish().unwrap();
    drop(archive);
    result
}

#[test]
fn commit_metadata_applies_changes_and_attribution_to_standalone_base() {
    let runtime = RuntimeConfig {
        entrypoint: Vec::new(),
        command: vec!["old".into()],
        environment: std::collections::BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let metadata = Metadata::standalone(Platform::linux_arm64(), runtime.clone())
        .committed(
            runtime,
            Some("Ada".into()),
            Some("snapshot".into()),
            &["CMD [\"new\"]".into(), "LABEL stage=committed".into()],
        )
        .unwrap();
    assert_eq!(metadata.author.as_deref(), Some("Ada"));
    assert_eq!(metadata.runtime.command, ["new"]);
    assert_eq!(metadata.labels.get("stage").map(String::as_str), Some("committed"));
    assert_eq!(metadata.history.last().unwrap().comment.as_deref(), Some("snapshot"));
}

#[test]
fn child_commit_reuses_parent_descriptors_and_appends_one_diff() {
    let root = tempfile::tempdir().unwrap();
    let images = Images::open(root.path()).unwrap();
    let parent_name: Reference = "example/parent:latest".parse().unwrap();
    let platform = Platform::linux_arm64();
    let runtime = RuntimeConfig {
        entrypoint: Vec::new(),
        command: Vec::new(),
        environment: std::collections::BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let parent = images
        .commit(&layer("lower", b"lower"), &runtime, &platform, &parent_name)
        .unwrap();
    let mut metadata = images.details(&parent, &platform).unwrap();
    metadata.created = Some("1970-01-01T00:00:01.250Z".into());
    metadata.author = Some("Ada".into());
    metadata.history.push(History {
        created_by: Some("commit child".into()),
        comment: Some("snapshot".into()),
        ..History::default()
    });
    let child_name: Reference = "example/child:latest".parse().unwrap();
    let child = images
        .commit_child(
            &parent,
            std::io::Cursor::new(layer("upper", b"upper")),
            &child_name,
            &metadata,
        )
        .unwrap();

    let parent_manifest: super::ManifestDocument =
        serde_json::from_slice(&images.content.read_document(&parent.target).unwrap()).unwrap();
    let child_manifest: super::ManifestDocument =
        serde_json::from_slice(&images.content.read_document(&child.target).unwrap()).unwrap();
    assert_eq!(child_manifest.layers.len(), parent_manifest.layers.len() + 1);
    assert_eq!(
        child_manifest.layers[..parent_manifest.layers.len()],
        parent_manifest.layers
    );
    let child_config: super::ConfigDocument =
        serde_json::from_slice(&images.content.read_document(&child_manifest.config).unwrap()).unwrap();
    assert_eq!(child_config.rootfs.diff_ids.len(), child_manifest.layers.len());
    assert_eq!(images.metadata().list().unwrap().len(), 2);
    let graph = images
        .metadata()
        .graphs()
        .unwrap()
        .into_iter()
        .find(|graph| graph.target.digest() == child.target.digest())
        .unwrap();
    assert_eq!(graph.created_at_ms, Some(1_250));
    assert!(images.leases.list().unwrap().is_empty());
    let details = images.details(&child, &platform).unwrap();
    assert_eq!(details.author.as_deref(), Some("Ada"));
    assert_eq!(details.history.last().unwrap().comment.as_deref(), Some("snapshot"));
}

#[test]
fn workspace_catalog_resolves_local_first_and_keeps_builds_local() {
    let external_root = tempfile::tempdir().unwrap();
    let workspace_root = tempfile::tempdir().unwrap();
    let external = Images::open(external_root.path()).unwrap();
    let platform = Platform::linux_arm64();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/bin/sh".into()],
        command: Vec::new(),
        environment: std::collections::BTreeMap::new(),
        working_directory: "/".into(),
        user: "0:0".into(),
    };
    let external_name: Reference = "library/postgres:latest".parse().unwrap();
    let external_image = external
        .commit(&layer("postgres", b"external"), &runtime, &platform, &external_name)
        .unwrap();
    let images = Images::workspace(Images::open(workspace_root.path()).unwrap(), external.clone());

    assert_eq!(
        images.resolve(&external_name).unwrap().unwrap().target.digest(),
        external_image.target.digest()
    );
    let unpacked = images.unpack(&external_image, &platform).unwrap();
    assert!(images.rootfs(&unpacked).is_ok());

    let built_name: Reference = "project/app:dev".parse().unwrap();
    let built = images
        .commit(&layer("app", b"workspace"), &runtime, &platform, &built_name)
        .unwrap();
    assert!(
        Images::open(workspace_root.path())
            .unwrap()
            .resolve(&built_name)
            .unwrap()
            .is_some()
    );
    assert!(external.resolve(&built_name).unwrap().is_none());
    assert_eq!(
        images
            .list()
            .unwrap()
            .into_iter()
            .map(|image| image.name.to_string())
            .collect::<Vec<_>>(),
        [built.name.to_string(), external_name.to_string()]
    );
}

#[tokio::test]
async fn workspace_pull_publishes_external_catalog_only() {
    let source_root = tempfile::tempdir().unwrap();
    let layout_root = tempfile::tempdir().unwrap();
    let external_root = tempfile::tempdir().unwrap();
    let workspace_root = tempfile::tempdir().unwrap();
    let source = Images::open(source_root.path()).unwrap();
    let platform = Platform::linux_arm64();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/bin/sh".into()],
        command: Vec::new(),
        environment: std::collections::BTreeMap::new(),
        working_directory: "/".into(),
        user: "0:0".into(),
    };
    let name: Reference = "library/ubuntu:latest".parse().unwrap();
    let image = source
        .commit(&layer("ubuntu", b"external"), &runtime, &platform, &name)
        .unwrap();
    let layout = crate::format::layout::Layout::open(layout_root.path()).await.unwrap();
    layout.export(&image, source.content()).await.unwrap();

    let external = Images::open(external_root.path()).unwrap();
    let images = Images::workspace(Images::open(workspace_root.path()).unwrap(), external.clone());
    images.pull(&layout, name.clone(), &platform).await.unwrap();

    assert!(external.resolve(&name).unwrap().is_some());
    assert!(
        Images::open(workspace_root.path())
            .unwrap()
            .resolve(&name)
            .unwrap()
            .is_none()
    );
    let resolved = images.resolve(&name).unwrap().unwrap();
    assert!(images.unpack(&resolved, &platform).is_ok());
}

#[test]
fn blob_descriptor_matches_exact_bytes_and_digest() {
    let bytes = br#"{"schemaVersion":2}"#;
    let descriptor = Blob::new(bytes, MediaType::ImageManifest).descriptor().unwrap();
    assert_eq!(descriptor.size(), bytes.len() as u64);
    assert_eq!(
        descriptor.digest().to_string(),
        crate::Digest::sha256(bytes).to_string()
    );
}

#[test]
fn config_platform_must_match_the_selected_guest() {
    let config: ConfigDocument = serde_json::from_value(serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "rootfs": { "type": "layers", "diff_ids": [] }
    }))
    .unwrap();
    assert!(config.require_platform(&Platform::linux_amd64()).is_ok());
    assert!(config.require_platform(&Platform::linux_arm64()).is_err());
}

#[test]
fn platform_refusal_names_the_requested_platform_before_the_image_s_own() {
    let config: ConfigDocument = serde_json::from_value(serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "rootfs": { "type": "layers", "diff_ids": [] }
    }))
    .unwrap();
    // The requested platform comes first, because a single-platform image refused for a daemon on
    // another architecture used to report only the image's own -- `no manifest for platform
    // linux/amd64` for an amd64 image, which reads as though amd64 were the platform missing.
    assert_eq!(
        config
            .require_platform(&Platform::linux_arm64())
            .unwrap_err()
            .to_string(),
        "no manifest for platform linux/arm64; the image is linux/amd64"
    );
    let index: IndexDocument = serde_json::from_value(serde_json::json!({
        "schemaVersion": 2,
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{}", "1".repeat(64)),
            "size": 1,
            "platform": {"os": "linux", "architecture": "arm64"}
        }]
    }))
    .unwrap();
    // An index carries many platforms, so there is no single "the image is" to name.
    assert_eq!(
        index.select_platform(&Platform::linux_amd64()).unwrap_err().to_string(),
        "no manifest for platform linux/amd64"
    );
    assert_eq!(
        index
            .select_platform(&Platform::new("linux", "riscv64", Some("v1".into())))
            .unwrap_err()
            .to_string(),
        "no manifest for platform linux/riscv64/v1"
    );
}

#[test]
fn platform_selection_honors_requested_variant() {
    let index: IndexDocument = serde_json::from_value(serde_json::json!({
        "schemaVersion": 2,
        "manifests": [
            {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": format!("sha256:{}", "1".repeat(64)),
                "size": 1,
                "platform": {"os": "linux", "architecture": "arm64", "variant": "v8"}
            },
            {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": format!("sha256:{}", "2".repeat(64)),
                "size": 1,
                "platform": {"os": "linux", "architecture": "arm64", "variant": "v9"}
            }
        ]
    }))
    .unwrap();
    let selected = index
        .select_platform(&Platform::new("linux", "arm64", Some("v9".into())))
        .unwrap();
    assert_eq!(selected.digest().to_string(), format!("sha256:{}", "2".repeat(64)));
    assert!(
        index
            .select_platform(&Platform::new("linux", "arm64", Some("v7".into())))
            .is_err()
    );
}

#[test]
fn platform_selection_rejects_schema_version_one() {
    let index: IndexDocument = serde_json::from_value(serde_json::json!({
        "schemaVersion": 1,
        "manifests": []
    }))
    .unwrap();
    let error = index.select_platform(&Platform::linux_arm64()).unwrap_err();
    assert!(error.to_string().contains("schema version 1"));
}

#[test]
fn nullable_runtime_sequences_use_empty_oci_defaults() {
    let config: ConfigDocument = serde_json::from_value(serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "rootfs": { "type": "layers", "diff_ids": [] },
        "config": {
            "Entrypoint": null,
            "Cmd": null,
            "Env": null,
            "Labels": null,
            "OnBuild": null,
            "ExposedPorts": null,
            "Volumes": null
        }
    }))
    .unwrap();
    assert!(config.config.on_build.is_empty());
    assert!(config.config.exposed_ports.is_empty());
    assert!(config.config.volumes.is_empty());
    let runtime = super::RuntimeConfig::try_from(config.config).unwrap();
    assert!(runtime.entrypoint.is_empty());
    assert!(runtime.command.is_empty());
    assert!(runtime.environment.is_empty());
}

#[test]
fn nullable_config_and_history_use_empty_oci_defaults() {
    let config: ConfigDocument = serde_json::from_value(serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "history": null,
        "rootfs": { "type": "layers", "diff_ids": [] },
        "config": null
    }))
    .unwrap();
    assert!(config.history.is_empty());
    let runtime = super::RuntimeConfig::try_from(config.config).unwrap();
    assert!(runtime.argv().is_empty());
    assert!(runtime.environment.is_empty());
}

#[test]
fn malformed_runtime_sequences_are_rejected() {
    let config = serde_json::from_value::<ConfigDocument>(serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "rootfs": { "type": "layers", "diff_ids": [] },
        "config": { "Entrypoint": "/bin/sh" }
    }));
    assert!(config.is_err());
}

fn runtime_config(value: serde_json::Value) -> ConfigDocument {
    let mut object = match value {
        serde_json::Value::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    object.insert("architecture".into(), serde_json::json!("arm64"));
    object.insert("os".into(), serde_json::json!("linux"));
    object.insert("rootfs".into(), serde_json::json!({"type":"layers","diff_ids":[]}));
    serde_json::from_value(object.into()).unwrap()
}

#[test]
fn config_exposed_ports_sorted_keys() {
    let config = runtime_config(serde_json::json!({"config":{"ExposedPorts":{"80/tcp":{},"5432/tcp":{}}}}));
    assert_eq!(
        config
            .config
            .exposed_ports
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["5432/tcp", "80/tcp"]
    );
}

#[test]
fn config_labels_map() {
    let config = runtime_config(serde_json::json!({"config":{"Labels":{"a":"b","maintainer":"acme"}}}));
    let labels = config.config.labels.unwrap();
    assert_eq!(labels.get("a").map(String::as_str), Some("b"));
    assert_eq!(labels.get("maintainer").map(String::as_str), Some("acme"));
}

#[test]
fn config_volumes_sorted_keys() {
    let config = runtime_config(serde_json::json!({"config":{"Volumes":{"/var":{},"/data":{}}}}));
    assert_eq!(
        config.config.volumes.keys().map(String::as_str).collect::<Vec<_>>(),
        ["/data", "/var"]
    );
}

#[test]
fn config_stop_signal_str() {
    let config = runtime_config(serde_json::json!({"config":{"StopSignal":"SIGQUIT"}}));
    assert_eq!(config.config.stop_signal.as_deref(), Some("SIGQUIT"));
    assert!(
        runtime_config(serde_json::json!({"config":{}}))
            .config
            .stop_signal
            .is_none()
    );
}

#[test]
fn config_strs_array() {
    let config = runtime_config(serde_json::json!({"config":{"Env":["A=1","B=2"]}}));
    let runtime = RuntimeConfig::try_from(config.config).unwrap();
    assert_eq!(runtime.environment.get("A").map(String::as_str), Some("1"));
    assert_eq!(runtime.environment.get("B").map(String::as_str), Some("2"));
    assert!(runtime.command.is_empty());
}

fn launch(
    entrypoint: &[&str],
    command: &[&str],
    override_entrypoint: Option<&[&str]>,
    override_command: Option<&[&str]>,
) -> Vec<String> {
    RuntimeConfig {
        entrypoint: entrypoint.iter().map(|value| (*value).into()).collect(),
        command: command.iter().map(|value| (*value).into()).collect(),
        environment: std::collections::BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    }
    .merge(RuntimeOverrides {
        entrypoint: override_entrypoint.map(|values| values.iter().map(|value| (*value).into()).collect()),
        command: override_command.map(|values| values.iter().map(|value| (*value).into()).collect()),
        ..RuntimeOverrides::default()
    })
    .unwrap()
    .argv()
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[test]
fn entrypoint_plus_image_cmd_when_no_override() {
    assert_eq!(
        launch(&["/entry"], &["arg1", "arg2"], None, None),
        ["/entry", "arg1", "arg2"]
    );
}

#[test]
fn entrypoint_plus_override_drops_image_cmd() {
    assert_eq!(
        launch(&["/entry"], &["image"], None, Some(&["run", "now"]),),
        ["/entry", "run", "now"]
    );
}

#[test]
fn override_alone_when_no_entrypoint() {
    assert_eq!(launch(&[], &["image"], None, Some(&["only"])), ["only"]);
}

#[test]
fn image_cmd_alone_when_no_entrypoint_no_override() {
    assert_eq!(launch(&[], &["default", "cmd"], None, None), ["default", "cmd"]);
    assert_eq!(launch(&[], &["default", "cmd"], None, Some(&[])), ["default", "cmd"]);
}

#[test]
fn explicit_entrypoint_obeys_docker_command_merge_rules() {
    assert_eq!(
        launch(&["/image-entry"], &["image-command"], Some(&["/user-entry"]), None,),
        ["/user-entry"]
    );
    assert_eq!(
        launch(
            &["/image-entry"],
            &["image-command"],
            Some(&["/user-entry"]),
            Some(&["user-command"]),
        ),
        ["/user-entry", "user-command"]
    );
    assert_eq!(
        launch(&["/image-entry"], &["image-command"], Some(&[]), None,),
        ["image-command"]
    );
}

#[test]
fn materialize_publishes_and_forks_the_chain_in_one_operation() {
    let root = tempfile::tempdir().unwrap();
    let platform = Platform::linux_arm64();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/bin/sh".into()],
        command: Vec::new(),
        environment: std::collections::BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let name: Reference = "library/alpine:3.20".parse().unwrap();
    let images = Images::open(root.path()).unwrap();
    let image = images
        .commit(&layer("bin/sh", b"shell"), &runtime, &platform, &name)
        .unwrap();

    let (unpacked, reference) = images.materialize(&image, &platform).unwrap();
    let view = images.roots().open(&reference).unwrap();
    assert_eq!(std::fs::read(view.path().join("bin/sh")).unwrap(), b"shell");
    assert!(images.pinned(unpacked.snapshot()).unwrap());
    images.roots().release(&reference).unwrap();
}

#[test]
fn concurrent_workers_materialize_one_image_without_losing_the_chain() {
    const WORKERS: usize = 6;
    let root = tempfile::tempdir().unwrap();
    let platform = Platform::linux_arm64();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/bin/sh".into()],
        command: Vec::new(),
        environment: std::collections::BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let name: Reference = "library/alpine:3.20".parse().unwrap();
    let image = Images::open(root.path())
        .unwrap()
        .commit(&layer("bin/sh", b"shell"), &runtime, &platform, &name)
        .unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(WORKERS * 2));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let failures = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = |failures: &std::sync::Mutex<Vec<String>>, message: String| {
        failures.lock().unwrap().push(message);
    };
    std::thread::scope(|scope| {
        // Every worker reopens the shared store while others are mid-chain, so store
        // recovery races the publication and fork of the snapshot they all depend on.
        for _ in 0..WORKERS {
            let (barrier, done, failures, root) =
                (barrier.clone(), done.clone(), failures.clone(), root.path().to_owned());
            scope.spawn(move || {
                barrier.wait();
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Err(error) = Images::open(&root) {
                        record(&failures, format!("open image cache: {error}"));
                    }
                }
            });
        }
        let mut workers = Vec::new();
        for _ in 0..WORKERS {
            let (barrier, failures, image, platform, root) = (
                barrier.clone(),
                failures.clone(),
                image.clone(),
                platform.clone(),
                root.path().to_owned(),
            );
            workers.push(scope.spawn(move || {
                barrier.wait();
                for _ in 0..60 {
                    let images = match Images::open(&root) {
                        Ok(images) => images,
                        Err(error) => {
                            record(&failures, format!("open image cache: {error}"));
                            continue;
                        }
                    };
                    let unpacked = match images.unpack(&image, &platform) {
                        Ok(unpacked) => unpacked,
                        Err(error) => {
                            record(&failures, format!("unpack: {error}"));
                            continue;
                        }
                    };
                    match images.rootfs(&unpacked) {
                        Ok(reference) => {
                            if let Err(error) = images.roots().open(&reference) {
                                record(&failures, format!("open rootfs: {error}"));
                            }
                            if let Err(error) = images.roots().release(&reference) {
                                record(&failures, format!("release rootfs: {error}"));
                            }
                        }
                        Err(error) => record(&failures, format!("fork rootfs: {error}")),
                    }
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let failures = failures.lock().unwrap();
    assert!(failures.is_empty(), "{failures:#?}");
}
