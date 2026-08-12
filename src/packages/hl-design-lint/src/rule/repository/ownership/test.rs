use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

fn fixture(domain: &str, package: &str) -> Workspace {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-design-lint-ownership-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let directory = root.join("src").join(domain).join(package);
    fs::create_dir_all(directory.join("src")).unwrap();
    fs::write(
        directory.join("Cargo.toml"),
        format!("[package]\nname = \"{package}\"\nversion = \"0.0.0\"\n"),
    )
    .unwrap();
    fs::write(directory.join("src/main.rs"), "fn main() {}\n").unwrap();
    Workspace::load([root.join("src")]).unwrap()
}

#[test]
fn rejects_runtime_audit_package() {
    let policy = crate::policy::OwnershipPolicy {
        protected_domains: vec!["runtime".into()],
        tool_contains: vec!["audit".into()],
        ..Default::default()
    };
    let findings = RuntimeTool::new(policy)
        .check(&fixture("runtime", "hl-syscall-audit"))
        .unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule, "runtime-tool-ownership");
}

#[test]
fn permits_testing_owned_audit_module() {
    assert!(
        RuntimeTool::default()
            .check(&fixture("apps", "testing"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn permits_runtime_domain_package() {
    assert!(
        RuntimeTool::default()
            .check(&fixture("runtime", "hl-linux"))
            .unwrap()
            .is_empty()
    );
}
