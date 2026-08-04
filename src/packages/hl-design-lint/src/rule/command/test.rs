use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use crate::rule::Rule;

use super::PlatformCommand;

fn findings_in(package_name: &str, source: &str, relative: &str) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock follows Unix epoch")
        .as_nanos();
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("hl-command-rule-{}-{nonce}-{sequence}", std::process::id()));
    let package = root.join(format!("src/packages/{package_name}"));
    let path = package.join(relative);
    fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create fixture");
    fs::write(
        package.join("Cargo.toml"),
        format!("[package]\nname = \"{package_name}\"\nversion = \"0.0.0\"\n"),
    )
    .expect("write manifest");
    fs::write(&path, source).expect("write fixture");
    let workspace = crate::source::Workspace::load([PathBuf::from(&path)]).expect("parse fixture");
    let values = PlatformCommand.check(&workspace).expect("run rule");
    fs::remove_dir_all(root).expect("remove fixture");
    values
}

fn findings(source: &str, relative: &str) -> Vec<crate::Finding> {
    findings_in("fixture", source, relative)
}

#[test]
fn renamed_imports() {
    let values = findings(
        r#"
use std::process::Command;
use std::process as host_process;
use tokio::{process::Command as AsyncCommand};
fn commands() {
    let _ = Command::new("git");
    let _ = host_process::Command::new("git");
    let _ = AsyncCommand::new("git");
    let _ = std::process::Command::new("git");
    let _ = tokio::process::Command::new("git");
}
"#,
        "src/lib.rs",
    );
    assert_eq!(values.len(), 5);
    assert!(values.iter().all(crate::Finding::is_violation));
    assert!(values
        .iter()
        .all(|finding| finding.message.contains("outside an application")));
}

#[test]
fn process_models_are_not_host_commands() {
    let values = findings(
        r#"
struct Process;
impl Process { fn new(_: &str) -> Self { Self } }
fn guest() {
    let _ = Process::new("/bin/sh");
    let _ = hl_engine::Process::new("/bin/bash");
}
"#,
        "src/lib.rs",
    );
    assert!(values.is_empty());
}

#[test]
fn adapter_modules_are_boundaries() {
    let values = findings(
        r#"
mod adapters {
    use std::process::Command as HostCommand;
    fn run() { let _ = HostCommand::new("git"); }
}
mod model {
    fn run() { let _ = std::process::Command::new("git"); }
}
"#,
        "src/lib.rs",
    );
    assert_eq!(values.len(), 1);
    assert!(values[0].location.source.contains("std::process::Command"));
}

#[test]
fn engine_native_module_is_boundary() {
    let values = findings_in(
        "hl-engine",
        "fn run() { let _ = std::process::Command::new(\"guest\"); }",
        "src/native/fixture.rs",
    );
    assert!(values.is_empty());
    let values = findings_in(
        "hl-engine",
        "fn run() { let _ = std::process::Command::new(\"guest\"); }",
        "src/domain.rs",
    );
    assert_eq!(values.len(), 1);
}

#[test]
fn interpolated_shell_source_is_reported() {
    let values = findings(
        r#"
fn unsafe_shell(value: &str) {
    let _ = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("echo {value}"));
    let _ = tokio::process::Command::new("bash")
        .args(["-c", value]);
}
"#,
        "src/lib.rs",
    );
    assert_eq!(values.len(), 4);
    assert_eq!(
        values
            .iter()
            .filter(|finding| finding.subject.contains("interpolated script"))
            .count(),
        2
    );
}

#[test]
fn dynamic_shells_are_reported_at_executable_boundary() {
    let values = findings(
        r#"
fn main() {
    let _ = std::process::Command::new("cc").arg("--version").status();
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(concat!("echo ", "static"))
        .status();
    let value = "untrusted";
    let _ = std::process::Command::new("sh").arg("-c").arg(value).status();
}
"#,
        "build.rs",
    );
    assert_eq!(values.len(), 1);
    assert!(values[0].subject.contains("interpolated script"));
}

#[test]
fn test_commands_are_permitted() {
    let values = findings(
        r#"
#[cfg(test)]
mod tests {
    fn fixture() { let _ = std::process::Command::new("git"); }
}
#[test]
fn test_fixture() { let _ = tokio::process::Command::new("git"); }
"#,
        "src/lib.rs",
    );
    assert!(values.is_empty());
}

#[test]
fn staged_shell_arguments_follow_lexical_aliases() {
    let values = findings(
        r#"
use std::process::Command as HostCommand;

fn unsafe_build(value: &str) {
    let mut command = HostCommand::new("/bin/sh");
    command.arg("-c");
    command.arg(format!("echo {value}"));

    let mut alias = command;
    alias.args(["-c", value]);
}

fn safe_build() {
    let mut command = HostCommand::new("sh");
    command.arg("-c");
    command.arg(concat!("echo ", "static"));
}

fn unrelated(value: &str) {
    struct Builder;
    impl Builder { fn arg(&mut self, _: &str) {} }
    let mut command = Builder;
    command.arg("-c");
    command.arg(value);
}
"#,
        "build.rs",
    );
    assert_eq!(values.len(), 2);
    assert!(values.iter().all(|finding| finding.message.contains("staged shell")));
}
