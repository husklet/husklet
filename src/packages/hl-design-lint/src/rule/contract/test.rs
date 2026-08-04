use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{BroadTrait, Rule, Workspace};

fn findings(source: &str) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-design-broad-trait-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"broad-trait-test\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let path: PathBuf = root.join("src/lib.rs");
    fs::write(&path, source).unwrap();
    let workspace = Workspace::load([path]).unwrap();
    let findings = BroadTrait.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    findings
}

#[test]
fn reports_capability_clusters() {
    let findings = findings(
        r#"
pub trait RuntimeService {
    fn create(&self, config: Config) -> Result<Id, Error>;
    fn remove(&self, id: Id) -> Result<(), Error>;
    fn start(&self, id: Id) -> Result<(), Error>;
    fn stop(&self, id: Id) -> Result<(), Error>;
    fn configure(&self, id: Id, config: Config) -> Result<(), Error>;
    fn update(&self, id: Id, config: Config) -> Result<(), Error>;
    fn inspect(&self, id: Id) -> Result<Snapshot, Error>;
    fn status(&self, id: Id) -> Result<Status, Error>;
}

impl RuntimeService for Host {
    fn create(&self, _: Config) -> Result<Id, Error> { todo!() }
    fn remove(&self, _: Id) -> Result<(), Error> { todo!() }
    fn start(&self, _: Id) -> Result<(), Error> { todo!() }
    fn stop(&self, _: Id) -> Result<(), Error> { todo!() }
    fn configure(&self, _: Id, _: Config) -> Result<(), Error> { todo!() }
    fn update(&self, _: Id, _: Config) -> Result<(), Error> { todo!() }
    fn inspect(&self, _: Id) -> Result<Snapshot, Error> { todo!() }
    fn status(&self, _: Id) -> Result<Status, Error> { todo!() }
}
"#,
    );
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    assert_eq!(finding.subject, "RuntimeService");
    assert!(finding.message.contains("4 distinct capability clusters"));
    assert!(
        finding
            .related
            .iter()
            .any(|related| related.label.contains("implemented by `Host`"))
    );
    let review = finding.review.as_ref().unwrap();
    assert!(
        review
            .metadata
            .iter()
            .any(|(name, value)| name == "Capability clusters" && value.contains("lifecycle: start, stop"))
    );
}

#[test]
fn noun_capability_presenters() {
    let findings = findings(
        r#"
pub trait Presenter {
    fn poll_events(&mut self);
    fn take_events(&mut self) -> Vec<Event>;
    fn set_clipboard_text(&mut self, text: &str);
    fn take_clipboard_text(&mut self) -> Option<String>;
    fn reconcile_window(&mut self, state: &WindowState);
    fn destroy_window(&mut self, id: SurfaceId);
    fn begin_interaction(&mut self, id: SurfaceId, interaction: Interaction);
    fn present(&mut self, image: Image) -> Feedback;
}
"#,
    );
    assert_eq!(findings.len(), 1);
    let clusters = findings[0]
        .review
        .as_ref()
        .unwrap()
        .metadata
        .iter()
        .find(|(name, _)| name == "Capability clusters")
        .map(|(_, value)| value.as_str())
        .unwrap();
    assert!(clusters.contains("clipboard: set_clipboard_text, take_clipboard_text"));
    assert!(clusters.contains("events: poll_events, take_events"));
    assert!(clusters.contains("window: reconcile_window, destroy_window, begin_interaction"));
}

#[test]
fn count_is_insufficient() {
    let findings = findings(
        r#"
pub trait RecordRepository {
    fn create(&self, value: Record) -> Result<Id, Error>;
    fn open(&self, id: Id) -> Result<Record, Error>;
    fn read(&self, id: Id) -> Result<Record, Error>;
    fn write(&self, value: Record) -> Result<(), Error>;
    fn save(&self, value: Record) -> Result<(), Error>;
    fn load(&self, id: Id) -> Result<Record, Error>;
    fn delete(&self, id: Id) -> Result<(), Error>;
    fn list(&self) -> Result<Vec<Record>, Error>;
    fn find(&self, query: Query) -> Result<Vec<Record>, Error>;
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn preserves_renderer_contracts() {
    let findings = findings(
        r#"
pub trait Codec {
    fn encode_header(&self, value: Header) -> Bytes;
    fn encode_body(&self, value: Body) -> Bytes;
    fn encode_tail(&self, value: Tail) -> Bytes;
    fn decode_header(&self, value: Bytes) -> Header;
    fn decode_body(&self, value: Bytes) -> Body;
    fn decode_tail(&self, value: Bytes) -> Tail;
    fn serialize(&self, value: Frame) -> Bytes;
    fn deserialize(&self, value: Bytes) -> Frame;
}
pub trait Visitor {
    fn visit_a(&mut self, value: A);
    fn visit_b(&mut self, value: B);
    fn visit_c(&mut self, value: C);
    fn visit_d(&mut self, value: D);
    fn visit_e(&mut self, value: E);
    fn visit_f(&mut self, value: F);
    fn visit_g(&mut self, value: G);
    fn visit_h(&mut self, value: H);
}
pub trait Renderer {
    fn render_a(&mut self, value: A);
    fn render_b(&mut self, value: B);
    fn draw_a(&mut self, value: A);
    fn draw_b(&mut self, value: B);
    fn present_a(&mut self, value: A);
    fn present_b(&mut self, value: B);
    fn frame_a(&mut self, value: A);
    fn frame_b(&mut self, value: B);
}
pub trait SurfaceProtocol {
    fn create_surface(&self, id: SurfaceId);
    fn remove_surface(&self, id: SurfaceId);
    fn configure_surface(&self, id: SurfaceId);
    fn update_surface(&self, id: SurfaceId);
    fn present_surface(&self, id: SurfaceId);
    fn commit_surface(&self, id: SurfaceId);
    fn inspect_surface(&self, id: SurfaceId) -> SurfaceSnapshot;
    fn status_surface(&self, id: SurfaceId) -> SurfaceStatus;
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn ignores_test_traits() {
    let findings = findings(
        r#"
pub unsafe trait Abi {
    fn create(&self); fn remove(&self);
    fn start(&self); fn stop(&self);
    fn inspect(&self); fn status(&self);
    fn configure(&self); fn update(&self);
}
pub trait Hidden: sealed::Sealed {
    fn create(&self); fn remove(&self);
    fn start(&self); fn stop(&self);
    fn inspect(&self); fn status(&self);
    fn configure(&self); fn update(&self);
}
#[cfg(test)]
pub trait Fixture {
    fn create(&self); fn remove(&self);
    fn start(&self); fn stop(&self);
    fn inspect(&self); fn status(&self);
    fn configure(&self); fn update(&self);
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn ignores_name_guessing() {
    let findings = findings(
        r#"
pub trait Odd {
    fn create(&self);
    fn remove(&self);
    fn start(&self);
    fn stop(&self);
    fn alpha(&self);
    fn beta(&self);
    fn gamma(&self);
    fn delta(&self);
    fn epsilon(&self);
    fn zeta(&self);
}
"#,
    );
    assert!(findings.is_empty());
}
