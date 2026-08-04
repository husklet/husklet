use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{ModelDuplication, Rule, Workspace};

fn findings(source: &str) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-design-model-duplication-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"models\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let path = root.join("src/lib.rs");
    fs::write(&path, source).unwrap();
    let workspace = Workspace::load([PathBuf::from(&path)]).unwrap();
    let findings = ModelDuplication.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    findings
}

fn package_findings(
    first_domain: &str,
    first: (&str, &str),
    second_domain: &str,
    second: (&str, &str, Option<&str>),
) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-design-model-packages-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let first_root = root.join("src").join(first_domain).join(first.0);
    let second_root = root.join("src").join(second_domain).join(second.0);
    for package in [&first_root, &second_root] {
        fs::create_dir_all(package.join("src")).unwrap();
    }
    fs::write(
        first_root.join("Cargo.toml"),
        format!("[package]\nname = \"{}\"\nversion = \"0.0.0\"\n", first.0),
    )
    .unwrap();
    let dependency = second
        .2
        .map(|name| format!("\n[dependencies]\n{name} = {{ path = \"{}\" }}\n", first_root.display()))
        .unwrap_or_default();
    fs::write(
        second_root.join("Cargo.toml"),
        format!("[package]\nname = \"{}\"\nversion = \"0.0.0\"\n{dependency}", second.0),
    )
    .unwrap();
    let first_source = first_root.join("src/lib.rs");
    let second_source = second_root.join("src/lib.rs");
    fs::write(&first_source, first.1).unwrap();
    fs::write(&second_source, second.1).unwrap();
    let workspace = Workspace::load([first_source, second_source]).unwrap();
    let findings = ModelDuplication.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    findings
}

#[test]
fn reports_bearing_model() {
    let found = findings(
        r#"
#[derive(serde::Serialize, serde::Deserialize)]
pub struct WireImage {
    pub id: u64,
    pub name: String,
    pub rootfs: String,
    pub arch: String,
}
pub struct Image {
    id: u64,
    name: String,
    rootfs: String,
    arch: String,
}
impl Image {
    pub fn validate(&self) -> bool { !self.name.is_empty() }
}
impl From<Image> for WireImage {
    fn from(image: Image) -> Self {
        Self {
            id: image.id,
            name: image.name,
            rootfs: image.rootfs,
            arch: image.arch,
        }
    }
}
"#,
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].severity, crate::Severity::Warning);
    assert_eq!(found[0].related.len(), 2);
}

#[test]
fn resolves_field_renames() {
    let found = findings(
        r#"
#[derive(serde::Serialize)]
pub struct ApiNode {
    #[serde(rename = "id")]
    pub identifier: Identifier,
    pub name: String,
    pub address: String,
}
pub struct Node {
    id: u64,
    name: String,
    address: String,
}
type Identifier = u64;
impl Node {
    pub fn name(&self) -> &str { &self.name }
}
"#,
    );
    assert_eq!(found.len(), 1);
}

#[test]
fn ignores_abi_layouts() {
    let found = findings(
        r#"
#[derive(serde::Serialize)]
pub struct ImageResponse {
    pub id: u64,
    pub name: String,
    pub rootfs: String,
}
#[repr(C)]
#[derive(serde::Serialize)]
pub struct WireHeader {
    pub id: u64,
    pub name: String,
    pub rootfs: String,
}
pub struct Image {
    id: u64,
    name: String,
    rootfs: String,
}
impl Image {
    pub fn validate(&self) -> bool { !self.name.is_empty() }
}
"#,
    );
    assert!(found.is_empty());
}

#[test]
fn ignores_concept_evidence() {
    let found = findings(
        r#"
#[derive(serde::Serialize)]
pub struct WireNetwork {
    pub id: u64,
    pub name: String,
    pub path: String,
}
pub struct Volume {
    id: u64,
    name: String,
    path: String,
}
impl Volume {
    pub fn mount(&self) {}
}
"#,
    );
    assert!(found.is_empty());
}

#[test]
fn ignores_composes_base() {
    let found = findings(
        r#"
#[derive(serde::Serialize)]
pub struct WireImage {
    pub image: Image,
    pub source: String,
    pub score: u64,
}
pub struct Image {
    id: u64,
    name: String,
    rootfs: String,
}
impl Image {
    pub fn validate(&self) -> bool { !self.name.is_empty() }
}
"#,
    );
    assert!(found.is_empty());
}

#[test]
fn ignores_overlap_shapes() {
    let found = findings(
        r#"
#[derive(serde::Serialize)]
pub struct WireImage {
    pub id: u64,
    pub name: String,
    pub rootfs: String,
    pub created: u64,
    pub labels: Vec<String>,
}

pub struct Image {
    id: u64,
    name: String,
    rootfs: String,
    arch: String,
    command: Vec<String>,
}
impl Image {
    pub fn validate(&self) -> bool { !self.name.is_empty() }
}
"#,
    );
    assert!(found.is_empty());
}

#[test]
fn reports_dependency_edge() {
    let owner = r#"
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ApiImage {
    pub id: u64,
    pub name: String,
    pub rootfs: String,
    pub arch: String,
}
"#;
    let client = r#"
#[derive(serde::Serialize, serde::Deserialize)]
pub struct WireImage {
    pub id: u64,
    pub name: String,
    pub rootfs: String,
    pub arch: String,
}
"#;
    let found = package_findings(
        "containers",
        ("api-owner", owner),
        "containers",
        ("api-client", client, Some("api-owner")),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].subject, "WireImage_ApiImage");
}

#[test]
fn ignores_packages_domains() {
    let model = r#"
#[derive(serde::Serialize)]
pub struct ApiImage {
    pub id: u64,
    pub name: String,
    pub rootfs: String,
}
"#;
    let found = package_findings("containers", ("api-owner", model), "gpu", ("unrelated", model, None));
    assert!(found.is_empty());
}
