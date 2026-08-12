use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{AccessorBloat, Rule, Workspace};

fn findings(source: &str) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-design-accessor-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"accessor-test\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let path: PathBuf = root.join("src/lib.rs");
    fs::write(&path, source).unwrap();
    let workspace = Workspace::load([path]).unwrap();
    let findings = AccessorBloat.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    findings
}

#[test]
fn reports_public_fields() {
    let findings = findings(
        r"
pub struct Metadata {
    pub labels: Vec<String>,
    pub options: Vec<String>,
}

impl Metadata {
    pub fn labels(&self) -> &Vec<String> { &self.labels }
    pub fn options(&self) -> Vec<String> { self.options.clone() }
    pub fn set_labels(&mut self, labels: Vec<String>) { self.labels = labels; }
}
",
    );
    assert_eq!(findings.len(), 3);
    assert!(
        findings
            .iter()
            .all(|finding| finding.message.contains("already access"))
    );
}

#[test]
fn reports_accessor_review() {
    let findings = findings(
        r"
pub struct Image {
    reference: String,
}

impl Image {
    pub fn reference(&self) -> &String { &self.reference }
    pub fn image_reference(&self) -> &String { &self.reference }
}
",
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "Image::image_reference");
    assert_eq!(findings[0].related.len(), 1);
}

#[test]
fn preserves_field_boundaries() {
    let findings = findings(
        r"
pub struct Account {
    balance: i64,
    tags: Vec<String>,
}

impl Account {
    pub fn balance(&self) -> i64 { self.balance }
    pub fn tags(&self) -> &[String] { &self.tags }
    pub fn set_balance(&mut self, balance: i64) {
        assert!(balance >= 0);
        self.balance = balance;
    }
    pub fn debit(&mut self, amount: i64) { self.balance -= amount; }
}
",
    );
    assert!(findings.is_empty());
}

#[test]
fn documentation_public_access() {
    let findings = findings(
        r"
/// Plain data.
pub struct Data {
    /// Public value.
    pub value: u32,
}

impl Data {
    /// Returns the already-public value.
    pub fn value(&self) -> u32 { self.value }
}
",
    );
    assert_eq!(findings.len(), 1);
}

#[test]
fn ignores_derived_values() {
    let findings = findings(
        r#"
pub struct Value {
    pub bytes: Vec<u8>,
    pub first: u32,
    pub second: u32,
}

impl Value {
    pub fn bytes(&self) -> &[u8] { self.bytes.as_slice() }
    pub fn total(&self) -> u32 { self.first + self.second }
    pub fn encoded(&self) -> String { format!("{:?}", self.bytes) }
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn ignores_compatibility_markers() {
    let findings = findings(
        r#"
pub trait Named { fn name(&self) -> &String; }
pub struct Model { pub name: String }

impl Named for Model {
    fn name(&self) -> &String { &self.name }
}

impl Model {
    #[deprecated(note = "compatibility alias")]
    pub fn old_name(&self) -> &String { &self.name }

    #[cfg(target_os = "linux")]
    pub fn platform_name(&self) -> &String { &self.name }

    #[cfg_attr(target_os = "macos", inline)]
    pub fn configured_name(&self) -> &String { &self.name }
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn ignores_ffi_models() {
    let findings = findings(
        r"
#[derive(serde::Serialize)]
pub struct Wire { pub value: String }
impl Wire {
    pub fn value(&self) -> &String { &self.value }
}

#[repr(C)]
pub struct Ffi { pub value: u32 }
impl Ffi {
    pub fn value(&self) -> u32 { self.value }
}
",
    );
    assert!(findings.is_empty());
}

#[test]
fn compare_shapes_duplicates() {
    let findings = findings(
        r"
pub struct Data { values: Vec<String> }
impl Data {
    pub fn values(&self) -> &Vec<String> { &self.values }
    pub fn values_owned(&self) -> Vec<String> { self.values.clone() }
}
",
    );
    assert!(findings.is_empty());
}

#[test]
fn compare_contracts_duplicates() {
    let findings = findings(
        r"
pub struct Data { value: String }
impl Data {
    pub fn value(&self) -> &String { &self.value }
    pub fn value_str(&self) -> &str { &self.value }
}
",
    );
    assert!(findings.is_empty());
}
