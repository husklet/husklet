use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{rule::Rule, Workspace};

use super::DependencyDirection;

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
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n{dependencies}"
        ),
    )
    .unwrap();
    fs::write(directory.join("src/lib.rs"), "").unwrap();
}

fn findings(root: &Path) -> Vec<crate::Finding> {
    let workspace = Workspace::load([root.join("src")]).unwrap();
    DependencyDirection.check(&workspace).unwrap()
}

#[test]
fn detects_every_dependency_table_and_resolves_renamed_packages() {
    let root = fixture("tables");
    package(&root, "containers", "runtime", "");
    package(&root, "workspaces", "sessions", "");
    package(&root, "apps", "husklet", "");
    package(
        &root,
        "packages",
        "foundation",
        r#"
[dependencies]
runtime_alias = { package = "runtime", path = "../../containers/runtime" }

[build-dependencies]
sessions = { path = "../../workspaces/sessions" }

[target.'cfg(target_os = "macos")'.dev-dependencies]
product = { package = "husklet", path = "../../apps/husklet" }
"#,
    );

    let values = findings(&root);
    assert_eq!(values.len(), 3);
    assert!(values
        .iter()
        .any(|finding| finding.subject == "foundation -> runtime"));
    assert!(values
        .iter()
        .any(|finding| finding.subject == "foundation -> sessions"));
    let target = values
        .iter()
        .find(|finding| finding.subject == "foundation -> husklet")
        .unwrap();
    let review = target.review.as_ref().unwrap();
    assert!(review
        .metadata
        .contains(&("Dependency kind".into(), "development".into())));
    assert!(review.metadata.iter().any(|(key, value)| {
        key == "Target condition" && value == "cfg(target_os = \"macos\")"
    }));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resolves_workspace_inherited_renamed_dependencies() {
    let root = fixture("workspace");
    fs::write(
        root.join("Cargo.toml"),
        r#"
[workspace]
resolver = "2"
members = ["src/*/*"]

[workspace.dependencies]
runtime_alias = { package = "runtime", path = "src/containers/runtime" }
"#,
    )
    .unwrap();
    package(&root, "containers", "runtime", "");
    package(
        &root,
        "packages",
        "foundation",
        "[dependencies]\nruntime_alias.workspace = true\n",
    );

    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "foundation -> runtime");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permits_inward_crate_dependencies() {
    let root = fixture("valid");
    package(&root, "packages", "foundation", "");
    package(
        &root,
        "containers",
        "runtime",
        "[dependencies]\nfoundation = { path = \"../../packages/foundation\" }\n",
    );
    package(
        &root,
        "apps",
        "husklet",
        r#"
[dependencies]
foundation = { path = "../../packages/foundation" }
runtime = { path = "../../containers/runtime" }
"#,
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn does_not_confuse_registry_package_names_with_workspace_members() {
    let root = fixture("registry");
    package(&root, "containers", "runtime", "");
    package(
        &root,
        "packages",
        "foundation",
        "[dependencies]\nruntime = \"1\"\n",
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn detects_normal_and_build_cycles_but_not_dev_only_cycles() {
    let root = fixture("cycles");
    package(
        &root,
        "containers",
        "first",
        "[dependencies]\nsecond = { path = \"../second\" }\n",
    );
    package(
        &root,
        "containers",
        "second",
        "[build-dependencies]\nfirst = { path = \"../first\" }\n",
    );
    package(
        &root,
        "gpu",
        "third",
        "[dev-dependencies]\nfourth = { path = \"../fourth\" }\n",
    );
    package(
        &root,
        "gpu",
        "fourth",
        "[dev-dependencies]\nthird = { path = \"../third\" }\n",
    );

    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert!(values[0].subject.contains("first"));
    assert!(values[0].subject.contains("second"));
    assert!(!values[0].subject.contains("third"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn checks_only_proven_crate_root_role_edges() {
    let root = fixture("roles");
    let directory = root.join("src/workspaces/runtime");
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
    assert!(values
        .iter()
        .any(|finding| finding.subject == "model -> service"));
    assert!(values
        .iter()
        .any(|finding| finding.subject == "model -> adapters"));
    fs::remove_dir_all(root).unwrap();
}
