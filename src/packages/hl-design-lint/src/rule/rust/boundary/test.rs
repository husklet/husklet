use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{rule::Rule, source::Workspace};

use super::PathModules;

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hl-design-lint-boundary-{name}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\nversion='0.0.0'\n").unwrap();
    root
}

#[test]
fn rejects_multiple_injected_domains() {
    let root = fixture("domains");
    fs::write(
        root.join("src/lib.rs"),
        r#"
#[path = "registry/state.rs"] mod state;
#[path = "registry/snapshot.rs"] mod snapshot;
#[path = "signal/plan.rs"] mod signal_plan;
#[path = "checkpoint/mod.rs"] mod checkpoint;
"#,
    )
    .unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = PathModules.check(&workspace).unwrap();
    assert_eq!(findings.len(), 1);
    assert!(findings[0].message.contains("checkpoint, registry, signal"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn keeps_one_domain_and_conditional_wiring() {
    let root = fixture("allowed");
    fs::write(
        root.join("src/lib.rs"),
        r#"
#[path = "registry/state.rs"] mod state;
#[path = "registry/snapshot.rs"] mod snapshot;
#[cfg(test)] #[path = "fixture/mock.rs"] mod mock;
#[cfg(target_os = "linux")] #[path = "platform/linux.rs"] mod linux;
"#,
    )
    .unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    assert!(PathModules.check(&workspace).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}
