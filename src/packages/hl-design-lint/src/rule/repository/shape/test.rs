use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{rule::Rule, source::Workspace};

use super::{FileName, FolderNoun, ModulePrefix, ParentName, PrefixDirectory, TestName, words};

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hl-design-lint-shape-{name}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='shape-fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    root
}

#[test]
fn semantic_acronyms_together() {
    assert_eq!(
        words("PreparedSharedMemoryExec"),
        ["prepared", "shared", "memory", "exec"]
    );
    assert_eq!(words("HTTPServerABI"), ["http", "server", "abi"]);
    assert_eq!(words("ipc_shared_test"), ["ipc", "shared", "test"]);
}

#[test]
fn filename_does_not_repeat_parent_semantic_word() {
    let root = fixture("parent-name");
    fs::create_dir_all(root.join("src/memory")).unwrap();
    fs::create_dir_all(root.join("src/net_work")).unwrap();
    for path in [
        "src/memory/shared_memory.c",
        "src/memory/memory_map.h",
        "src/net_work/socket_work.rs",
    ] {
        fs::write(root.join(path), "").unwrap();
    }
    for path in [
        "src/memory/memorial.c",
        "src/memory/shared.c",
        "src/net_work/network.rs",
        "src/memory/index.c",
    ] {
        fs::write(root.join(path), "").unwrap();
    }
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = ParentName.check(&workspace).unwrap();

    assert_eq!(findings.len(), 3);
    assert!(findings.iter().any(|finding| finding.subject == "shared_memory"));
    assert!(findings.iter().any(|finding| finding.subject == "memory_map"));
    assert!(findings.iter().any(|finding| finding.subject == "socket_work"));
    assert!(!findings.iter().any(|finding| finding.subject == "memorial"));
    assert!(!findings.iter().any(|finding| finding.subject == "network"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn filename_conventions_independent() {
    let root = fixture("files");
    fs::write(root.join("src/message_queue.rs"), "").unwrap();
    fs::write(root.join("src/message_queue_test.rs"), "").unwrap();
    fs::write(root.join("src/message_queue_tests.rs"), "").unwrap();
    fs::write(root.join("src/shared_message_queue.rs"), "").unwrap();
    fs::write(root.join("src/mod.rs"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();

    // The `_test` companion suffix `singular-test-file` requires is not a semantic word.
    let findings = FileName.check(&workspace).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "shared_message_queue");
    assert_eq!(TestName.check(&workspace).unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn filename_stem_policy() {
    let root = fixture("filename-policy");
    for extension in ["rs", "c", "h"] {
        for stem in ["single", "short_name", "aaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbb"] {
            fs::write(root.join("src").join(format!("{stem}.{extension}")), "").unwrap();
        }
        for stem in ["dashed-name", "three_word_name", "abcdefghijklmnopqrstuvwxyzabcdefg"] {
            fs::write(root.join("src").join(format!("{stem}.{extension}")), "").unwrap();
        }
    }
    fs::create_dir_all(root.join("include")).unwrap();
    fs::write(root.join("include/outside-name.h"), "").unwrap();
    fs::create_dir_all(root.join("target/generated")).unwrap();
    fs::write(root.join("target/generated/build-name.c"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = FileName.check(&workspace).unwrap();

    assert_eq!(findings.len(), 9);
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.message.contains("contains a dash"))
            .count(),
        3
    );
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.message.contains("maximum is 32"))
            .count(),
        3
    );
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.message.contains("semantic words"))
            .count(),
        3
    );
    assert!(findings.iter().all(|finding| finding.location.line == 1));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_header_path_is_checked() {
    let root = fixture("explicit-header");
    let header = root.join("src/invalid-header.h");
    fs::write(&header, "").unwrap();
    let workspace = Workspace::load([header]).unwrap();
    let findings = FileName.check(&workspace).unwrap();

    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].message,
        "C header filename stem `invalid-header` in `invalid-header.h` contains a dash"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn c_source_diagnostic_names_source_and_extension() {
    let root = fixture("c-source-diagnostic");
    fs::write(root.join("src/three_word_name.c"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = FileName.check(&workspace).unwrap();

    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].message,
        "C source filename `three_word_name.c` contains more than two semantic words"
    );
    assert!(!findings[0].message.contains("Rust"));
    assert!(!findings[0].message.contains(".rs"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn repository_test_sources_are_exempt() {
    let root = fixture("repository-test-sources");
    for directory in ["tests/runtime", "src/exec/test"] {
        fs::create_dir_all(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("aarch64_dirty_bound.c"), "").unwrap();
        fs::write(root.join(directory).join("invalid-rust-source.rs"), "").unwrap();
    }
    fs::write(root.join("src/invalid-c-source.c"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = FileName.check(&workspace).unwrap();

    // A test filename names the assertion it makes; a production filename names an API.
    assert_eq!(findings.len(), 1);
    assert!(findings[0].location.path.ends_with("src/invalid-c-source.c"));
    assert!(findings[0].message.starts_with("C source filename stem"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prefix_directory_ignores_companion_tests() {
    let root = fixture("prefix-companions");
    fs::create_dir_all(root.join("src/store")).unwrap();
    for name in ["atomic_access.rs", "atomic_access_test.rs", "atomic_batch.rs"] {
        fs::write(root.join("src/store").join(name), "").unwrap();
    }
    let workspace = Workspace::load([root.clone()]).unwrap();
    assert!(PrefixDirectory.check(&workspace).unwrap().is_empty());

    fs::write(root.join("src/store/atomic_publish.rs"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = PrefixDirectory.check(&workspace).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "atomic");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn numbered_fragments_fail() {
    let root = fixture("numbered-files");
    for name in ["part_1.rs", "chunk2.rs", "section_03.rs"] {
        fs::write(root.join("src").join(name), "").unwrap();
    }
    for name in ["sha1.rs", "x86.rs", "utf8.rs"] {
        fs::write(root.join("src").join(name), "").unwrap();
    }
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = FileName.check(&workspace).unwrap();

    assert_eq!(findings.len(), 3);
    assert!(
        findings
            .iter()
            .all(|finding| finding.message.contains("numbered implementation fragment"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn three_require_module() {
    let root = fixture("prefix");
    for child in ["b", "c", "d"] {
        fs::write(root.join(format!("src/a_{child}.rs")), "").unwrap();
    }
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = PrefixDirectory.check(&workspace).unwrap();

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "a");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn declarations_module_noun() {
    let root = fixture("redundant");
    fs::create_dir_all(root.join("src/launcher")).unwrap();
    fs::write(
        root.join("src/launcher/plan.rs"),
        r"
struct LauncherPlan;
struct RuntimePlan;
fn launcher_start() {}
struct Runner;
impl Runner { fn launcher_prepare() {} }
impl external::Contract for Runner { fn launcher_external_name() {} }
trait Drive { fn launcher_publish(); }
#[cfg(test)]
mod tests { struct LauncherFixture; }
",
    )
    .unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = ModulePrefix.check(&workspace).unwrap();

    // An `impl` item is namespaced by its type, not by the parent directory.
    assert_eq!(findings.len(), 3);
    assert!(!findings.iter().any(|finding| finding.subject == "LauncherFixture"));
    assert!(!findings.iter().any(|finding| finding.subject == "launcher_prepare"));
    assert!(findings.iter().any(|finding| finding.subject == "launcher_publish"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn naming_test_files() {
    let root = fixture("test-symbols");
    fs::create_dir_all(root.join("src/launcher")).unwrap();
    fs::write(
        root.join("src/launcher/plan_test.rs"),
        r"
struct LauncherPlanFixture;
fn launcher_fixture() {}
fn deliberately_very_long_test_function_name() {}
",
    )
    .unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();

    assert_eq!(ModulePrefix.check(&workspace).unwrap().len(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn naming_test_attribute() {
    let root = fixture("test-attribute");
    fs::create_dir_all(root.join("src/launcher")).unwrap();
    fs::write(
        root.join("src/launcher/plan.rs"),
        r"
#[test]
fn launcher_rejects_a_deliberately_long_assertion_name() {}
fn launcher_rejects_a_deliberately_long_production_name() {}
",
    )
    .unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();

    assert_eq!(ModulePrefix.check(&workspace).unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn module_noun_evidence() {
    let root = fixture("noun");
    fs::create_dir_all(root.join("src/selected")).unwrap();
    fs::write(root.join("src/selected/value.rs"), "").unwrap();
    fs::create_dir_all(root.join("src/launcher")).unwrap();
    fs::write(root.join("src/launcher/plan.rs"), "").unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let findings = FolderNoun.check(&workspace).unwrap();

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "selected");
    fs::remove_dir_all(root).unwrap();
}
