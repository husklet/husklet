#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use hl_engine::{
    activation::GuestIsa,
    composition::{StandardStream, StandardStreamPort, StandardStreams},
    launcher::plan::RuntimePlan,
    options::Options,
    runtime::Engine,
};
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

struct Output {
    stdout: Mutex<Vec<u8>>,
    stderr: Mutex<Vec<u8>>,
    input: Mutex<Vec<u8>>,
}

impl Default for Output {
    fn default() -> Self {
        Self { stdout: Mutex::new(Vec::new()), stderr: Mutex::new(Vec::new()), input: Mutex::new(Vec::new()) }
    }
}

impl StandardStreamPort for Output {
    fn read(&self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let mut input = self.input.lock().unwrap();
        let count = bytes.len().min(input.len());
        bytes[..count].copy_from_slice(&input[..count]);
        input.drain(..count);
        Ok(count)
    }
    fn write(&self, stream: StandardStream, input: &[u8]) -> std::io::Result<usize> {
        match stream {
            StandardStream::Stdout => self.stdout.lock().unwrap().extend_from_slice(input),
            StandardStream::Stderr => self.stderr.lock().unwrap().extend_from_slice(input),
        }
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
) -> (i32, Vec<u8>, Vec<u8>) {
    let mut options = Options::default();
    if selected {
        options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    }
    if let Some(refusal) = refusal {
        options.set("HL_NATIVE_SUPERVISED_REFUSE", refusal, true).unwrap();
    }
    let output = Arc::new(Output::default());
    output.input.lock().unwrap().extend_from_slice(b"pipe");
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
    let stdout = output.stdout.lock().unwrap().clone();
    let stderr = output.stderr.lock().unwrap().clone();
    (status, stdout, stderr)
}

fn run_with_refusal(executable: &Path, arguments: &[&str], selected: bool, refusal: Option<&str>) -> (i32, Vec<u8>, Vec<u8>) {
    run_configured(executable, arguments, selected, refusal, Vec::new())
}

fn run(executable: &Path, arguments: &[&str], selected: bool) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_refusal(executable, arguments, selected, None)
}

#[test]
fn option_is_default_off_and_true_exits_identically() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(run(&executable, &[], false), (0, Vec::new(), Vec::new()));
    assert_eq!(run(&executable, &[], true), (0, Vec::new(), Vec::new()));
}

#[test]
fn supervised_stdout_and_exit_status_keep_the_engine_contract() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output, error) = run(&executable, &["output"], true);
    assert_eq!(status, 23);
    assert_eq!(output, b"native-supervised");
    assert!(error.is_empty());
}

#[test]
fn refusal_reaches_a_fork_descendant_without_fallback() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output, _) = run_with_refusal(&executable, &["descendant"], true, Some("39:38"));
    assert_eq!(status, 0);
    assert_eq!(output, b"descendant-supervised");
}

#[test]
fn supervisor_drains_an_orphaned_descendant() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output, _) = run_with_refusal(&executable, &["orphan"], true, Some("39:38"));
    assert_eq!(status, 0);
    assert_eq!(output, b"orphan-supervised");
}

#[test]
fn supervised_exec_uses_only_the_exact_guest_environment() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let environment = vec![b"NATIVE_SUPERVISED_ENV=line1\nline2\\tail".to_vec()];
    let (status, output, _) = run_configured(&executable, &["environment"], true, None, environment);
    assert_eq!(status, 0);
    assert_eq!(output, b"environment-exact");
}

#[test]
fn supervised_stream_projection_supports_read_writev_stderr_and_dup2() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output, error) = run(&executable, &["streams"], true);
    assert_eq!(status, 0);
    assert_eq!(output, b"writev-dup");
    assert_eq!(error, b"stderr");
}

#[test]
fn readiness_precedes_exec_permission_failure_and_result_reports_126() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(run(&executable, &[], true), (126, Vec::new(), Vec::new()));
}
