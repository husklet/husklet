use super::SourcePath;
use crate::{Rule, source::Workspace};
use std::{fs, path::PathBuf};

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hl-design-lint-catch-all-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    root
}

#[test]
fn rejects_generic_c_files_and_directories_below_any_source_root() {
    let root = fixture("c");
    for path in ["common.c", "portable/helpers/clock.c", "portable/network.c"] {
        let path = root.join("src").join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "int value;\n").unwrap();
    }

    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = SourcePath.check(&workspace).unwrap();
    assert_eq!(
        findings
            .iter()
            .map(|finding| finding.location.path.strip_prefix(root.join("src")).unwrap().to_owned())
            .collect::<Vec<_>>(),
        [PathBuf::from("common.c"), PathBuf::from("portable/helpers")]
    );
    assert!(findings.iter().all(|finding| finding.rule == "catch-all-source-path"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_every_reserved_name_without_project_paths() {
    let root = fixture("names");
    for name in ["common", "core", "helper", "helpers", "misc", "shared", "util", "utils"] {
        let source = root.join("src").join(format!("{name}.h"));
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(source, "int value;\n").unwrap();
    }
    let findings = SourcePath.check(&Workspace::load([root.clone()]).unwrap()).unwrap();
    assert_eq!(findings.len(), 8);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn accepts_shared_file_when_capability_and_sibling_supply_context() {
    let root = fixture("contextual-shared");
    for path in ["platform/memory/shared.c", "platform/memory/mapping.c"] {
        let path = root.join("src").join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "int value;\n").unwrap();
    }

    let findings = SourcePath.check(&Workspace::load([root.clone()]).unwrap()).unwrap();
    assert!(findings.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_shared_file_without_a_scoped_capability_or_precise_sibling() {
    let root = fixture("ambiguous-shared");
    for path in ["shared.c", "memory/shared.c", "platform/storage/shared.c"] {
        let path = root.join("src").join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "int value;\n").unwrap();
    }

    let findings = SourcePath.check(&Workspace::load([root.clone()]).unwrap()).unwrap();
    assert_eq!(findings.len(), 3);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn contextual_exception_does_not_apply_to_other_catch_all_names() {
    let root = fixture("contextual-common");
    for path in ["platform/memory/common.c", "platform/memory/mapping.c"] {
        let path = root.join("src").join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "int value;\n").unwrap();
    }

    let findings = SourcePath.check(&Workspace::load([root.clone()]).unwrap()).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "common");
    fs::remove_dir_all(root).unwrap();
}
