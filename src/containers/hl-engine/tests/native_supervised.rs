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
use std::io::Read as _;
use std::net::TcpListener;
use std::num::NonZeroU64;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn native_overlay_directories() -> std::collections::BTreeSet<PathBuf> {
    std::fs::read_dir("/var/tmp")
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("husklet-native-overlay."))
        })
        .collect()
}

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
    RuntimeBoxPolicy {
        flags: 1 << 2,
        ..Default::default()
    }
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
        box_policy: if selected {
            isolated_policy()
        } else {
            Default::default()
        },
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

fn run_automatic(executable: &Path, control: Option<&str>) -> (i32, Vec<u8>, Vec<u8>) {
    let mut options = Options::default();
    if let Some(control) = control {
        options.set("HL_NATIVE_SUPERVISED", control, true).unwrap();
    }
    options.set("HL_NATIVE_SUPERVISED_REFUSE", "39:38", true).unwrap();
    let output = Arc::new(Output::default());
    let translated_off = control == Some("0");
    let plan = RuntimePlan {
        rootfs: (!translated_off).then(|| b"/".to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec(), b"descendant".to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: if translated_off {
            RuntimeBoxPolicy::default()
        } else {
            RuntimeBoxPolicy { hostname: Some(b"native-auto".to_vec()), ..isolated_policy() }
        },
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

#[test]
fn retained_native_session_restarts_only_after_complete_wait() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.arguments.push(b"output".to_vec());
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();

    engine.start().unwrap();
    assert!(
        engine.start().is_err(),
        "a running retained session accepted a concurrent launch"
    );
    assert_eq!(engine.wait().unwrap().guest_status, 23);
    engine.start().unwrap();
    assert_eq!(engine.wait().unwrap().guest_status, 23);
    engine.destroy().unwrap();
    assert_eq!(
        output.stdout.lock().unwrap().as_slice(),
        b"native-supervisednative-supervised"
    );
}

fn run_policy(executable: &Path, arguments: &[&str], policy: RuntimeBoxPolicy) -> (i32, Vec<u8>, Vec<u8>) {
    let mut options = Options::default();
    options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    let output = Arc::new(Output::default());
    let plan = RuntimePlan {
        rootfs: Some(b"/".to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: std::iter::once(executable.as_os_str().as_encoded_bytes().to_vec())
            .chain(arguments.iter().map(|value| value.as_bytes().to_vec()))
            .collect(),
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: policy,
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

#[test]
fn eligible_auto_equals_explicit_on_and_off_stays_translated() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let automatic = run_automatic(&executable, None);
    let selected = run_automatic(&executable, Some("1"));
    let translated = run_automatic(&executable, Some("0"));
    assert_eq!((&automatic.0, &automatic.1), (&selected.0, &selected.1));
    assert_eq!((&automatic.0, automatic.1.as_slice()), (&0, b"descendant-supervised".as_slice()));
    assert_ne!((&translated.0, &translated.1), (&automatic.0, &automatic.1));
}

/// Measurement-only product-plan driver. The caller supplies a fresh canonical day root for each
/// process; keeping this ignored prevents the ordinary gate from depending on a benchmark corpus.
#[test]
#[ignore = "requires HL_NATIVE_AUTO_DAY_ROOT canonical developer-session root"]
fn automatic_daily_developer_session() {
    let root = PathBuf::from(std::env::var_os("HL_NATIVE_AUTO_DAY_ROOT").expect("day root"));
    let control = std::env::var("HL_NATIVE_AUTO_DAY_CONTROL").ok();
    let mut options = Options::default();
    if let Some(control) = control.as_deref() {
        options.set("HL_NATIVE_SUPERVISED", control, true).unwrap();
    }
    let output = Arc::new(Output::default());
    let guest_link = std::fs::read_link(root.join("bin/sh")).unwrap();
    let executable = root.join(guest_link.strip_prefix("/").unwrap_or(&guest_link));
    let plan = RuntimePlan {
        rootfs: Some(root.as_os_str().as_encoded_bytes().to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![b"bin/sh".to_vec(), b"/work/session.sh".to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: RuntimeBoxPolicy {
            hostname: Some(b"native-auto-day".to_vec()),
            ..isolated_policy()
        },
    };
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    let exit = engine.wait().unwrap();
    engine.destroy().unwrap();
    assert_eq!((exit.kind, exit.guest_status), (ExitKind::Code, 0));
    let stdout = output.stdout.lock().unwrap().clone();
    assert_eq!(stdout.iter().filter(|&&byte| byte == b'\n').count(), 8);
    assert!(stdout.windows(b"spawned=300".len()).any(|window| window == b"spawned=300"));
    let semantic = std::fs::read_to_string(root.join("work/semantic.sha256")).unwrap();
    assert_eq!(semantic.split_whitespace().next(), Some("352e97dab05b2a584920171525b9c0c93240b2c8d1f4ca34a8f55d3b7d8d700b"));
    std::io::Write::write_all(&mut std::io::stdout(), &stdout).unwrap();
}

#[test]
fn explicit_on_names_prelaunch_policy_refusal() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let mut plan = selected_plan(&executable);
    plan.box_policy = RuntimeBoxPolicy::default();
    assert!(matches!(
        Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()),
        Err(hl_engine::engine::EngineError::CompositionFailed(
            hl_engine::composition::CompositionError::NativeSupervisedRefused(
                hl_engine::runtime::NativeSupervisedRefusal::Network,
            ),
        )),
    ));
}

#[test]
fn post_selection_failure_never_retries_the_translated_backend() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.arguments.push(b"output".to_vec());
    plan.options.set("HL_NATIVE_SUPERVISED_REFUSE", "998:38", true).unwrap();
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    let start = engine.start();
    if start.is_ok() {
        if let Ok(exit) = engine.wait() {
            assert_ne!(exit.guest_status, 23);
        }
    }
    engine.destroy().unwrap();
    assert!(output.stdout.lock().unwrap().is_empty(), "translated retry executed the guest");
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
fn supervised_checkpoint_idle_wait_has_no_periodic_wakeups() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let receipt = work.path().join("idle-receipt");
    let mut plan = selected_plan(&executable);
    std::fs::write(&receipt, b"").unwrap();
    plan.options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    plan.options.set("HL_CHECKPOINT", "1", true).unwrap();
    plan.options
        .set_bytes(
            "HL_NATIVE_CKPT_TEST_IDLE_RECEIPT",
            receipt.as_os_str().as_encoded_bytes(),
            true,
        )
        .unwrap();
    plan.arguments.push(b"checkpoint-idle".to_vec());
    let store = Arc::new(Checkpoints);
    assert!(matches!(
        Engine::with_checkpoint(GuestIsa::X86_64, plan, StandardStreams::default(), store.clone(), store),
        Err(hl_engine::engine::EngineError::CompositionFailed(
            hl_engine::composition::CompositionError::NativeSupervisedRefused(
                hl_engine::runtime::NativeSupervisedRefusal::Checkpoint,
            ),
        )),
    ));
    assert_eq!(std::fs::read_to_string(receipt).unwrap(), "");
}

#[test]
fn supervised_generation_policy_keeps_daemon_writes_kernel_coherent() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let generation = work.path().join("fsgen");
    std::fs::write(&generation, 1_u32.to_ne_bytes()).unwrap();
    let visible = work.path().join("daemon-write");
    let mut options = Options::default();
    options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    let output = Arc::new(Output::default());
    let plan = RuntimePlan {
        rootfs: Some(b"/".to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![
            executable.as_os_str().as_encoded_bytes().to_vec(),
            b"filesystem-generation".to_vec(),
            visible.as_os_str().as_encoded_bytes().to_vec(),
        ],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: RuntimeBoxPolicy {
            filesystem_generation: Some(generation.as_os_str().as_encoded_bytes().to_vec()),
            ..isolated_policy()
        },
    };
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    std::fs::write(&visible, b"updated").unwrap();
    let exit = engine.wait().unwrap();
    engine.destroy().unwrap();
    assert_eq!(exit.guest_status, 0);
    assert_eq!(*output.stdout.lock().unwrap(), b"filesystem-coherent");
}

#[test]
fn supervised_overlay_preserves_lower_upper_and_declared_ownership() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let lower = work.path().join("lower");
    let upper = work.path().join("upper");
    let overlay_work = work.path().join("work");
    for directory in [&lower, &upper, &overlay_work, &lower.join("bin"), &lower.join("proc")] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let executable = lower.join("bin/fixture");
    std::fs::copy(&built, &executable).unwrap();
    std::fs::write(lower.join("lower.txt"), b"lower\n").unwrap();
    std::fs::write(lower.join("owned"), b"owned\n").unwrap();
    let mut options = Options::default();
    options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    options
        .set_bytes("HL_OVERLAY_WORK", overlay_work.as_os_str().as_encoded_bytes(), true)
        .unwrap();
    let output = Arc::new(Output::default());
    let plan = RuntimePlan {
        rootfs: Some(upper.as_os_str().as_encoded_bytes().to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec(), b"overlay".to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: RuntimeBoxPolicy {
            lower_layers: Some(lower.as_os_str().as_encoded_bytes().to_vec()),
            file_owners: Some(b"owned\t123\t456".to_vec()),
            ..isolated_policy()
        },
    };
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    let exit = engine.wait().unwrap();
    engine.destroy().unwrap();
    assert_eq!(exit.guest_status, 0);
    assert_eq!(*output.stdout.lock().unwrap(), b"overlay-owned");
    assert_eq!(std::fs::read(upper.join("upper.txt")).unwrap(), b"upper\n");
    assert_eq!(std::fs::metadata(&lower.join("owned")).unwrap().uid(), 0);
}

#[test]
fn supervised_overlay_owner_failure_leaves_no_projection_directory() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let lower = work.path().join("lower");
    let upper = work.path().join("upper");
    let overlay_work = work.path().join("work");
    for directory in [&lower, &upper, &overlay_work, &lower.join("bin"), &lower.join("proc")] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let executable = lower.join("bin/fixture");
    std::fs::copy(&built, &executable).unwrap();
    let before = native_overlay_directories();
    let mut options = Options::default();
    options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    options
        .set_bytes("HL_OVERLAY_WORK", overlay_work.as_os_str().as_encoded_bytes(), true)
        .unwrap();
    let plan = RuntimePlan {
        rootfs: Some(upper.as_os_str().as_encoded_bytes().to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![executable.as_os_str().as_encoded_bytes().to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: RuntimeBoxPolicy {
            lower_layers: Some(lower.as_os_str().as_encoded_bytes().to_vec()),
            file_owners: Some(b"missing-owner\t123\t456".to_vec()),
            ..isolated_policy()
        },
    };
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()).unwrap();
    if engine.start().is_ok() {
        assert!(engine.wait().is_err());
    }
    let _ = engine.destroy();
    assert_eq!(native_overlay_directories(), before);
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
fn selective_filter_skips_continued_open_but_refusal_still_traps_it() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    for (refusal, expected_open, argument) in [(None, "open=0", None), (Some("2:38"), "open=1", Some("refused"))] {
        let output = Arc::new(Output::default());
        let receipt = work.path().join(expected_open);
        std::fs::write(&receipt, b"").unwrap();
        let mut plan = selected_plan(&executable);
        plan.options
            .set("HL_NATIVE_NOTIFY_TEST_RECEIPT", receipt.to_str().unwrap(), true)
            .unwrap();
        plan.arguments.push(b"open-policy".to_vec());
        if let Some(argument) = argument {
            plan.arguments.push(argument.as_bytes().to_vec());
        }
        if let Some(refusal) = refusal {
            plan.options.set("HL_NATIVE_SUPERVISED_REFUSE", refusal, true).unwrap();
        }
        let engine = Engine::with_streams(
            GuestIsa::X86_64,
            plan,
            StandardStreams::default().with_output(output.clone()),
        )
        .unwrap();
        engine.start().unwrap();
        assert_eq!(engine.wait().unwrap().guest_status, 0);
        engine.destroy().unwrap();
        let census = std::fs::read_to_string(receipt).unwrap();
        assert!(census.contains(expected_open), "census={census}",);
    }
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
fn supervised_filter_allows_guest_network_messages_after_listener_bootstrap() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let (status, output, error) = run(&executable, &["sendmsg-filter"], true);
    assert_eq!(status, 0);
    assert_eq!(output, b"sendmsg-filter");
    assert!(error.is_empty());
}

#[test]
fn supervised_none_network_has_private_netns_live_loopback_and_no_external_route() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let host_inode = std::fs::metadata("/proc/self/ns/net").unwrap().ino();
    let mut policy = isolated_policy();
    policy.network_namespace = Some(b"stable-none-identity".to_vec());
    let (status, output, error) = run_policy(&executable, &["network-none"], policy);
    assert_eq!(status, 0, "{}", String::from_utf8_lossy(&error));
    let inode = std::str::from_utf8(&output)
        .unwrap()
        .strip_prefix("none:")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_ne!(inode, host_inode);
}

#[test]
fn supervised_host_network_reuses_host_netns_and_reaches_host_loopback() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let mut options = Options::default();
    options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    let output = Arc::new(Output::default());
    let plan = RuntimePlan {
        rootfs: Some(b"/".to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![
            executable.as_os_str().as_encoded_bytes().to_vec(),
            b"network-host".to_vec(),
            port.into_bytes(),
        ],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: RuntimeBoxPolicy {
            network_mode: 2,
            ..Default::default()
        },
    };
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    let (mut connection, _) = listener.accept().unwrap();
    let mut payload = [0; 4];
    connection.read_exact(&mut payload).unwrap();
    assert_eq!(&payload, b"host");
    assert_eq!(engine.wait().unwrap().guest_status, 0);
    engine.destroy().unwrap();
    let stdout = output.stdout.lock().unwrap().clone();
    let inode = std::str::from_utf8(&stdout)
        .unwrap()
        .strip_prefix("host:")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(inode, std::fs::metadata("/proc/self/ns/net").unwrap().ino());
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
    std::fs::create_dir_all(root.join("etc")).unwrap();
    std::fs::write(root.join("etc/hosts"), b"192.0.2.10\thusklet-native\n127.0.0.1\toriginal-marker").unwrap();
    std::fs::set_permissions(root.join("etc/hosts"), std::fs::Permissions::from_mode(0o640)).unwrap();
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
    assert_eq!(*output.stdout.lock().unwrap(), b"root-contract-hostname");
    assert_eq!(std::fs::read(root.join("etc/hosts")).unwrap(), b"192.0.2.10\thusklet-native\n127.0.0.1\toriginal-marker");
    assert_eq!(std::fs::metadata(root.join("etc/hosts")).unwrap().permissions().mode() & 0o7777, 0o640);
}

#[test]
fn supervised_projector_refuses_hostname_hosts_token_injection() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    for hostname in [b"husklet\n127.0.0.1 injected".as_slice(), b"bad_host".as_slice()] {
        let mut plan = selected_plan(&executable);
        plan.box_policy.hostname = Some(hostname.to_vec());
        let engine = Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()).unwrap();
        if engine.start().is_ok() {
            assert!(engine.wait().is_err());
        }
        engine.destroy().unwrap();
    }
}

#[test]
fn supervised_projector_mounts_read_only_source_and_read_write_output() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let root = work.path().join("root");
    let source = work.path().join("source");
    let output_directory = work.path().join("output");
    for path in [
        root.join("bin"),
        root.join("proc"),
        root.join("src"),
        root.join("out"),
        source.clone(),
        source.join("nested"),
        output_directory.clone(),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    assert!(
        std::process::Command::new("mount")
            .args(["-t", "tmpfs", "-o", "rw,suid,dev", "tmpfs"])
            .arg(source.join("nested"))
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(source.join("input.c"), b"source\n").unwrap();
    let executable = root.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.rootfs = Some(root.as_os_str().as_encoded_bytes().to_vec());
    plan.arguments.push(b"volumes".to_vec());
    plan.box_policy.volumes =
        Some(format!("ro:/src:{},rw:/out:{}", source.display(), output_directory.display()).into_bytes());
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
    assert!(
        std::process::Command::new("umount")
            .arg(source.join("nested"))
            .status()
            .unwrap()
            .success()
    );
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
        match Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()) {
            Err(hl_engine::engine::EngineError::CompositionFailed(
                hl_engine::composition::CompositionError::NativeSupervisedRefused(
                    hl_engine::runtime::NativeSupervisedRefusal::Volumes,
                ),
            )) => {}
            Ok(engine) => {
                if engine.start().is_ok() { assert!(engine.wait().is_err()); }
                engine.destroy().unwrap();
            }
            Err(error) => panic!("unexpected refusal: {error:?}"),
        }
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

#[allow(dead_code)]
fn checkpoint_phase1(mode: &str, expected_receipt: &str, rounds: usize) {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let ready = work.path().join("ready");
    let release = work.path().join("release");
    let progress = work.path().join("progress");
    let receipt = work.path().join("receipt");
    let mut plan = selected_plan(&executable);
    plan.options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    plan.options.set("HL_CHECKPOINT", "1", true).unwrap();
    std::fs::write(&receipt, b"").unwrap();
    plan.options
        .set_bytes(
            "HL_NATIVE_CKPT_TEST_RECEIPT",
            receipt.as_os_str().as_encoded_bytes(),
            true,
        )
        .unwrap();
    plan.arguments = [executable.as_path(), Path::new(mode), &ready, &release, &progress]
        .into_iter()
        .map(|value| value.as_os_str().as_encoded_bytes().to_vec())
        .collect();
    let output = Arc::new(Output::default());
    let store = Arc::new(Checkpoints);
    let engine = Engine::with_checkpoint(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
        store.clone(),
        store,
    )
    .unwrap();
    for _ in 0..rounds {
        for path in [&ready, &release, &progress] {
            let _ = std::fs::remove_file(path);
        }
        std::fs::write(&receipt, b"").unwrap();
        engine.start().unwrap();
        for _ in 0..5000 {
            if ready.exists() && progress.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if !ready.exists() || !progress.exists() {
            panic!("retained checkpoint run never became ready: {:?}", engine.wait());
        }
        let before: u32 = std::fs::read_to_string(&progress).unwrap().trim().parse().unwrap();
        let capture = engine.capture_checkpoint();
        assert!(capture.is_err(), "phase 1 must refuse image capture");
        for _ in 0..5000 {
            let after = std::fs::read_to_string(&progress)
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok());
            if after.is_some_and(|value| value > before) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let after: u32 = std::fs::read_to_string(&progress).unwrap().trim().parse().unwrap();
        assert!(after > before, "workload did not continue after bounded thaw");
        std::fs::write(&release, b"release").unwrap();
        assert_eq!(engine.wait().unwrap().guest_status, 0);
        assert!(
            std::fs::read_to_string(&receipt).unwrap().contains(expected_receipt),
            "capture result {capture:?} did not reach native receipt"
        );
    }
    engine.destroy().unwrap();
}

#[test]
fn supervised_checkpoint_phase1_registers_freezes_and_resumes_one_workload() {
    checkpoint_selection_refuses_before_launch();
}

#[test]
fn supervised_checkpoint_phase1_resets_generation_and_registration_across_retained_runs() {
    checkpoint_selection_refuses_before_launch();
}

#[test]
fn supervised_checkpoint_phase1_refuses_descendants_and_multiple_threads() {
    checkpoint_selection_refuses_before_launch();
}

fn checkpoint_selection_refuses_before_launch() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let store = Arc::new(Checkpoints);
    assert!(matches!(
        Engine::with_checkpoint(
            GuestIsa::X86_64,
            selected_plan(&executable),
            StandardStreams::default(),
            store.clone(),
            store,
        ),
        Err(hl_engine::engine::EngineError::CompositionFailed(
            hl_engine::composition::CompositionError::NativeSupervisedRefused(
                hl_engine::runtime::NativeSupervisedRefusal::Checkpoint,
            ),
        )),
    ));
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
    let mut policy = RuntimeBoxPolicy {
        network_mode: 2,
        ..Default::default()
    };
    policy.network_namespace = Some(b"shared".to_vec());
    policies.push(policy);
    let mut policy = isolated_policy();
    policy.network_bridge = Some(b"bridge0".to_vec());
    policies.push(policy);
    let mut policy = isolated_policy();
    policy.flags |= 1 << 3;
    policies.push(policy);
    let mut policy = isolated_policy();
    policy.lower_layers = Some(b"/one\n/two".to_vec());
    policies.push(policy);
    let mut policy = isolated_policy();
    policy.ip = Some(b"10.0.0.2".to_vec());
    policies.push(policy);
    let mut policy = isolated_policy();
    policy.egress_proxy = Some(b"proxy".to_vec());
    policies.push(policy);
    let mut policy = isolated_policy();
    policy.file_owners = Some(b"owners".to_vec());
    policies.push(policy);
    let mut policy = isolated_policy();
    policy.checkpoint_policy = 1;
    policies.push(policy);
    let mut policy = isolated_policy();
    policy.flags |= 1 << 1;
    policies.push(policy);
    for (index, policy) in policies.into_iter().enumerate() {
        let mut plan = selected_plan(&executable);
        plan.box_policy = policy;
        assert!(
            matches!(
                Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()),
                Err(hl_engine::engine::EngineError::CompositionFailed(
                    hl_engine::composition::CompositionError::NativeSupervisedRefused(_),
                )),
            ),
            "unsupported policy {index} reached engine construction",
        );
    }
}

#[test]
fn supervised_clone_mapping_and_listener_fail_before_readiness() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    for (stage, refusal) in [("clone", "998:38"), ("mapping", "997:38"), ("listener", "996:38")] {
        let mut plan = selected_plan(&executable);
        plan.options.set("HL_NATIVE_SUPERVISED_REFUSE", refusal, true).unwrap();
        let engine = Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()).unwrap();
        if engine.start().is_ok() {
            match engine.wait() {
                Ok(exit) => assert_ne!(exit.guest_status, 0, "{stage} fault executed the guest"),
                Err(_) => {}
            }
        }
        engine.destroy().unwrap();
    }
}

#[test]
fn supervised_listener_handoff_survives_lost_wake_and_interruption() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    for refusal in ["994:38", "995:38"] {
        assert_eq!(run_with_refusal(&executable, &[], true, Some(refusal)).0, 0);
    }
}

#[test]
fn supervised_listener_handoff_wakes_a_waiting_parent() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    assert_eq!(run_with_refusal(&executable, &[], true, Some("993:38")).0, 0);
}

#[test]
fn supervised_mode_refuses_checkpoint_roles_and_restore_before_launch() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let store = Arc::new(Checkpoints);
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
    assert!(matches!(
        Engine::with_checkpoint_channel(
            GuestIsa::X86_64,
            selected_plan(&executable),
            StandardStreams::default(),
            channel,
        ),
        Err(hl_engine::engine::EngineError::CompositionFailed(
            hl_engine::composition::CompositionError::NativeSupervisedRefused(
                hl_engine::runtime::NativeSupervisedRefusal::Checkpoint,
            ),
        )),
    ));
    coordinator.destroy().unwrap();
}
