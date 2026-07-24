use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    model::Finding,
    rule::{FiniteStateString, Rule},
    source::Workspace,
};

fn findings(source: &str) -> Vec<Finding> {
    findings_in("src/lib.rs", source)
}

fn findings_in(relative: &str, source: &str) -> Vec<Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-finite-state-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, source).unwrap();
    let workspace = Workspace::load([PathBuf::from(&path)]).unwrap();
    let findings = FiniteStateString.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    findings
}

#[test]
fn reports_three_matched_field_states() {
    let findings = findings(
        r#"
struct Upload { status: String }
impl Upload {
    fn finished(&self) -> bool {
        match self.status.as_str() {
            "preparing" => false,
            "pushing" => false,
            "pushed" => true,
            _ => false,
        }
    }
}
"#,
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "Upload::status");
    assert!(findings[0].message.contains("3 string literals"));
}

#[test]
fn reports_scoped_binding_compared_to_three_values() {
    let findings = findings(
        r#"
fn ready(status: &str) -> bool {
    let mut phase = status;
    phase = "pending";
    phase = "running";
    phase = "complete";
    phase == "pending" || phase == "running" || phase == "complete"
}
"#,
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "ready::phase");
}

#[test]
fn ignores_two_values_and_literal_assignments_without_decisions() {
    let findings = findings(
        r#"
fn binary(status: &str) -> bool {
    status == "on" || status == "off"
}
fn labels() {
    let mut phase = "one";
    phase = "two";
    phase = "three";
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn reports_persistent_struct_field_states_from_construction() {
    let findings = findings(
        r#"
struct Lifecycle { state: &'static str }
fn lifecycle(code: u8) -> Lifecycle {
    match code {
        0 => Lifecycle { state: "created" },
        1 => Lifecycle { state: "running" },
        2 => Lifecycle { state: "paused" },
        _ => Lifecycle { state: "exited" },
    }
}
"#,
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "Lifecycle::state");
}

#[test]
fn reports_repeated_state_setter_targets() {
    let findings = findings(
        r#"
struct Job;
impl Job {
    fn set_status(&mut self, _: &str) {}
}
fn advance(job: &mut Job) {
    job.set_status("queued");
    job.set_status("running");
    job.set_status("complete");
}
"#,
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "advance::status");
}

#[test]
fn ignores_open_ended_text_identifiers_and_paths() {
    let findings = findings(
        r#"
fn render(message: &str, path: &str, id: &str) -> bool {
    message == "starting" || message == "running" || message == "done"
        || path == "/a" || path == "/b" || path == "/c"
        || id == "one" || id == "two" || id == "three"
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn ignores_test_modules() {
    let findings = findings(
        r#"
#[cfg(test)]
mod tests {
    fn state(status: &str) -> bool {
        matches!(status, "a" | "b" | "c")
    }
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn reports_closed_state_inside_protocol_namespace() {
    let findings = findings_in(
        "src/protocol/status.rs",
        r#"
pub struct Transfer { status: String }
impl Transfer {
    pub fn complete(&self) -> bool {
        match self.status.as_str() {
            "preparing" => false,
            "sending" => false,
            "complete" => true,
            _ => self.status.is_empty(),
        }
    }
}
"#,
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].subject, "Transfer::status");
}

#[test]
fn ignores_open_protocol_state_that_preserves_unknown_value() {
    let findings = findings_in(
        "src/protocol/status.rs",
        r#"
pub enum Status {
    Preparing,
    Sending,
    Complete,
    Unknown(String),
}
pub struct Transfer { status: String }
impl Transfer {
    pub fn status(&self) -> Status {
        match self.status.as_str() {
            "preparing" => Status::Preparing,
            "sending" => Status::Sending,
            "complete" => Status::Complete,
            unknown => Status::Unknown(unknown.to_owned()),
        }
    }
}
"#,
    );
    assert!(findings.is_empty());
}

#[test]
fn location_and_review_include_vocabulary_evidence() {
    let findings = findings(
        r#"
struct Process { phase: String }
impl Process {
fn transition(&self) -> bool {
    match self.phase.as_str() {
        "queued" => false,
        "active" => false,
        "finished" => true,
        _ => false,
    }
}
}
"#,
    );
    let finding = &findings[0];
    assert!(finding.location.source.contains("\"queued\""));
    assert!(finding.related.len() >= 2);
    let review = finding.review.as_ref().unwrap();
    assert!(review
        .metadata
        .iter()
        .any(|(key, value)| key == "Values" && value.contains("finished")));
}
