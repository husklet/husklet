#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use hl_engine::{
    activation::GuestIsa,
    composition::{StandardStream, StandardStreamPort, StandardStreams},
    launcher::plan::RuntimePlan,
    options::Options,
    runtime::Engine,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

#[derive(Default)]
struct Output(Mutex<Vec<u8>>);

impl StandardStreamPort for Output {
    fn write(&self, _: StandardStream, input: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(input);
        Ok(input.len())
    }
    fn close(&self) {}
}

fn fixture(directory: &Path) -> PathBuf {
    let output = directory.join("native-supervised-fixture");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_supervised.c");
    let status = std::process::Command::new("x86_64-linux-gnu-gcc")
        .args(["-static-pie", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap();
    assert!(status.success());
    output
}

fn run_configured(
    executable: &Path,
    arguments: &[&str],
    selected: bool,
    refusal: Option<&str>,
    environment: Vec<Vec<u8>>,
) -> (i32, Vec<u8>) {
    let mut options = Options::default();
    if selected {
        options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    }
    if let Some(refusal) = refusal {
        options.set("HL_NATIVE_SUPERVISED_REFUSE", refusal, true).unwrap();
    }
    let output = Arc::new(Output::default());
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: std::iter::once(executable.as_os_str().as_encoded_bytes().to_vec())
            .chain(arguments.iter().map(|value| value.as_bytes().to_vec()))
            .collect(),
        environment,
        result_path: None,
        options,
    };
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    let status = engine.wait().unwrap().guest_status;
    engine.destroy().unwrap();
    let bytes = output.0.lock().unwrap().clone();
    (status, bytes)
}

fn run_with_refusal(executable: &Path, arguments: &[&str], selected: bool, refusal: Option<&str>) -> (i32, Vec<u8>) {
    run_configured(executable, arguments, selected, refusal, Vec::new())
}

fn run(executable: &Path, arguments: &[&str], selected: bool) -> (i32, Vec<u8>) {
    run_with_refusal(executable, arguments, selected, None)
}

#[test]
fn option_is_default_off_and_true_exits_identically() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(run(&executable, &[], false), (0, Vec::new()));
    assert_eq!(run(&executable, &[], true), (0, Vec::new()));
}

#[test]
fn supervised_stdout_and_exit_status_keep_the_engine_contract() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output) = run(&executable, &["output"], true);
    assert_eq!(status, 23);
    assert_eq!(output, b"native-supervised");
}

#[test]
fn refusal_reaches_a_fork_descendant_without_fallback() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output) = run_with_refusal(&executable, &["descendant"], true, Some("39:38"));
    assert_eq!(status, 0);
    assert_eq!(output, b"descendant-supervised");
}

#[test]
fn supervisor_drains_an_orphaned_descendant() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output) = run_with_refusal(&executable, &["orphan"], true, Some("39:38"));
    assert_eq!(status, 0);
    assert_eq!(output, b"orphan-supervised");
}

#[test]
fn supervised_exec_uses_only_the_exact_guest_environment() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let environment = vec![b"NATIVE_SUPERVISED_ENV=line1\nline2\\tail".to_vec()];
    let (status, output) = run_configured(&executable, &["environment"], true, None, environment);
    assert_eq!(status, 0);
    assert_eq!(output, b"environment-exact");
}
