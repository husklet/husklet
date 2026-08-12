use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{rule::Rule, source::Workspace};

use super::SingleFileDirectory;

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hl-design-lint-folder-{name}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn reports_one_file() {
    let root = fixture("single");
    let module = root.join("workspace");
    fs::create_dir_all(&module).unwrap();
    fs::write(module.join("view.rs"), "").unwrap();

    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = SingleFileDirectory.check(&workspace).unwrap();

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "workspace");
    assert_eq!(findings[0].severity, crate::Severity::Error);
    assert_eq!(findings[0].location.path, module);
    assert!(findings[0].message.contains("view.rs"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn keeps_cargo_roots() {
    let root = fixture("valid");
    let component = root.join("component");
    fs::create_dir_all(&component).unwrap();
    fs::write(component.join("button.rs"), "").unwrap();
    fs::write(component.join("input.rs"), "").unwrap();
    let package = root.join("crate");
    fs::create_dir_all(package.join("src")).unwrap();
    fs::write(package.join("Cargo.toml"), "[package]\nname='crate'\nversion='0.0.0'\n").unwrap();
    fs::write(package.join("src/lib.rs"), "").unwrap();
    let module = root.join("layout");
    fs::create_dir_all(&module).unwrap();
    fs::write(root.join("layout.rs"), "mod tests;").unwrap();
    fs::write(module.join("tests.rs"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();

    assert!(SingleFileDirectory.check(&workspace).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn keeps_fixture_golden_boundary() {
    let root = fixture("golden");
    let expected = root.join("tests/compat/process/expected");
    fs::create_dir_all(&expected).unwrap();
    fs::write(expected.join("lifecycle.out"), "ok\n").unwrap();

    let workspace = Workspace::load([root.clone()]).unwrap();

    assert!(SingleFileDirectory.check(&workspace).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reports_when_required_structure_is_mutated_into_arbitrary_structure() {
    let root = fixture("non-vacuity");
    let package = root.join("crate");
    fs::create_dir_all(package.join("src")).unwrap();
    fs::write(package.join("Cargo.toml"), "[package]\nname='crate'\nversion='0.0.0'\n").unwrap();
    fs::write(package.join("src/lib.rs"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    assert!(SingleFileDirectory.check(&workspace).unwrap().is_empty());

    fs::rename(package.join("src"), package.join("implementation")).unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = SingleFileDirectory.check(&workspace).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].location.path, package.join("implementation"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn does_not_exempt_arbitrary_registry_or_reference_directories() {
    let root = fixture("narrow");
    for name in ["registry", "references", "migrations"] {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("only.data"), "value").unwrap();
    }
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = SingleFileDirectory.check(&workspace).unwrap();
    assert_eq!(findings.len(), 3);
    fs::remove_dir_all(root).unwrap();
}
