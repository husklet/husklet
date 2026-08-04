use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{rule::Rule, source::Workspace};

use super::Suffix;

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hl-design-lint-role-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\nversion='0.0.0'\n").unwrap();
    root
}

#[test]
fn reports_roles_but_keeps_test_companions() {
    let root = fixture();
    for child in [
        "job_registry.rs",
        "wait_registry.rs",
        "tid_registry.rs",
        "job_test.rs",
        "wait_test.rs",
        "tid_test.rs",
    ] {
        fs::write(root.join("src").join(child), "").unwrap();
    }
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = Suffix.check(&workspace).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "registry");
    fs::remove_dir_all(root).unwrap();
}
