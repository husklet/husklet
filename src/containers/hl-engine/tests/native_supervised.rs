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

fn run(executable: &Path, arguments: &[&str], selected: bool) -> (i32, Vec<u8>) {
    let mut options = Options::default();
    if selected {
        options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    }
    let output = Arc::new(Output::default());
    let plan = RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: std::iter::once(executable.as_os_str().as_encoded_bytes().to_vec())
            .chain(arguments.iter().map(|value| value.as_bytes().to_vec()))
            .collect(),
        environment: Vec::new(),
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

#[test]
fn option_is_default_off_and_true_exits_identically() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(run(&executable, &[], false), (0, Vec::new()));
    assert_eq!(run(&executable, &[], true), (0, Vec::new()));
}
