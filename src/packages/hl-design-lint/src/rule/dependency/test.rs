use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{Workspace, rule::Rule};

use super::Direction;

fn fixture(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-design-dependencies-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"src/*/*\"]\n",
    )
    .unwrap();
    root
}

fn package(root: &Path, layer: &str, name: &str, dependencies: &str) {
    let directory = root.join("src").join(layer).join(name);
    fs::create_dir_all(directory.join("src")).unwrap();
    fs::write(
        directory.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n{dependencies}"),
    )
    .unwrap();
    fs::write(directory.join("src/lib.rs"), "").unwrap();
}

fn findings(root: &Path) -> Vec<crate::Finding> {
    let workspace = Workspace::load([root.join("src")]).unwrap();
    Direction.check(&workspace).unwrap()
}

#[test]
fn detects_renamed_packages() {
    let root = fixture("tables");
    package(&root, "runtime", "engine-state", "");
    package(&root, "runtime", "sessions", "");
    package(&root, "app", "hl-engine", "");
    package(
        &root,
        "packages",
        "foundation",
        r#"
[dependencies]
state_alias = { package = "engine-state", path = "../../runtime/engine-state" }

[build-dependencies]
sessions = { path = "../../runtime/sessions" }

[target.'cfg(target_os = "macos")'.dev-dependencies]
product = { package = "hl-engine", path = "../../app/hl-engine" }
"#,
    );

    let values = findings(&root);
    assert_eq!(values.len(), 3);
    assert!(
        values
            .iter()
            .any(|finding| finding.subject == "foundation -> engine-state")
    );
    assert!(values.iter().any(|finding| finding.subject == "foundation -> sessions"));
    let target = values
        .iter()
        .find(|finding| finding.subject == "foundation -> hl-engine")
        .unwrap();
    let review = target.review.as_ref().unwrap();
    assert!(
        review
            .metadata
            .contains(&("Dependency kind".into(), "development".into()))
    );
    assert!(
        review
            .metadata
            .iter()
            .any(|(key, value)| { key == "Target condition" && value == "cfg(target_os = \"macos\")" })
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resolves_renamed_dependencies() {
    let root = fixture("workspace");
    fs::write(
        root.join("Cargo.toml"),
        r#"
[workspace]
resolver = "2"
members = ["src/*/*"]

[workspace.dependencies]
state_alias = { package = "engine-state", path = "src/runtime/engine-state" }
"#,
    )
    .unwrap();
    package(&root, "runtime", "engine-state", "");
    package(
        &root,
        "packages",
        "foundation",
        "[dependencies]\nstate_alias.workspace = true\n",
    );

    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "foundation -> engine-state");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_crate_dependencies() {
    let root = fixture("valid");
    package(&root, "packages", "hl-log", "");
    package(&root, "runtime", "hl-runtime", "");
    package(
        &root,
        "app",
        "hl-engine",
        r#"
[dependencies]
hl-log = { path = "../../packages/hl-log" }
hl-runtime = { path = "../../runtime/hl-runtime" }
"#,
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_reviewed_dev_edge() {
    let root = fixture("development-contracts");
    package(&root, "containers", "hl-client", "");
    package(
        &root,
        "apps",
        "dockerd",
        "[dev-dependencies]\nhl-client = { path = \"../../containers/hl-client\" }\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_unreviewed_dev_edge() {
    let root = fixture("unreviewed-development-contracts");
    package(&root, "workspaces", "hl-ws", "");
    package(
        &root,
        "apps",
        "dockerd",
        "[dev-dependencies]\nhl-ws = { path = \"../../workspaces/hl-ws\" }\n",
    );

    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "dockerd -> hl-ws");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_dev_edge_in_production() {
    let root = fixture("production-development-contract");
    package(&root, "containers", "hl-client", "");
    package(
        &root,
        "apps",
        "dockerd",
        "[dependencies]\nhl-client = { path = \"../../containers/hl-client\" }\n",
    );

    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "dockerd -> hl-client");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_runtime_logging_foundation() {
    let root = fixture("runtime-logging");
    package(&root, "packages", "hl-log", "");
    package(
        &root,
        "runtime",
        "hl-runtime",
        "[dependencies]\nhl-log = { path = \"../../packages/hl-log\" }\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recognizes_integrated_layers() {
    let root = fixture("integrated-layers");
    package(&root, "packages", "hl-log", "");
    package(&root, "runtime", "hl-runtime", "");
    package(&root, "containers", "hl-container", "");
    package(&root, "workspaces", "hl-ws", "");
    package(
        &root,
        "apps",
        "husklet",
        r#"
[dependencies]
hl-log = { path = "../../packages/hl-log" }
hl-container = { path = "../../containers/hl-container" }
hl-ws = { path = "../../workspaces/hl-ws" }
"#,
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_cross_domain_layer_reversals() {
    let root = fixture("integrated-reversals");
    package(&root, "apps", "husklet", "");
    package(&root, "containers", "hl-container", "");
    package(&root, "workspaces", "hl-ws", "");
    package(
        &root,
        "packages",
        "foundation",
        "[dependencies]\nhl-container = { path = \"../../containers/hl-container\" }\n",
    );
    package(
        &root,
        "runtime",
        "engine-state",
        "[dependencies]\nhusklet = { path = \"../../apps/husklet\" }\n",
    );
    package(
        &root,
        "containers",
        "container-state",
        "[dependencies]\nhl-ws = { path = \"../../workspaces/hl-ws\" }\n",
    );

    let values = findings(&root);
    assert_eq!(values.len(), 3);
    assert!(
        values
            .iter()
            .any(|finding| finding.subject == "foundation -> hl-container")
    );
    assert!(
        values
            .iter()
            .any(|finding| finding.subject == "engine-state -> husklet")
    );
    assert!(
        values
            .iter()
            .any(|finding| finding.subject == "container-state -> hl-ws")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn distinguishes_workspace_members() {
    let root = fixture("registry");
    package(&root, "runtime", "runtime", "");
    package(&root, "packages", "foundation", "[dependencies]\nruntime = \"1\"\n");

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn detects_dev_cycles() {
    let root = fixture("cycles");
    package(
        &root,
        "runtime",
        "first",
        "[dependencies]\nsecond = { path = \"../second\" }\n",
    );
    package(
        &root,
        "runtime",
        "second",
        "[build-dependencies]\nfirst = { path = \"../first\" }\n",
    );
    package(
        &root,
        "runtime",
        "third",
        "[dev-dependencies]\nfourth = { path = \"../fourth\" }\n",
    );
    package(
        &root,
        "runtime",
        "fourth",
        "[dev-dependencies]\nthird = { path = \"../third\" }\n",
    );

    let values = findings(&root);
    let cycles = values
        .iter()
        .filter(|finding| finding.subject.starts_with("crate cycle:"))
        .collect::<Vec<_>>();
    assert_eq!(cycles.len(), 1);
    assert!(cycles[0].subject.contains("first"));
    assert!(cycles[0].subject.contains("second"));
    assert!(!cycles[0].subject.contains("third"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_renamed_tables() {
    let root = fixture("reviewed-edges");
    package(&root, "runtime", "hl-descriptor", "");
    package(&root, "runtime", "hl-task", "");
    package(&root, "runtime", "hl-event", "");
    package(
        &root,
        "runtime",
        "hl-linux",
        r#"
[dependencies]
descriptors = { package = "hl-descriptor", path = "../hl-descriptor" }

[build-dependencies]
hl-task = { path = "../hl-task" }

[target.'cfg(target_os = "macos")'.dependencies]
hl-event = { path = "../hl-event" }
"#,
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_memory_edges() {
    let root = fixture("engine-memory");
    package(&root, "runtime", "hl-isa", "");
    package(&root, "runtime", "hl-loader", "");
    package(&root, "runtime", "hl-memory", "");
    package(
        &root,
        "app",
        "hl-engine",
        "[dependencies]\nhl-isa = { path = \"../../runtime/hl-isa\" }\n\
         hl-loader = { path = \"../../runtime/hl-loader\" }\n\
         hl-memory = { path = \"../../runtime/hl-memory\" }\n",
    );
    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reviews_engine_event_composition_only() {
    let root = fixture("engine-event");
    package(&root, "runtime", "hl-event", "");
    package(&root, "runtime", "hl-vfs", "");
    package(
        &root,
        "app",
        "hl-engine",
        "[dependencies]\nhl-event = { path = \"../../runtime/hl-event\" }\n",
    );
    assert!(findings(&root).is_empty());

    package(
        &root,
        "app",
        "hl-engine",
        "[dependencies]\nhl-event = { path = \"../../runtime/hl-event\" }\n\
         hl-vfs = { path = \"../../runtime/hl-vfs\" }\n",
    );
    let output = findings(&root);
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].subject, "hl-engine -> hl-vfs");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reviews_engine_network_composition_only() {
    let root = fixture("engine-network");
    package(&root, "runtime", "hl-network", "");
    package(&root, "runtime", "hl-vfs", "");
    package(
        &root,
        "app",
        "hl-engine",
        "[dependencies]\nhl-network = { path = \"../../runtime/hl-network\" }\n",
    );
    assert!(findings(&root).is_empty());

    package(
        &root,
        "app",
        "hl-engine",
        "[dependencies]\nhl-network = { path = \"../../runtime/hl-network\" }\n\
         hl-vfs = { path = \"../../runtime/hl-vfs\" }\n",
    );
    let output = findings(&root);
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].subject, "hl-engine -> hl-vfs");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_io_objects() {
    let root = fixture("ipc-descriptor");
    package(&root, "runtime", "hl-descriptor", "");
    package(
        &root,
        "runtime",
        "hl-ipc",
        "[dependencies]\nhl-descriptor = { path = \"../hl-descriptor\" }\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_time_contracts() {
    let root = fixture("ipc-waits");
    package(&root, "runtime", "hl-sync", "");
    package(&root, "runtime", "hl-time", "");
    package(
        &root,
        "runtime",
        "hl-ipc",
        "[dependencies]\nhl-sync = { path = \"../hl-sync\" }\nhl-time = { path = \"../hl-time\" }\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_fork_state() {
    let root = fixture("runtime-memory");
    package(&root, "runtime", "hl-memory", "");
    package(
        &root,
        "runtime",
        "hl-runtime",
        "[dependencies]\nhl-memory = { path = \"../hl-memory\" }\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_isa_geometry() {
    let root = fixture("memory-isa");
    package(&root, "runtime", "hl-isa", "");
    package(
        &root,
        "runtime",
        "hl-memory",
        "[dependencies]\nhl-isa = { path = \"../hl-isa\" }\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_file_objects() {
    let root = fixture("provider-descriptor");
    package(&root, "runtime", "hl-descriptor", "");
    package(
        &root,
        "runtime",
        "hl-provider",
        "[dependencies]\nhl-descriptor = { path = \"../hl-descriptor\" }\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_provider_adapters() {
    let root = fixture("descriptor-provider");
    package(&root, "runtime", "hl-provider", "");
    package(
        &root,
        "runtime",
        "hl-descriptor",
        "[dependencies]\nhl-provider = { path = \"../hl-provider\" }\n",
    );

    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "hl-descriptor -> hl-provider");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_wait_queues() {
    let root = fixture("network-sync");
    package(&root, "runtime", "hl-sync", "");
    package(
        &root,
        "runtime",
        "hl-network",
        "[dependencies]\nhl-sync = { path = \"../hl-sync\" }\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_network_policy() {
    let root = fixture("sync-network");
    package(&root, "runtime", "hl-network", "");
    package(
        &root,
        "runtime",
        "hl-sync",
        "[dependencies]\nhl-network = { path = \"../hl-network\" }\n",
    );

    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "hl-sync -> hl-network");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_dependency_table() {
    let root = fixture("unreviewed-edges");
    package(&root, "runtime", "hl-task", "");
    package(&root, "runtime", "hl-network", "");
    package(&root, "runtime", "hl-loader", "");
    package(
        &root,
        "runtime",
        "hl-descriptor",
        r#"
[dependencies]
hl-task = { path = "../hl-task" }

[build-dependencies]
hl-network = { path = "../hl-network" }

[target.'cfg(target_os = "windows")'.dependencies]
image = { package = "hl-loader", path = "../hl-loader" }
"#,
    );

    let values = findings(&root);
    assert_eq!(values.len(), 3);
    for target in ["hl-task", "hl-network", "hl-loader"] {
        let finding = values
            .iter()
            .find(|finding| finding.subject == format!("hl-descriptor -> {target}"))
            .unwrap();
        assert!(
            finding
                .message
                .contains("not present in the checked engine package graph")
        );
    }
    let target = values
        .iter()
        .find(|finding| finding.subject == "hl-descriptor -> hl-loader")
        .unwrap();
    assert!(
        target
            .review
            .as_ref()
            .unwrap()
            .metadata
            .iter()
            .any(|(key, value)| { key == "Target condition" && value == "cfg(target_os = \"windows\")" })
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn accepts_dependency_edges() {
    let root = fixture("empty-graph");
    package(&root, "packages", "hl-io", "");
    package(&root, "runtime", "hl-descriptor", "");
    package(&root, "app", "hl-engine", "");

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn checks_role_edges() {
    let root = fixture("roles");
    let directory = root.join("src/runtime/runtime");
    fs::create_dir_all(directory.join("src/model")).unwrap();
    fs::write(
        directory.join("Cargo.toml"),
        "[package]\nname = \"runtime\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        directory.join("src/model/value.rs"),
        r#"
use crate::{model::Value, service::Sessions};
use crate::modeling::Unrelated;

fn local() {
    let _ = crate::adapters::Host;
    let service = 1;
    let _ = service;
}
"#,
    )
    .unwrap();

    let values = findings(&root);
    assert_eq!(values.len(), 2);
    assert!(values.iter().any(|finding| finding.subject == "model -> service"));
    assert!(values.iter().any(|finding| finding.subject == "model -> adapters"));
    fs::remove_dir_all(root).unwrap();
}
