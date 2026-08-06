use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{Workspace, rule::Rule};

use super::Boundary;

fn findings(package: &str, layer: &str, relative: &str, source: &str) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-unsafe-boundary-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let package_root = root.join("src").join(layer).join(package);
    let path = package_root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture source has a parent")).unwrap();
    fs::write(
        package_root.join("Cargo.toml"),
        format!("[package]\nname = \"{package}\"\nversion = \"0.0.0\"\n"),
    )
    .unwrap();
    fs::write(&path, source).unwrap();
    let workspace = Workspace::load([PathBuf::from(&path)]).unwrap();
    let values = Boundary.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    values
}

fn ordinary(source: &str) -> Vec<crate::Finding> {
    findings("hl-memory", "runtime", "src/lib.rs", source)
}

#[test]
fn rejects_approved_boundary() {
    let values = ordinary(
        r"
unsafe fn free() {}
struct Value;
unsafe trait Contract {
    unsafe fn invoke();
}
unsafe impl Contract for Value {
    unsafe fn invoke() {}
}
fn call() {
    // SAFETY: the fixture deliberately exercises a forbidden but documented block.
    unsafe {}
}
",
    );

    for subject in [
        "unsafe function `free`",
        "unsafe trait `Contract`",
        "unsafe trait method `invoke`",
        "unsafe impl",
        "unsafe method `invoke`",
        "unsafe block",
    ] {
        assert!(
            values.iter().any(|finding| finding.subject == subject),
            "missing finding for {subject}"
        );
    }
    assert_eq!(values.len(), 6);
}

#[test]
fn permits_ffi_module() {
    let source = r"
unsafe fn entry() {}
fn call() {
    // SAFETY: the pointer contract is validated by the boundary caller.
    unsafe {}
}
";
    assert!(findings("native-kernel", "runtime", "src/native/execution.rs", source,).is_empty());
    for relative in ["src/ffi.rs", "src/ffi/export.rs"] {
        assert!(
            findings("hl-engine", "app", relative, source).is_empty(),
            "{relative} must be an approved FFI module"
        );
    }
    assert!(findings("husklet", "apps", "src/ffi/export.rs", source).is_empty());
    assert!(findings("hl-engine", "containers", "src/ffi/export.rs", source).is_empty());
    assert!(
        findings(
            "hl-engine",
            "app",
            "src/lib.rs",
            r"
mod ffi {
    unsafe fn export() {}
    fn call() {
        // SAFETY: the caller validates the pointer before entering the adapter.
        unsafe {}
    }
}
",
        )
        .is_empty()
    );
}

#[test]
fn requires_allowed_block() {
    let values = findings(
        "native-kernel",
        "runtime",
        "src/native/execution.rs",
        r"
fn missing() {
    unsafe {}
}

fn present() {
    // SAFETY: the allocation remains live and aligned for this access.
    unsafe {}
}
",
    );
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "unsafe block without SAFETY rationale");
}

#[test]
fn rejects_similar_names() {
    for relative in ["src/not_ffi.rs", "src/model/ffi_value.rs", "src/ffi_adapter.rs"] {
        let values = findings(
            "hl-engine",
            "app",
            relative,
            "// SAFETY: documented but outside the approved module.\nfn call() { unsafe {} }",
        );
        assert_eq!(values.len(), 1, "{relative}");
        assert_eq!(values[0].subject, "unsafe block");
    }
    assert_eq!(
        findings(
            "hl-engine-helper",
            "runtime",
            "src/ffi.rs",
            "// SAFETY: documented but outside the application layer.\nfn call() { unsafe {} }",
        )
        .len(),
        1
    );
}

#[test]
fn marker_needs_reason() {
    let values = findings(
        "native-kernel",
        "runtime",
        "src/native/execution.rs",
        "fn call() {\n    // SAFETY:\n    unsafe {}\n}",
    );
    assert_eq!(values.len(), 1);
}
