use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{Workspace, rule::Rule};

use super::Repository;

fn findings(relative: &str, source: &str) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-repository-escape-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let package_root = root.join("src/containers/hl-engine");
    let path = package_root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture source has a parent")).unwrap();
    fs::write(
        package_root.join("Cargo.toml"),
        "[package]\nname = \"hl-engine\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    fs::write(&path, source).unwrap();
    let workspace = Workspace::load([PathBuf::from(&path)]).unwrap();
    let values = Repository.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    values
}

#[test]
fn rejects_path_leaving_the_repository() {
    let values = findings("src/native/executor.rs", "const ORACLE: &str = \"../../../../../../engine/number.c\";\n");
    assert_eq!(values.len(), 1);
    assert!(values[0].message.contains("outside the repository root"));
}

#[test]
fn rejects_include_of_another_directory() {
    let values = findings("src/native/executor.rs", "include!(\"../../../../native/cpu/rust/layout.rs\");\n");
    assert_eq!(values.len(), 1);
    assert!(values[0].message.contains("owned by another directory"));
}

#[test]
fn accepts_paths_inside_the_owning_crate() {
    let values = findings(
        "src/native/executor.rs",
        "const TABLE: &str = include_str!(\"../../numbers.tsv\");\nconst HEADER: &str = \"../include/cpu.h\";\n",
    );
    assert!(values.is_empty());
}
