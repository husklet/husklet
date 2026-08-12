use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{policy::SourcePolicy, rule::Rule, source::Workspace};

use super::Directory;

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hl-design-lint-empty-{name}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn relative_findings(root: &Path, workspace: &Workspace) -> Vec<PathBuf> {
    Directory
        .check(workspace)
        .unwrap()
        .iter()
        .map(|finding| finding.location.path.strip_prefix(root).unwrap().to_owned())
        .collect()
}

#[test]
fn reports_empty_and_placeholder_only_directories() {
    let root = fixture("placeholder");
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("planned")).unwrap();
    fs::write(root.join("planned/.gitkeep"), "").unwrap();
    fs::write(root.join("owned.txt"), "content").unwrap();

    let workspace = Workspace::load([root.clone()]).unwrap();
    assert_eq!(
        relative_findings(&root, &workspace),
        [Path::new("empty"), Path::new("planned")]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ignored_generated_entries_do_not_justify_a_parent() {
    let root = fixture("generated");
    let parent = root.join("owned-parent");
    fs::create_dir_all(parent.join("generated-cache/nested-empty")).unwrap();
    let policy = SourcePolicy {
        ignored_directories: vec!["generated-cache".into()],
        ..Default::default()
    };

    let workspace = Workspace::load_with_policy([root.clone()], &policy).unwrap();
    let paths = relative_findings(&root, &workspace);

    assert_eq!(paths, [Path::new("owned-parent")]);
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with("owned-parent/generated-cache"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn owned_file_or_directory_is_substantive() {
    let root = fixture("nonempty");
    fs::create_dir_all(root.join("module/nested")).unwrap();
    fs::write(root.join("module/nested/value.c"), "int value;\n").unwrap();

    let workspace = Workspace::load([root.clone()]).unwrap();

    assert!(Directory.check(&workspace).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleting_the_only_owned_file_makes_the_rule_red() {
    let root = fixture("non-vacuity");
    let module = root.join("module");
    fs::create_dir_all(&module).unwrap();
    fs::write(module.join("value.c"), "int value;\n").unwrap();
    assert!(
        Directory
            .check(&Workspace::load([root.clone()]).unwrap())
            .unwrap()
            .is_empty()
    );

    fs::remove_file(module.join("value.c")).unwrap();
    let findings = Directory.check(&Workspace::load([root.clone()]).unwrap()).unwrap();

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].location.path, module);
    fs::remove_dir_all(root).unwrap();
}
