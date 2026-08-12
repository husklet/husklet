use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{rule::Rule, source::Workspace};

use super::{Dependency, Directory};

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hl-design-lint-suite-{name}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\nversion='0.0.0'\n").unwrap();
    root
}

#[test]
fn rejects_detached_test_directory() {
    let root = fixture("directory");
    fs::create_dir_all(root.join("src/test")).unwrap();
    fs::write(root.join("src/lib.rs"), "#[cfg(test)] mod test;").unwrap();
    fs::write(root.join("src/test/mod.rs"), "mod alpha_test; mod beta_test;").unwrap();
    fs::write(root.join("src/test/alpha_test.rs"), "").unwrap();
    fs::write(root.join("src/test/beta_test.rs"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    assert_eq!(Directory.check(&workspace).unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn keeps_tests_beside_production_owner() {
    let root = fixture("owner");
    fs::create_dir_all(root.join("src/registry")).unwrap();
    fs::write(root.join("src/lib.rs"), "mod registry;").unwrap();
    fs::write(
        root.join("src/registry/mod.rs"),
        "mod state; #[cfg(test)] mod state_test;",
    )
    .unwrap();
    fs::write(root.join("src/registry/state.rs"), "pub struct State;").unwrap();
    fs::write(root.join("src/registry/state_test.rs"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    assert!(Directory.check(&workspace).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_sibling_test_import_but_keeps_support() {
    let root = fixture("dependency");
    fs::write(
        root.join("src/lib.rs"),
        "#[cfg(test)] mod alpha_test; #[cfg(test)] mod beta_test; #[cfg(test)] mod test_support;",
    )
    .unwrap();
    fs::write(
        root.join("src/alpha_test.rs"),
        "use super::beta_test::Fixture; use super::beta_test::Fixture as OtherFixture; use super::test_support::Builder;",
    )
    .unwrap();
    fs::write(root.join("src/beta_test.rs"), "pub struct Fixture;").unwrap();
    fs::write(root.join("src/test_support.rs"), "pub struct Builder;").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = Dependency.check(&workspace).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "beta_test");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_inline_test_module_dependency() {
    let root = fixture("inline-dependency");
    fs::write(
        root.join("src/lib.rs"),
        r"
#[cfg(test)] mod alpha {
    use super::beta::Fixture;
}
#[cfg(test)] mod beta {
    pub struct Fixture;
}
",
    )
    .unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = Dependency.check(&workspace).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "beta");
    fs::remove_dir_all(root).unwrap();
}
