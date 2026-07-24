use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{rule::Rule, Workspace};

use super::GuiToolkitLeakage;

fn fixture(package: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-gui-leakage-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!("[package]\nname = \"{package}\"\nversion = \"0.0.0\"\n"),
    )
    .unwrap();
    root
}

fn write(root: &Path, relative: &str, source: &str) {
    let path = root.join("src").join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

fn findings(root: &Path) -> Vec<crate::Finding> {
    let workspace = Workspace::load([root.join("src")]).unwrap();
    GuiToolkitLeakage.check(&workspace).unwrap()
}

#[test]
fn detects_nested_native_types_and_import_aliases_in_public_api() {
    let root = fixture("hl-gui");
    write(
        &root,
        "lib.rs",
        r#"
use gtk as native;
use vte4::Terminal as NativeTerminal;

pub struct Public {
    pub callbacks: Vec<Box<dyn Fn(&native::Window) -> Option<NativeTerminal>>>,
}

pub trait Render {
    type Native: glib::ObjectExt;
    fn render<T: Into<gdk::RGBA>>(&self, value: T) -> Result<(), native::glib::Error>;
}
"#,
    );
    let values = findings(&root);
    assert_eq!(values.len(), 3);
    assert!(values.iter().any(|finding| {
        finding.subject == "Public" && finding.message.contains("gtk (native :: Window)")
    }));
    assert!(values.iter().any(|finding| {
        finding.subject == "Render::Native" && finding.message.contains("glib")
    }));
    assert!(values.iter().any(|finding| {
        finding.subject == "Render::render"
            && finding.message.contains("gdk")
            && finding.message.contains("gtk")
    }));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn detects_reachable_module_methods_but_ignores_private_surfaces() {
    let root = fixture("hl-gui");
    write(
        &root,
        "lib.rs",
        "pub mod adapter;\nmod private;\npub(crate) mod crate_only;",
    );
    write(
        &root,
        "adapter.rs",
        r#"
pub struct Renderer(gtk::Window);
impl Renderer {
    pub fn native(&self) -> &gtk::Window { &self.0 }
    fn internal(&self, value: gtk::Button) { let _ = value; }
}
pub(crate) fn crate_only() -> gtk::Button { todo!() }
struct Hidden;
impl Hidden { pub fn misleading() -> gtk::Window { todo!() } }
"#,
    );
    write(
        &root,
        "private.rs",
        "pub fn hidden() -> gtk::Window { todo!() }",
    );
    write(
        &root,
        "crate_only.rs",
        "pub fn hidden() -> gtk::Window { todo!() }",
    );
    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "Renderer::native");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ignores_other_packages_and_similarly_named_owned_modules() {
    let other = fixture("husklet");
    write(
        &other,
        "lib.rs",
        "pub fn compose(parent: gtk::Window) -> gtk::Window { parent }",
    );
    assert!(findings(&other).is_empty());
    fs::remove_dir_all(other).unwrap();

    let gui = fixture("hl-gui");
    write(
        &gui,
        "lib.rs",
        r#"
pub mod gtk { pub struct Window; }
pub mod glib { pub struct Error; }
pub fn owned(value: crate::gtk::Window) -> crate::glib::Error { todo!() }
"#,
    );
    assert!(findings(&gui).is_empty());
    fs::remove_dir_all(gui).unwrap();
}

#[test]
fn reports_public_aliases_and_qualified_paths_with_signature_context() {
    let root = fixture("hl-gui");
    write(
        &root,
        "lib.rs",
        r#"
pub type Native = Option<gtk::Button>;
pub fn qualified() -> <gtk::Window as Widget>::State { todo!() }
"#,
    );
    let values = findings(&root);
    assert_eq!(values.len(), 2);
    assert!(values.iter().all(|finding| finding
        .review
        .as_ref()
        .unwrap()
        .metadata
        .iter()
        .any(|(key, value)| key == "signature" && !value.is_empty())));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn detects_toolkits_in_declaration_bounds_and_public_reexports() {
    let root = fixture("hl-gui");
    write(
        &root,
        "lib.rs",
        r#"
pub use gtk::Button as NativeButton;
pub struct Generic<T: gtk::glib::ObjectType>(T);
pub trait NativeRender: gdk::prelude::GdkCairoContextExt {}
"#,
    );
    let values = findings(&root);
    assert_eq!(values.len(), 3);
    assert!(values
        .iter()
        .any(|finding| finding.message.contains("re-export")
            && finding.message.contains("gtk (gtk::Button)")));
    assert!(values.iter().any(|finding| {
        finding.subject == "Generic" && finding.message.contains("gtk :: glib :: ObjectType")
    }));
    assert!(values.iter().any(|finding| {
        finding.subject == "NativeRender"
            && finding
                .message
                .contains("gdk :: prelude :: GdkCairoContextExt")
    }));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn follows_selective_reexports_without_scanning_unexported_items() {
    let root = fixture("hl-gui");
    write(&root, "lib.rs", "mod adapter;\npub use adapter::Exported;");
    write(
        &root,
        "adapter.rs",
        r#"
pub struct Exported;
impl Exported {
    pub fn native(&self) -> gtk::Window { todo!() }
}
pub struct Internal;
impl Internal {
    pub fn native(&self) -> gtk::Button { todo!() }
}
"#,
    );
    let values = findings(&root);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "Exported::native");
    fs::remove_dir_all(root).unwrap();
}
