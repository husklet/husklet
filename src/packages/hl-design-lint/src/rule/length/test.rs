use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{rule::Rule, source::Workspace};

use super::FileLength;

fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "hl-design-length-{name}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\nversion='0.0.0'\n").unwrap();
    let source = root.join("src/lib.rs");
    (root, source)
}

#[test]
fn excludes_large_inline_test_module() {
    let (root, source) = fixture("inline");
    let production = "pub struct Production;\n".repeat(200);
    let tests = "let _ = 1;\n".repeat(400);
    fs::write(
        &source,
        format!("{production}#[cfg(test)]\nmod tests {{\n#[test]\nfn behavior() {{\n{tests}}}\n}}\n"),
    )
    .unwrap();
    let workspace = Workspace::load([source]).unwrap();
    assert!(FileLength.check(&workspace).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn excludes_test_module_nested_in_production_module() {
    let (root, source) = fixture("nested-inline");
    let production = "pub struct Production;\n".repeat(200);
    let tests = "let _ = 1;\n".repeat(400);
    fs::write(
        &source,
        format!(
            "mod domain {{\n{production}#[cfg(test)]\nmod tests {{\n#[test]\nfn behavior() {{\n{tests}}}\n}}\n}}\n"
        ),
    )
    .unwrap();
    let workspace = Workspace::load([source]).unwrap();
    assert!(FileLength.check(&workspace).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_more_than_limit_production_lines() {
    let (root, source) = fixture("production");
    fs::write(&source, "pub struct Production;\n".repeat(501)).unwrap();
    let workspace = Workspace::load([source]).unwrap();
    let findings = FileLength.check(&workspace).unwrap();
    assert_eq!(findings.len(), 1);
    assert!(findings[0].message.contains("501 lines"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn excludes_singular_companion_test_file() {
    let (root, source) = fixture("companion");
    fs::write(&source, "pub struct Production;\n").unwrap();
    let test = root.join("src/production_test.rs");
    fs::write(&test, "#[test]\nfn behavior() {}\n".repeat(600)).unwrap();
    let workspace = Workspace::load([root.join("src")]).unwrap();
    assert!(FileLength.check(&workspace).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}
