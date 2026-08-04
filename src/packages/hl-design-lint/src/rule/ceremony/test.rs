use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{CeremonialStructure, Workspace, rule::Rule};

fn temporary(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-design-ceremonial-{name}-{}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    root
}

fn write(root: &Path, relative: &str, source: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

fn findings(root: &Path) -> Vec<crate::Finding> {
    let workspace = Workspace::load([root.to_path_buf()]).unwrap();
    CeremonialStructure.check(&workspace).unwrap()
}

#[test]
fn finds_transparent_namespace() {
    let root = temporary("namespace");
    write(&root, "src/lib.rs", "mod shell;");
    write(
        &root,
        "src/shell/mod.rs",
        "mod process;\npub(crate) use process::{Child, Status};",
    );
    write(&root, "src/shell/process.rs", "pub struct Child;\npub struct Status;");

    let findings = findings(&root);

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "shell");
    assert!(findings[0].message.contains("transparent re-exports"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn keeps_used_namespaces() {
    let root = temporary("namespace-boundaries");
    write(
        &root,
        "src/lib.rs",
        "pub mod public;\nmod configured;\nmod private_api;\nmod used;\nfn consume(_: used::Value) {}",
    );
    for module in ["public", "used"] {
        write(
            &root,
            &format!("src/{module}/mod.rs"),
            "mod value;\npub use value::Value;",
        );
        write(&root, &format!("src/{module}/value.rs"), "pub struct Value;");
    }
    write(
        &root,
        "src/configured/mod.rs",
        "#[cfg(unix)] mod value;\npub use value::Value;",
    );
    write(&root, "src/configured/value.rs", "pub struct Value;");
    write(&root, "src/private_api/mod.rs", "mod value;\npub use value::Value;");
    write(&root, "src/private_api/value.rs", "pub struct Value;");

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn finds_marker_traits() {
    let root = temporary("markers");
    write(
        &root,
        "src/lib.rs",
        r#"
trait Forgotten {}
trait Anything {}
impl<T> Anything for T {}
"#,
    );

    let findings = findings(&root);
    let subjects = findings
        .iter()
        .map(|finding| finding.subject.as_str())
        .collect::<Vec<_>>();

    assert_eq!(subjects, ["Forgotten", "Anything"]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn keeps_marker_contracts() {
    let root = temporary("marker-contracts");
    write(
        &root,
        "src/lib.rs",
        r#"
pub trait ExternalTag {}
trait Selective {}
struct Linux;
impl Selective for Linux {}
trait Required {}
fn require<T: Required>() {}
trait Aggregate: Send + Sync {}
unsafe trait Safety {}
"#,
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn finds_forwarding_wrapper() {
    let root = temporary("wrapper");
    write(
        &root,
        "src/lib.rs",
        r#"
struct Store;
impl Store {
    fn read(&self, id: u64) -> usize { id as usize }
    fn write(&mut self, id: u64) -> bool { id > 0 }
    fn remove(&mut self, id: u64) -> bool { id > 0 }
}

struct Storage { inner: Store }
impl Storage {
    fn new(inner: Store) -> Self { Self { inner } }
    fn read(&self, id: u64) -> usize { self.inner.read(id) }
    fn write(&mut self, id: u64) -> bool { self.inner.write(id) }
    fn remove(&mut self, id: u64) -> bool { self.inner.remove(id) }
}
"#,
    );

    let findings = findings(&root);

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "Storage");
    assert!(findings[0].message.contains("identical names and signatures"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn keeps_public_contracts() {
    let root = temporary("wrapper-boundaries");
    write(
        &root,
        "src/lib.rs",
        r#"
struct Store;
impl Store {
    fn read(&self, id: u64) -> usize { id as usize }
    fn write(&self, id: u64) -> bool { id > 0 }
    fn remove(&self, id: u64) -> bool { id > 0 }
}

trait Port { fn read(&self, id: u64) -> usize; }
struct Adapter { inner: Store }
impl Adapter {
    fn read(&self, id: u64) -> usize { self.inner.read(id) }
    fn write(&self, id: u64) -> bool { self.inner.write(id) }
    fn remove(&self, id: u64) -> bool { self.inner.remove(id) }
}
impl Port for Adapter {
    fn read(&self, id: u64) -> usize { self.inner.read(id) }
}

struct Validated { inner: Store }
impl Validated {
    fn read(&self, id: u64) -> usize { self.inner.read(id) }
    fn write(&self, id: u64) -> bool { self.inner.write(id) }
    fn remove(&self, id: u64) -> bool { self.inner.remove(id) }
    fn validate(&self) -> bool { true }
}

pub struct Compatible { inner: Store }
impl Compatible {
    fn read(&self, id: u64) -> usize { self.inner.read(id) }
    fn write(&self, id: u64) -> bool { self.inner.write(id) }
    fn remove(&self, id: u64) -> bool { self.inner.remove(id) }
}

#[derive(serde::Serialize)]
struct Wire { inner: Store }
impl Wire {
    fn read(&self, id: u64) -> usize { self.inner.read(id) }
    fn write(&self, id: u64) -> bool { self.inner.write(id) }
    fn remove(&self, id: u64) -> bool { self.inner.remove(id) }
}
"#,
    );

    assert!(findings(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}
