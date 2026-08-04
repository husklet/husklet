#![cfg(target_os = "linux")]

use hl_engine::activation::{ActivationDescriptor, ActivationStreams, GuestIsa};
use hl_engine::engine::{Engine, ExitKind};
use hl_engine::launch_plan::{ConfigOrigin, DiagnosticsMode, LaunchMaterial};
use hl_engine::native_host::{ChildExit, LinuxHost, ProcessGroup, ProcessHandle, ProcessSignal, SpawnRequest};
use hl_engine::native_launcher::{NativeSelection, NativeWorkspace, StandardWorkspaceFiles};
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hl-native-launch-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn material(output: i32, activation: i32) -> LaunchMaterial {
    let mut wire = vec![0; 192 + 8];
    for (offset, value) in [(0, 0x484c_4346_u32), (4, 8), (8, 192), (12, 1), (108, 1)] {
        wire[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    wire[152..160].copy_from_slice(&1_u64.to_le_bytes());
    wire[193..200].copy_from_slice(b"guest\0\0");
    LaunchMaterial::from_validated_wire(
        &wire,
        ConfigOrigin::File(b"/original/config".to_vec()),
        ActivationStreams {
            input: ActivationDescriptor::INHERIT,
            output: ActivationDescriptor::new(output as u64).unwrap(),
            error: ActivationDescriptor::INHERIT,
        },
        Some(activation),
        DiagnosticsMode::Disabled,
    )
    .unwrap()
}

#[test]
fn native_launcher_inherits() {
    let temporary = TemporaryDirectory::new();
    let binaries = temporary.0.join("bin");
    let staging = temporary.0.join("staging");
    std::fs::create_dir(&binaries).unwrap();
    std::fs::create_dir(&staging).unwrap();
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_hl-native-child-fixture"),
        binaries.join("hl-aarch64"),
    )
    .unwrap();
    let output_path = temporary.0.join("output");
    let output = std::fs::File::create(&output_path).unwrap();
    let activation_path = temporary.0.join("activation");
    let activation = std::fs::File::create(&activation_path).unwrap();
    let sentinel_path = temporary.0.join("must-not-leak");
    let _sentinel = std::fs::File::create(&sentinel_path).unwrap();
    let material = material(output.as_raw_fd(), activation.as_raw_fd());
    let workspace = NativeWorkspace::new(staging.clone(), Arc::new(StandardWorkspaceFiles)).unwrap();
    let launcher = workspace.launcher(binaries, Arc::new(LinuxHost)).unwrap();
    let engine = Engine::new_material(GuestIsa::Aarch64, material, launcher, workspace);
    engine.start().unwrap();
    let exit = engine.wait().unwrap();
    assert_eq!(exit.kind, ExitKind::Code);
    assert_eq!(exit.guest_status, 0);
    let report = std::fs::read_to_string(output_path).unwrap();
    assert!(report.contains("argv="));
    assert!(report.contains(&format!("activation={}", activation.as_raw_fd())));
    assert!(report.contains(&activation_path.display().to_string()));
    assert!(!report.contains(&sentinel_path.display().to_string()));
    assert!(std::fs::read_dir(staging).unwrap().next().is_none());
}

#[test]
fn missing_runner_is() {
    let temporary = TemporaryDirectory::new();
    let binaries = temporary.0.join("empty-bin");
    let staging = temporary.0.join("staging");
    std::fs::create_dir(&binaries).unwrap();
    std::fs::create_dir(&staging).unwrap();
    let activation = std::fs::File::create(temporary.0.join("activation")).unwrap();
    let material = material(0, activation.as_raw_fd());
    let engine = NativeSelection::compose(
        GuestIsa::X86_64,
        material,
        staging.clone(),
        binaries,
        Arc::new(StandardWorkspaceFiles),
        Arc::new(LinuxHost),
    )
    .unwrap();
    engine.start().unwrap();
    assert_eq!(engine.wait().unwrap().guest_status, 127);
    assert_eq!(engine.wait().unwrap().guest_status, 127);
    assert!(std::fs::read_dir(staging).unwrap().next().is_none());
}

#[test]
fn fixture_honors_requested() {
    let host = Arc::new(LinuxHost);
    let program = CString::new(env!("CARGO_BIN_EXE_hl-native-child-fixture")).unwrap();
    let exit_request = SpawnRequest {
        program: program.clone(),
        arguments: Vec::new(),
        environment: vec![CString::new("HL_FIXTURE_EXIT=42").unwrap()],
        process_group: ProcessGroup::New,
        file_actions: Vec::new(),
    };
    let exit = ProcessHandle::spawn(Arc::clone(&host), &exit_request).unwrap();
    assert_eq!(exit.wait_blocking().unwrap(), ChildExit::Code(42));

    let block_request = SpawnRequest {
        program,
        arguments: Vec::new(),
        environment: vec![CString::new("HL_FIXTURE_BLOCK=1").unwrap()],
        process_group: ProcessGroup::New,
        file_actions: Vec::new(),
    };
    let blocked = ProcessHandle::spawn(host, &block_request).unwrap();
    blocked.signal_group(ProcessSignal::Terminate).unwrap();
    assert_eq!(blocked.wait_blocking().unwrap(), ChildExit::Signal(15));
}
