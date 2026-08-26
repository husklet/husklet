#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use hl_engine::{
    activation::GuestIsa,
    composition::{
        CheckpointSink, CheckpointSource, CompositionError, StandardStream, StandardStreamPort, StandardStreams,
    },
    engine::ExitKind,
    launcher::plan::{RuntimeBoxPolicy, RuntimePlan},
    options::Options,
    runtime::Engine,
};
use std::num::NonZeroU64;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

struct Output {
    stdout: Mutex<Vec<u8>>,
    stderr: Mutex<Vec<u8>>,
    input: Mutex<Vec<u8>>,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            stdout: Mutex::new(Vec::new()),
            stderr: Mutex::new(Vec::new()),
            input: Mutex::new(Vec::new()),
        }
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

fn isolated_policy() -> RuntimeBoxPolicy {
    RuntimeBoxPolicy { flags: 1 << 2, ..Default::default() }
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
    if arguments.contains(&"secure-jail") {
        options.set("HL_NATIVE_SUPERVISED_REFUSE", "999:38", true).unwrap();
    }
    if let Some(refusal) = refusal {
        options.set("HL_NATIVE_SUPERVISED_REFUSE", refusal, true).unwrap();
    }
    let output = Arc::new(Output::default());
    output.input.lock().unwrap().extend_from_slice(b"pipe");
    let plan = RuntimePlan {
        rootfs: selected.then(|| b"/".to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: std::iter::once(executable.as_os_str().as_encoded_bytes().to_vec())
            .chain(arguments.iter().map(|value| value.as_bytes().to_vec()))
            .collect(),
        environment,
        result_path: None,
        options,
        box_policy: if selected { isolated_policy() } else { Default::default() },
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

fn run_with_refusal(
    executable: &Path,
    arguments: &[&str],
    selected: bool,
    refusal: Option<&str>,
) -> (i32, Vec<u8>, Vec<u8>) {
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

#[test]
fn supervised_tracee_signal_keeps_public_signal_kind() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let mut options = Options::default();
    options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    let plan = RuntimePlan {
        rootfs: Some(b"/".to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec(), b"signal".to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: isolated_policy(),
    };
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()).unwrap();
    engine.start().unwrap();
    let exit = engine.wait().unwrap();
    assert_eq!(exit.kind, ExitKind::Signal);
    assert_eq!(exit.guest_status, libc::SIGTERM);
    engine.destroy().unwrap();
}

#[test]
fn supervised_filter_denies_sendmsg_after_listener_bootstrap() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output, error) = run(&executable, &["sendmsg-denied"], true);
    assert_eq!(status, 0);
    assert_eq!(output, b"sendmsg-denied");
    assert!(error.is_empty());
}

#[test]
fn supervised_projector_applies_nnp_and_denies_escape_syscalls() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output, _) = run(&executable, &["secure-jail"], true);
    assert_eq!(status, 0);
    assert_eq!(output, b"secure-jail");
}

#[test]
fn supervised_projector_confines_root_cwd_and_replaces_hostile_proc() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let root = work.path().join("root");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("tmp")).unwrap();
    std::fs::create_dir_all(root.join("proc")).unwrap();
    std::fs::write(root.join("proc/hostile"), b"host").unwrap();
    let executable = root.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.rootfs = Some(root.as_os_str().as_encoded_bytes().to_vec());
    plan.arguments.push(b"root-contract".to_vec());
    plan.box_policy.working_directory = Some(b"/tmp".to_vec());
    plan.box_policy.hostname = Some(b"husklet-native".to_vec());
    plan.box_policy.flags |= 1;
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    assert_eq!(engine.wait().unwrap().guest_status, 0);
    engine.destroy().unwrap();
    assert_eq!(*output.stdout.lock().unwrap(), b"root-contract");
}

#[test]
fn supervised_projector_mounts_read_only_source_and_read_write_output() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let root = work.path().join("root");
    let source = work.path().join("source");
    let output_directory = work.path().join("output");
    for path in [root.join("bin"), root.join("proc"), root.join("src"), root.join("out"), source.clone(), source.join("nested"), output_directory.clone()] {
        std::fs::create_dir_all(path).unwrap();
    }
    assert!(std::process::Command::new("mount").args(["-t", "tmpfs", "-o", "rw,suid,dev", "tmpfs"])
        .arg(source.join("nested")).status().unwrap().success());
    std::fs::write(source.join("input.c"), b"source\n").unwrap();
    let executable = root.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.rootfs = Some(root.as_os_str().as_encoded_bytes().to_vec());
    plan.arguments.push(b"volumes".to_vec());
    plan.box_policy.volumes = Some(
        format!("ro:/src:{},rw:/out:{}", source.display(), output_directory.display()).into_bytes(),
    );
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    assert_eq!(engine.wait().unwrap().guest_status, 0);
    engine.destroy().unwrap();
    assert_eq!(*output.stdout.lock().unwrap(), b"volumes");
    assert_eq!(std::fs::read(output_directory.join("result.o")).unwrap(), b"object\n");
    assert!(!source.join("blocked").exists());
    assert!(std::process::Command::new("umount").arg(source.join("nested")).status().unwrap().success());
}

#[test]
fn supervised_projector_refuses_volume_traversal_and_symlink_sources() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let source = work.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let symlink = work.path().join("source-link");
    std::os::unix::fs::symlink(&source, &symlink).unwrap();
    let specifications = [
        format!("rw:/tmp/../escape:{}", source.display()),
        format!("rw:/tmp:{}", symlink.display()),
        format!("rw:/proc:{}", source.display()),
        format!("rw:/tmp:{},ro:/tmp:{}", source.display(), source.display()),
        format!("rw:/tmp:{},ro:/tmp/nested:{}", source.display(), source.display()),
    ];
    for specification in specifications {
        let mut plan = selected_plan(&executable);
        plan.box_policy.volumes = Some(specification.into_bytes());
        let engine = Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()).unwrap();
        if engine.start().is_ok() {
            assert!(engine.wait().is_err());
        }
        engine.destroy().unwrap();
    }
}

#[test]
fn supervised_projector_applies_identity_empty_groups_and_typed_limits() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.arguments.push(b"identity-limits".to_vec());
    plan.box_policy.uid = 1234;
    plan.box_policy.gid = 2345;
    plan.box_policy.limits = Some(b"nofile=32:32,core=0:0".to_vec());
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    assert_eq!(engine.wait().unwrap().guest_status, 0);
    engine.destroy().unwrap();
    assert_eq!(*output.stdout.lock().unwrap(), b"identity-limits");
}

#[test]
fn supervised_projector_uses_distinct_mount_pid_net_uts_and_ipc_namespaces() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let environment = ["mnt", "pid", "net", "uts", "ipc"]
        .into_iter()
        .map(|name| {
            let value = std::fs::read_link(format!("/proc/self/ns/{name}")).unwrap();
            format!("HOST_{}_NS={}", name.to_ascii_uppercase(), value.display()).into_bytes()
        })
        .collect();
    let (status, output, _) = run_configured(&executable, &["namespaces"], true, None, environment);
    assert_eq!(status, 0);
    assert_eq!(output, b"namespaces");
}

#[test]
fn supervised_clone3_enosys_falls_back_for_pthread_create_and_join() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output, _) = run(&executable, &["pthread"], true);
    assert_eq!(status, 0);
    assert_eq!(output, b"pthread");
}

struct Checkpoints;
impl CheckpointSink for Checkpoints {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Ok(())
    }
    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(NonZeroU64::MIN)
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }
    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }
    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }
}
impl CheckpointSource for Checkpoints {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Ok(Vec::new())
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Ok(Vec::new())
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Ok(Vec::new())
    }
}

fn selected_plan(executable: &Path) -> RuntimePlan {
    let mut options = Options::default();
    options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    RuntimePlan {
        rootfs: Some(b"/".to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: isolated_policy(),
    }
}

#[test]
fn supervised_mode_explicitly_refuses_every_unsupported_policy_class() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let mut policies = Vec::new();
    policies.push(RuntimeBoxPolicy::default());
    let mut policy = isolated_policy(); policy.network_namespace = Some(b"shared".to_vec()); policies.push(policy);
    let mut policy = isolated_policy(); policy.network_bridge = Some(b"bridge0".to_vec()); policies.push(policy);
    let mut policy = isolated_policy(); policy.flags |= 1 << 3; policies.push(policy);
    let mut policy = isolated_policy(); policy.lower_layers = Some(b"/lower".to_vec()); policies.push(policy);
    let mut policy = isolated_policy(); policy.ip = Some(b"10.0.0.2".to_vec()); policies.push(policy);
    let mut policy = isolated_policy(); policy.egress_proxy = Some(b"proxy".to_vec()); policies.push(policy);
    let mut policy = isolated_policy(); policy.file_owners = Some(b"owners".to_vec()); policies.push(policy);
    let mut policy = isolated_policy(); policy.filesystem_generation = Some(b"generation".to_vec()); policies.push(policy);
    let mut policy = isolated_policy(); policy.checkpoint_policy = 1; policies.push(policy);
    let mut policy = isolated_policy(); policy.flags |= 1 << 1; policies.push(policy);
    for (index, policy) in policies.into_iter().enumerate() {
        let mut plan = selected_plan(&executable);
        plan.box_policy = policy;
        let engine = Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()).unwrap();
        if engine.start().is_ok() {
            if let Ok(result) = engine.wait() {
                assert_ne!(result.guest_status, 0, "unsupported policy {index} executed");
            }
        }
        engine.destroy().unwrap();
    }
}

#[test]
fn supervised_mode_refuses_checkpoint_capture_restore_and_join_at_construction() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let store = Arc::new(Checkpoints);
    assert!(
        Engine::with_checkpoint(
            GuestIsa::X86_64,
            selected_plan(&executable),
            StandardStreams::default(),
            store.clone(),
            store.clone(),
        )
        .is_err()
    );

    let mut restore = selected_plan(&executable);
    restore.options.set("HL_RESTORE", "1", true).unwrap();
    assert!(Engine::with_streams(GuestIsa::X86_64, restore, StandardStreams::default()).is_err());

    let coordinator = Engine::with_checkpoint(
        GuestIsa::X86_64,
        RuntimePlan {
            options: Options::default(),
            ..selected_plan(&executable)
        },
        StandardStreams::default(),
        store.clone(),
        store,
    )
    .unwrap();
    let channel = coordinator.checkpoint_channel().unwrap();
    assert!(
        Engine::with_checkpoint_channel(
            GuestIsa::X86_64,
            selected_plan(&executable),
            StandardStreams::default(),
            channel,
        )
        .is_err()
    );
    coordinator.destroy().unwrap();
}
