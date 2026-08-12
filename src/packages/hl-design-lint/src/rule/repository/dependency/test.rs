use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{DependencyPolicy, LayerPolicy, PackageDependencyBudget, Workspace, rule::Rule};

use super::Direction;

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "dependency-policy-{}-{}",
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

fn policy() -> DependencyPolicy {
    DependencyPolicy {
        ignored_packages: vec!["ignored-analyzer".into()],
        layers: vec![
            LayerPolicy {
                name: "foundation".into(),
                directory: "foundation".into(),
                may_depend_on: vec!["foundation".into()],
            },
            LayerPolicy {
                name: "service".into(),
                directory: "services".into(),
                may_depend_on: vec!["foundation".into(), "service".into()],
            },
            LayerPolicy {
                name: "product".into(),
                directory: "products".into(),
                may_depend_on: vec!["foundation".into(), "service".into(), "product".into()],
            },
        ],
        package_budgets: Vec::new(),
    }
}

fn findings(root: &Path, policy: DependencyPolicy) -> Vec<crate::Finding> {
    let workspace = Workspace::load([root.join("src")]).unwrap();
    Direction::new(policy).check(&workspace).unwrap()
}

#[test]
fn accepts_dependency_permitted_by_layer_policy() {
    let root = fixture();
    package(&root, "foundation", "clock", "");
    package(
        &root,
        "services",
        "scheduler",
        "[dependencies]\nclock = { path = \"../../foundation/clock\" }\n",
    );
    assert!(findings(&root, policy()).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_forbidden_layer_direction() {
    let root = fixture();
    package(&root, "products", "console", "");
    package(
        &root,
        "foundation",
        "clock",
        "[dependencies]\nconsole = { path = \"../../products/console\" }\n",
    );
    let values = findings(&root, policy());
    assert_eq!(values.len(), 1);
    assert!(values[0].message.contains("layer policy"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_budget_counts_distinct_local_targets_across_dependency_kinds() {
    let root = fixture();
    package(&root, "foundation", "clock", "");
    package(&root, "foundation", "storage", "");
    package(
        &root,
        "services",
        "scheduler",
        "[dependencies]\nclock = { path = \"../../foundation/clock\" }\n[dev-dependencies]\nstorage = { path = \"../../foundation/storage\" }\n",
    );
    let mut policy = policy();
    policy.package_budgets.push(PackageDependencyBudget {
        package: "scheduler".into(),
        maximum: 1,
    });
    let values = findings(&root, policy);
    assert_eq!(values.len(), 1);
    assert!(values[0].message.contains("exceeding its configured maximum of 1"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn configured_policy_rejects_unclassified_local_packages() {
    let root = fixture();
    package(&root, "unknown", "clock", "");
    package(
        &root,
        "services",
        "scheduler",
        "[dependencies]\nclock = { path = \"../../unknown/clock\" }\n",
    );
    let values = findings(&root, policy());
    assert_eq!(values.len(), 1);
    assert!(values[0].message.contains("not classified"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_policy_is_portable_and_only_checks_cycles() {
    let root = fixture();
    package(&root, "custom", "alpha", "");
    package(
        &root,
        "custom",
        "beta",
        "[dependencies]\nalpha = { path = \"../alpha\" }\n",
    );
    assert!(findings(&root, DependencyPolicy::default()).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn detects_normal_dependency_cycles() {
    let root = fixture();
    package(
        &root,
        "custom",
        "alpha",
        "[dependencies]\nbeta = { path = \"../beta\" }\n",
    );
    package(
        &root,
        "custom",
        "beta",
        "[dependencies]\nalpha = { path = \"../alpha\" }\n",
    );
    let values = findings(&root, DependencyPolicy::default());
    assert_eq!(values.len(), 1);
    assert!(values[0].subject.starts_with("crate cycle:"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dependency_analyzer_contains_no_repository_business_literals() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/rule/repository/dependency");
    for name in ["mod.rs", "cycle.rs", "discovery.rs", "location.rs", "module.rs"] {
        let source = fs::read_to_string(directory.join(name)).unwrap();
        for forbidden in [
            "husklet",
            "hl-",
            "src/runtime",
            "src/packages",
            "src/apps",
            "src/containers",
        ] {
            assert!(
                !source.contains(forbidden),
                "generic dependency analyzer {name} contains repository literal {forbidden:?}"
            );
        }
    }
}
