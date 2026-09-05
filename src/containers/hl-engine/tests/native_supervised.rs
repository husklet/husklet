#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use hl_engine::{
    activation::GuestIsa,
    composition::{
        CheckpointSink, CheckpointSource, CompositionError, StandardStream, StandardStreamPort, StandardStreams,
        Terminal, TerminalPort,
    },
    engine::ExitKind,
    launcher::plan::{RuntimeBoxPolicy, RuntimePlan},
    options::Options,
    runtime::Engine,
};
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::num::NonZeroU64;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
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

#[derive(Default)]
struct PaneTerminal {
    closed: Mutex<bool>,
    bytes: Mutex<Vec<u8>>,
    changed: Condvar,
}

impl TerminalPort for PaneTerminal {
    fn read(&self, _: &mut [u8]) -> std::io::Result<usize> {
        let mut closed = self.closed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*closed {
            closed = self
                .changed
                .wait(closed)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        Ok(0)
    }

    fn write(&self, input: &[u8]) -> std::io::Result<usize> {
        self.bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(input);
        Ok(input.len())
    }

    fn close(&self) {
        *self.closed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.changed.notify_all();
    }
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
        rootfs: selected.then(|| {
            std::env::var_os("HL_NATIVE_TEST_ROOTFS")
                .map_or_else(|| b"/".to_vec(), |root| root.as_encoded_bytes().to_vec())
        }),
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
    let streams = if std::env::var_os("HL_NATIVE_TEST_NO_STREAMS").is_some() {
        StandardStreams::default()
    } else {
        StandardStreams::default().with_output(output.clone())
    };
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).unwrap();
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
fn supervised_node_can_execute_the_npm_cli_when_a_fixture_rootfs_is_supplied() {
    let Some(root) = std::env::var_os("HL_NATIVE_TEST_ROOTFS") else {
        return;
    };
    let executable = PathBuf::from(root).join("usr/local/bin/node");
    if !executable.is_file() {
        return;
    }
    let (status, output, error) = run(
        &executable,
        &["/usr/local/lib/node_modules/npm/bin/npm-cli.js", "--version"],
        true,
    );
    assert_eq!(status, 0, "stderr: {}", String::from_utf8_lossy(&error));
    if std::env::var_os("HL_NATIVE_TEST_NO_STREAMS").is_none() {
        assert!(!output.is_empty());
    }
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
        arguments: vec![
            executable.as_os_str().as_encoded_bytes().to_vec(),
            b"descendant".to_vec(),
        ],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: if translated_off {
            RuntimeBoxPolicy::default()
        } else {
            RuntimeBoxPolicy {
                hostname: Some(b"native-auto".to_vec()),
                ..isolated_policy()
            }
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
fn interpreted_runtime_accepts_an_executable_larger_than_sixty_four_mebibytes() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    std::fs::OpenOptions::new()
        .write(true)
        .open(&executable)
        .unwrap()
        .set_len(65 * 1024 * 1024)
        .unwrap();

    let (status, stdout, stderr) = run(&executable, &["output"], false);

    assert_eq!(status, 23);
    assert_eq!(stdout, b"native-supervised");
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
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
    assert_eq!(
        (&automatic.0, automatic.1.as_slice()),
        (&0, b"descendant-supervised".as_slice())
    );
    assert_ne!((&translated.0, &translated.1), (&automatic.0, &automatic.1));
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
    assert!(
        output.stdout.lock().unwrap().is_empty(),
        "translated retry executed the guest"
    );
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
    plan.options.set("HL_CHECKPOINT", "1", true).unwrap();
    plan.options
        .set_bytes(
            "HL_NATIVE_CKPT_TEST_IDLE_RECEIPT",
            receipt.as_os_str().as_encoded_bytes(),
            true,
        )
        .unwrap();
    plan.arguments.push(b"checkpoint-idle".to_vec());
    let store = Arc::new(Checkpoints::default());
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
fn supervised_overlay_projects_case_distinct_bookworm_names_before_ownership() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let lower = work.path().join("lower");
    let upper = work.path().join("upper");
    let overlay_work = work.path().join("work");
    let headers = lower.join("usr/include/linux");
    let netfilter = headers.join("netfilter");
    let encoded_directory = headers.join(".hl-name-directory");
    for directory in [
        &upper,
        &overlay_work,
        &lower.join("bin"),
        &lower.join("proc"),
        &netfilter,
        &encoded_directory,
    ] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let executable = lower.join("bin/fixture");
    std::fs::copy(&built, &executable).unwrap();
    std::fs::write(netfilter.join("xt_CONNMARK.h"), b"upper-CONNMARK\n").unwrap();
    std::fs::write(netfilter.join(".hl-name-bookworm"), b"lower-connmark\n").unwrap();
    std::fs::write(encoded_directory.join(".hl-name-child"), b"nested\n").unwrap();
    std::fs::write(encoded_directory.join("hard-a"), b"hardlink\n").unwrap();
    std::fs::hard_link(encoded_directory.join("hard-a"), encoded_directory.join("hard-b")).unwrap();
    std::os::unix::fs::symlink("netfilter/xt_CONNMARK.h", headers.join(".hl-name-link")).unwrap();
    assert!(
        std::process::Command::new("mknod")
            .args(["-m", "600"])
            .arg(encoded_directory.join("device"))
            .args(["c", "1", "3"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("setcap")
            .arg("cap_net_bind_service=ep")
            .arg(encoded_directory.join("hard-a"))
            .status()
            .unwrap()
            .success()
    );

    let mut options = Options::default();
    options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
    options
        .set_bytes("HL_OVERLAY_WORK", overlay_work.as_os_str().as_encoded_bytes(), true)
        .unwrap();
    options
        .set(
            "HL_FILE_NAMES",
            concat!(
                "usr/include/linux/.hl-name-directory\tusr/include/linux/CaseDir\n",
                "usr/include/linux/.hl-name-link\tusr/include/linux/case-link\n",
                "usr/include/linux/netfilter/.hl-name-bookworm\tusr/include/linux/netfilter/xt_connmark.h\n",
                "usr/include/linux/CaseDir/.hl-name-child\tusr/include/linux/CaseDir/value"
            ),
            true,
        )
        .unwrap();
    options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    let output = Arc::new(Output::default());
    let plan = RuntimePlan {
        rootfs: Some(upper.as_os_str().as_encoded_bytes().to_vec()),
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: vec![
            executable.as_os_str().as_encoded_bytes().to_vec(),
            b"overlay-names".to_vec(),
        ],
        environment: Vec::new(),
        result_path: None,
        options,
        box_policy: RuntimeBoxPolicy {
            lower_layers: Some(lower.as_os_str().as_encoded_bytes().to_vec()),
            file_owners: Some(b"usr/include/linux/netfilter/xt_connmark.h\t123\t456".to_vec()),
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
    let exit = engine.wait();
    engine.destroy().unwrap();
    assert!(
        exit.is_ok(),
        "{}",
        String::from_utf8_lossy(&output.stderr.lock().unwrap())
    );
    let exit = exit.unwrap();
    assert_eq!(exit.guest_status, 0);
    assert_eq!(*output.stdout.lock().unwrap(), b"overlay-names-projected");
    assert_eq!(std::fs::metadata(netfilter.join(".hl-name-bookworm")).unwrap().uid(), 0);
    assert_eq!(
        std::fs::read(upper.join("usr/include/linux/CaseDir/value")).unwrap(),
        b"nested\n"
    );
    let capabilities = std::process::Command::new("getcap")
        .arg(upper.join("usr/include/linux/CaseDir/hard-a"))
        .output()
        .unwrap();
    assert!(capabilities.status.success());
    assert!(String::from_utf8_lossy(&capabilities.stdout).contains("cap_net_bind_service=ep"));
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
    std::fs::write(
        root.join("etc/hosts"),
        b"192.0.2.10\thusklet-native\n127.0.0.1\toriginal-marker",
    )
    .unwrap();
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
    assert_eq!(
        std::fs::read(root.join("etc/hosts")).unwrap(),
        b"192.0.2.10\thusklet-native\n127.0.0.1\toriginal-marker"
    );
    assert_eq!(
        std::fs::metadata(root.join("etc/hosts")).unwrap().permissions().mode() & 0o7777,
        0o640
    );
}

#[test]
fn supervised_overlay_projector_confines_root_cwd_and_replaces_hostile_proc() {
    let work = TempDir::new().unwrap();
    let before = native_overlay_directories();
    let built = fixture(work.path());
    let lower = work.path().join("lower");
    let upper = work.path().join("upper");
    let overlay_work = work.path().join("work");
    for path in [
        lower.join("bin"),
        lower.join("tmp"),
        lower.join("proc"),
        lower.join("etc"),
        upper.clone(),
        overlay_work.clone(),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(
        lower.join("etc/hosts"),
        b"192.0.2.10\thusklet-native\n127.0.0.1\toriginal-marker",
    )
    .unwrap();
    std::fs::set_permissions(lower.join("etc/hosts"), std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::write(lower.join("proc/hostile"), b"host").unwrap();
    let executable = lower.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.rootfs = Some(upper.as_os_str().as_encoded_bytes().to_vec());
    plan.arguments.push(b"root-contract".to_vec());
    plan.options
        .set_bytes("HL_OVERLAY_WORK", overlay_work.as_os_str().as_encoded_bytes(), true)
        .unwrap();
    plan.box_policy.lower_layers = Some(lower.as_os_str().as_encoded_bytes().to_vec());
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
    assert_eq!(
        std::fs::read(lower.join("etc/hosts")).unwrap(),
        b"192.0.2.10\thusklet-native\n127.0.0.1\toriginal-marker"
    );
    assert_eq!(native_overlay_directories(), before);
}

#[test]
fn ephemeral_gui_shape_combines_overlay_pty_identity_volumes_and_selective_sentry() {
    let work = TempDir::new().unwrap();
    let before = native_overlay_directories();
    let built = fixture(work.path());
    let lower = work.path().join("lower");
    let upper = work.path().join("upper");
    let overlay_work = work.path().join("work");
    let source = work.path().join("source");
    let output_directory = work.path().join("output");
    for path in [
        lower.join("bin"),
        lower.join("dev"),
        lower.join("tmp"),
        lower.join("proc"),
        lower.join("src"),
        lower.join("out"),
        upper.clone(),
        overlay_work.clone(),
        source.clone(),
        output_directory.clone(),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(lower.join("dev/tty"), b"").unwrap();
    let executable = lower.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    let terminal = Arc::new(PaneTerminal::default());
    let mut plan = selected_plan(&executable);
    plan.options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
    plan.rootfs = Some(upper.as_os_str().as_encoded_bytes().to_vec());
    plan.arguments.push(b"secure-jail".to_vec());
    plan.arguments.push(b"pty-session".to_vec());
    plan.options
        .set_bytes("HL_OVERLAY_WORK", overlay_work.as_os_str().as_encoded_bytes(), true)
        .unwrap();
    plan.box_policy.lower_layers = Some(lower.as_os_str().as_encoded_bytes().to_vec());
    plan.box_policy.working_directory = Some(b"/tmp".to_vec());
    plan.box_policy.hostname = Some(b"husklet-native".to_vec());
    plan.box_policy.uid = 1234;
    plan.box_policy.gid = 2345;
    plan.box_policy.volumes =
        Some(format!("ro:/src:{},rw:/out:{}", source.display(), output_directory.display()).into_bytes());
    let streams = StandardStreams::default().with_terminal(Terminal::new(terminal.clone(), 37, 111).unwrap());
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).unwrap();
    engine.start().unwrap();
    assert_eq!(engine.wait().unwrap().guest_status, 0);
    engine.destroy().unwrap();
    let text = terminal
        .bytes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        text.windows(b"secure-jail".len())
            .any(|window| window == b"secure-jail"),
        "pty={text:?}"
    );
    assert!(
        text.windows(b"pty-session".len())
            .any(|window| window == b"pty-session"),
        "pty={text:?}"
    );
    assert_eq!(native_overlay_directories(), before);
}

#[test]
fn supervised_terminal_has_a_controlling_session_before_guest_exec() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let terminal = Arc::new(PaneTerminal::default());
    let mut plan = selected_plan(&executable);
    plan.arguments.push(b"pty-session".to_vec());
    let streams = StandardStreams::default().with_terminal(Terminal::new(terminal.clone(), 37, 111).unwrap());
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).unwrap();
    if let Err(error) = engine.start() {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let text = terminal
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        panic!("native terminal start failed: {error:?}, pty={text:?}");
    }
    let waited = engine.wait();
    if let Err(error) = &waited {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let text = terminal
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        panic!("native terminal wait failed: {error:?}, pty={text:?}");
    }
    assert_eq!(waited.unwrap().guest_status, 0);
    engine.destroy().unwrap();
    let text = terminal
        .bytes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        text.windows(b"pty-session".len())
            .any(|window| window == b"pty-session"),
        "pty={text:?}"
    );
}

#[test]
fn supervised_terminal_refuses_an_image_supplied_non_tty_character_device() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let root = work.path().join("root");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("dev")).unwrap();
    std::fs::create_dir_all(root.join("proc")).unwrap();
    let executable = root.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    assert!(
        std::process::Command::new("mknod")
            .arg(root.join("dev/tty"))
            .args(["c", "1", "3"])
            .status()
            .unwrap()
            .success()
    );
    let terminal = Arc::new(PaneTerminal::default());
    let mut plan = selected_plan(&executable);
    plan.rootfs = Some(root.as_os_str().as_encoded_bytes().to_vec());
    plan.arguments.push(b"pty-session".to_vec());
    let streams = StandardStreams::default().with_terminal(Terminal::new(terminal, 37, 111).unwrap());
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, streams).unwrap();
    engine.start().unwrap();
    assert!(engine.wait().is_err());
    engine.destroy().unwrap();
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
fn supervised_projector_mounts_pinned_regular_files_with_exact_access() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let root = work.path().join("root");
    let sources = work.path().join("sources");
    for path in [root.join("bin"), root.join("proc"), root.join("etc"), sources.clone()] {
        std::fs::create_dir_all(path).unwrap();
    }
    let hosts = sources.join("hosts");
    let hostname = sources.join("hostname");
    let resolver = sources.join("resolv.conf");
    std::fs::write(&hosts, b"identity-hosts\n").unwrap();
    std::fs::write(&hostname, b"old-host\n").unwrap();
    std::fs::write(&resolver, b"nameserver 192.0.2.1\n").unwrap();
    for name in ["hosts", "hostname", "resolv.conf"] {
        std::fs::write(root.join("etc").join(name), b"target\n").unwrap();
    }
    let executable = root.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.rootfs = Some(root.as_os_str().as_encoded_bytes().to_vec());
    plan.arguments.push(b"file-volumes".to_vec());
    plan.box_policy.volumes = Some(
        format!(
            "ro:/etc/hosts:{},rw:/etc/hostname:{},rw:/etc/resolv.conf:{}",
            hosts.display(),
            hostname.display(),
            resolver.display()
        )
        .into_bytes(),
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
    assert_eq!(*output.stdout.lock().unwrap(), b"file-volumes");
    assert_eq!(std::fs::read(&hosts).unwrap(), b"identity-hosts\n");
    assert_eq!(std::fs::read(&hostname).unwrap(), b"guest-host\n");
    assert_eq!(std::fs::read(&resolver).unwrap(), b"nameserver 127.0.0.1\n");
}

#[test]
fn supervised_projector_connects_an_explicitly_bound_unix_socket() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let root = work.path().join("root");
    for path in [root.join("bin"), root.join("proc"), root.join("run/husklet")] {
        std::fs::create_dir_all(path).unwrap();
    }
    let executable = root.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    std::fs::write(root.join("run/husklet/extension.sock"), b"").unwrap();
    let socket = work.path().join("extension.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.write_all(b"socket").unwrap();
        let mut reply = [0_u8; 5];
        peer.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"reply");
    });
    let output = Arc::new(Output::default());
    let mut plan = selected_plan(&executable);
    plan.rootfs = Some(root.as_os_str().as_encoded_bytes().to_vec());
    plan.arguments
        .extend([b"socket-volume".to_vec(), b"/run/husklet/extension.sock".to_vec()]);
    plan.box_policy.volumes = Some(format!("rw:/run/husklet/extension.sock:{}", socket.display()).into_bytes());
    let engine = Engine::with_streams(
        GuestIsa::X86_64,
        plan,
        StandardStreams::default().with_output(output.clone()),
    )
    .unwrap();
    engine.start().unwrap();
    assert_eq!(engine.wait().unwrap().guest_status, 0);
    engine.destroy().unwrap();
    server.join().unwrap();
    assert_eq!(*output.stdout.lock().unwrap(), b"socket-volume");
}

#[test]
fn supervised_projector_refuses_missing_symlinked_and_wrong_type_file_volumes() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    let root = work.path().join("root");
    let source = work.path().join("source");
    let source_link = work.path().join("source-link");
    for path in [root.join("etc"), root.join("tmp")] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(&source, b"source\n").unwrap();
    std::os::unix::fs::symlink(&source, &source_link).unwrap();
    std::fs::write(root.join("etc/regular"), b"target\n").unwrap();
    std::os::unix::fs::symlink(root.join("etc/regular"), root.join("etc/link")).unwrap();
    for specification in [
        format!("rw:/etc/regular:{}", work.path().join("missing").display()),
        format!("rw:/etc/regular:{}", source_link.display()),
        format!("rw:/etc/link:{}", source.display()),
        format!("rw:/tmp:{}", source.display()),
        format!("rw:/etc/regular:{}", work.path().display()),
    ] {
        let mut plan = selected_plan(&executable);
        plan.rootfs = Some(root.as_os_str().as_encoded_bytes().to_vec());
        plan.box_policy.volumes = Some(specification.into_bytes());
        let engine = Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()).unwrap();
        if engine.start().is_ok() {
            assert!(engine.wait().is_err());
        }
        engine.destroy().unwrap();
    }
    assert_eq!(std::fs::read(root.join("etc/regular")).unwrap(), b"target\n");
    assert_eq!(std::fs::read(&source).unwrap(), b"source\n");
}

#[test]
fn supervised_projector_refuses_target_swap_after_pinning_without_mounting_replacement() {
    let work = TempDir::new().unwrap();
    let built = fixture(work.path());
    let root = work.path().join("root");
    for path in [root.join("bin"), root.join("proc"), root.join("etc")] {
        std::fs::create_dir_all(path).unwrap();
    }
    let executable = root.join("bin/fixture");
    std::fs::copy(built, &executable).unwrap();
    let source = work.path().join("source");
    std::fs::write(&source, b"trusted-source\n").unwrap();
    std::fs::write(root.join("etc/target"), b"pinned-target\n").unwrap();
    std::fs::write(root.join("etc/target.swap"), b"attacker-target\n").unwrap();
    let mut plan = selected_plan(&executable);
    plan.rootfs = Some(root.as_os_str().as_encoded_bytes().to_vec());
    plan.box_policy.volumes = Some(format!("ro:/etc/target:{}", source.display()).into_bytes());
    plan.options
        .set("HL_NATIVE_SUPERVISED_REFUSE", "file-volume-target-swap", true)
        .unwrap();
    let engine = Engine::with_streams(GuestIsa::X86_64, plan, StandardStreams::default()).unwrap();
    if engine.start().is_ok() {
        assert!(engine.wait().is_err());
    }
    engine.destroy().unwrap();
    assert_eq!(std::fs::read(root.join("etc/target")).unwrap(), b"attacker-target\n");
    assert_eq!(
        std::fs::read(root.join("etc/target.pinned")).unwrap(),
        b"pinned-target\n"
    );
    assert_eq!(std::fs::read(&source).unwrap(), b"trusted-source\n");
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
                if engine.start().is_ok() {
                    assert!(engine.wait().is_err());
                }
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

#[derive(Default)]
struct Checkpoints(std::sync::atomic::AtomicUsize);

impl Checkpoints {
    fn touched(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl CheckpointSink for Checkpoints {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        self.touched();
        Ok(())
    }
    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        self.touched();
        Ok(NonZeroU64::MIN)
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        self.touched();
        Ok(())
    }
    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        self.touched();
        Ok(())
    }
    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        self.touched();
        Ok(())
    }
}
impl CheckpointSource for Checkpoints {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        self.touched();
        Ok(Vec::new())
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        self.touched();
        Ok(Vec::new())
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        self.touched();
        Ok(Vec::new())
    }
}

#[test]
fn supervised_checkpoint_lifecycle_refuses_before_launch_or_storage_access() {
    let work = TempDir::new().unwrap();
    let executable = fixture(work.path());
    for scenario in ["fresh-capture", "restore", "checkpoint-mode", "checkpoint-policy"] {
        let mut plan = selected_plan(&executable);
        match scenario {
            "fresh-capture" => {}
            "restore" => plan.options.set("HL_RESTORE", "1", true).unwrap(),
            "checkpoint-mode" => plan.box_policy.checkpoint_mode = 1,
            "checkpoint-policy" => plan.box_policy.checkpoint_policy = 1,
            _ => unreachable!(),
        }
        let store = Arc::new(Checkpoints::default());
        let result = Engine::with_checkpoint(
            GuestIsa::X86_64,
            plan,
            StandardStreams::default(),
            store.clone(),
            store.clone(),
        );
        assert!(
            matches!(
                result,
                Err(hl_engine::engine::EngineError::CompositionFailed(
                    hl_engine::composition::CompositionError::NativeSupervisedRefused(
                        hl_engine::runtime::NativeSupervisedRefusal::Checkpoint,
                    ),
                ),)
            ),
            "native supervised admitted unsupported {scenario} lifecycle"
        );
        assert_eq!(
            store.0.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "native supervised touched checkpoint storage while refusing {scenario}",
        );
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
    let store = Arc::new(Checkpoints::default());
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
