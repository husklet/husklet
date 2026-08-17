#![cfg(target_os = "linux")]

use hl_engine::{
    activation::GuestIsa,
    composition::{CheckpointSink, CheckpointSource, CompositionError, StandardStreams, Terminal, TerminalPort},
    launcher::plan::RuntimePlan,
    options::Options,
    runtime::Engine,
};
use std::{
    collections::{BTreeMap, VecDeque},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::{Duration, Instant},
};

fn checkpoint_test_gate() -> &'static RwLock<()> {
    static LOCK: OnceLock<RwLock<()>> = OnceLock::new();
    LOCK.get_or_init(|| RwLock::new(()))
}

fn fixture_compilation() -> RwLockReadGuard<'static, ()> {
    checkpoint_test_gate()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn exclusive_checkpoint_test() -> RwLockWriteGuard<'static, ()> {
    checkpoint_test_gate()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (variable, compiler, name) = match isa {
        GuestIsa::Aarch64 => (
            "HL_CHECKPOINT_TREE_AARCH64",
            "aarch64-linux-gnu-gcc",
            "checkpoint-tree-aarch64",
        ),
        GuestIsa::X86_64 => (
            "HL_CHECKPOINT_TREE_X86_64",
            "x86_64-linux-gnu-gcc",
            "checkpoint-tree-x86_64",
        ),
    };
    if let Some(path) = std::env::var_os(variable) {
        return PathBuf::from(path);
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/tree.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn signalfd_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (variable, compiler, name) = match isa {
        GuestIsa::Aarch64 => (
            "HL_CHECKPOINT_SIGNALFD_AARCH64",
            "aarch64-linux-gnu-gcc",
            "checkpoint-signalfd-aarch64",
        ),
        GuestIsa::X86_64 => (
            "HL_CHECKPOINT_SIGNALFD_X86_64",
            "x86_64-linux-gnu-gcc",
            "checkpoint-signalfd-x86_64",
        ),
    };
    if let Some(path) = std::env::var_os(variable) {
        return PathBuf::from(path);
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/signalfd.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-pthread", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn sleep_tree_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (variable, compiler, name) = match isa {
        GuestIsa::Aarch64 => (
            "HL_CHECKPOINT_SLEEP_TREE_AARCH64",
            "aarch64-linux-gnu-gcc",
            "checkpoint-sleep-tree-aarch64",
        ),
        GuestIsa::X86_64 => (
            "HL_CHECKPOINT_SLEEP_TREE_X86_64",
            "x86_64-linux-gnu-gcc",
            "checkpoint-sleep-tree-x86_64",
        ),
    };
    if let Some(path) = std::env::var_os(variable) {
        return PathBuf::from(path);
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/sleep_tree.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn exit_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-exit-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-exit-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/exit.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn continuation_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-continuation-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-continuation-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/continuation.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn foreground_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-foreground-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-foreground-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/foreground.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn timeout_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-timeout-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-timeout-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/timeout.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn secondary_tty_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-secondary-tty-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-secondary-tty-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/secondary_tty.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn timeout_plan(executable: &Path, mode: &str, ready: &Path, result: &Path, restore: bool) -> RuntimePlan {
    let mut options = Options::default();
    options.set(if restore { "HL_RESTORE" } else { "HL_CHECKPOINT" }, "1", true).unwrap();
    RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: [
            executable.as_os_str().as_encoded_bytes().to_vec(),
            mode.as_bytes().to_vec(),
            ready.as_os_str().as_encoded_bytes().to_vec(),
            result.as_os_str().as_encoded_bytes().to_vec(),
        ]
        .into(),
        environment: Vec::new(),
        result_path: None,
        options,
    }
}

fn continuation_plan(executable: &Path, ready: &Path, release: &Path, result: &Path, restore: bool) -> RuntimePlan {
    let mut options = Options::default();
    options.set(if restore { "HL_RESTORE" } else { "HL_CHECKPOINT" }, "1", true).unwrap();
    RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: [executable, ready, release, result]
            .into_iter()
            .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
            .collect(),
        environment: Vec::new(),
        result_path: None,
        options,
    }
}

fn pipeline_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-pipeline-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-pipeline-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/pipeline.c");
    let output = directory.join(name);
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

#[derive(Default)]
struct TerminalState {
    input: VecDeque<u8>,
    output: Vec<u8>,
    closed: bool,
}

#[derive(Default)]
struct TestTerminal {
    state: Mutex<TerminalState>,
    changed: Condvar,
}

impl TestTerminal {
    fn input(&self, bytes: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state.input.extend(bytes);
        self.changed.notify_all();
    }

    fn wait_output(&self, marker: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut state = self.state.lock().unwrap();
        while !state.output.windows(marker.len()).any(|window| window == marker) {
            assert!(
                !state.closed,
                "terminal closed before {:?}:\n{}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(&state.output)
            );
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "terminal did not produce {:?}:\n{}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(&state.output)
            );
            let (next, timeout) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
            assert!(
                !timeout.timed_out(),
                "terminal did not produce {:?}:\n{}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(&state.output)
            );
        }
    }

    fn output(&self) -> String {
        String::from_utf8_lossy(&self.state.lock().unwrap().output).into_owned()
    }
}

impl TerminalPort for TestTerminal {
    fn read(&self, output: &mut [u8]) -> std::io::Result<usize> {
        let mut state = self.state.lock().unwrap();
        while state.input.is_empty() && !state.closed {
            state = self.changed.wait(state).unwrap();
        }
        if state.closed {
            return Ok(0);
        }
        let count = output.len().min(state.input.len());
        for byte in &mut output[..count] {
            *byte = state.input.pop_front().unwrap();
        }
        Ok(count)
    }

    fn write(&self, input: &[u8]) -> std::io::Result<usize> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        state.output.extend(input);
        self.changed.notify_all();
        Ok(input.len())
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        self.changed.notify_all();
    }
}

#[derive(Default)]
struct Store(Mutex<BTreeMap<String, Vec<u8>>>);

#[derive(Default)]
struct TestTerminalPort {
    closed: Mutex<bool>,
    changed: Condvar,
}

impl TerminalPort for TestTerminalPort {
    fn read(&self, _: &mut [u8]) -> std::io::Result<usize> {
        let mut closed = self.closed.lock().unwrap();
        while !*closed {
            closed = self.changed.wait(closed).unwrap();
        }
        Ok(0)
    }

    fn write(&self, input: &[u8]) -> std::io::Result<usize> {
        Ok(input.len())
    }

    fn close(&self) {
        *self.closed.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

fn streams(terminal: bool) -> StandardStreams {
    if terminal {
        StandardStreams::default().with_terminal(Terminal::new(Arc::new(TestTerminalPort::default()), 24, 80).unwrap())
    } else {
        StandardStreams::default()
    }
}

impl CheckpointSink for Store {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn begin_until(&self, _: Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(NonZeroU64::MIN)
    }

    fn put_until(&self, _: NonZeroU64, name: &str, bytes: &[u8], deadline: Instant) -> Result<(), CompositionError> {
        (Instant::now() < deadline)
            .then_some(())
            .ok_or(CompositionError::DeadlineExceeded)?;
        self.0.lock().unwrap().insert(name.into(), bytes.into());
        Ok(())
    }

    fn abort_until(&self, _: NonZeroU64, _: Instant) -> Result<(), CompositionError> {
        Ok(())
    }

    fn commit_until(&self, _: NonZeroU64, manifest: &[u8], deadline: Instant) -> Result<(), CompositionError> {
        (Instant::now() < deadline)
            .then_some(())
            .ok_or(CompositionError::DeadlineExceeded)?;
        self.0.lock().unwrap().insert("MANIFEST".into(), manifest.into());
        Ok(())
    }
}

impl CheckpointSource for Store {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CompositionError> {
        self.0
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or(CompositionError::RuntimeConstruction)
    }

    fn list(&self) -> Result<Vec<String>, CompositionError> {
        Ok(self.0.lock().unwrap().keys().cloned().collect())
    }

    fn get_until(&self, name: &str, deadline: Instant) -> Result<Vec<u8>, CompositionError> {
        (Instant::now() < deadline)
            .then_some(())
            .ok_or(CompositionError::DeadlineExceeded)?;
        self.get(name)
    }

    fn list_until(&self, deadline: Instant) -> Result<Vec<String>, CompositionError> {
        (Instant::now() < deadline)
            .then_some(())
            .ok_or(CompositionError::DeadlineExceeded)?;
        self.list()
    }
}

fn manifest_foreground_group(store: &Store) -> i32 {
    let image = store.0.lock().unwrap();
    let manifest = image.get("MANIFEST").expect("checkpoint manifest");
    i32::from_ne_bytes(manifest[60..64].try_into().expect("foreground process-group field"))
}

fn assert_transient_signalfd_slots_absent(store: &Store) {
    const SIGNALS: usize = 65;
    const DEPTH: usize = 128;
    const ENTRY_SIZE: usize = 48;
    const SLOT_OFFSET: usize = 40;
    const COUNTS_OFFSET: usize = 24 + 4 * SIGNALS * 4;
    const QUEUE_OFFSET: usize = 2368;
    let image = store.0.lock().unwrap();
    let (_, bytes) = image
        .iter()
        .find(|(name, _)| name.ends_with("/signals"))
        .expect("checkpoint did not publish a signal-state object");
    assert!(bytes.len() >= QUEUE_OFFSET + SIGNALS * DEPTH * ENTRY_SIZE);
    for signal in 1..SIGNALS {
        let count_offset = COUNTS_OFFSET + signal * 4;
        let count = u32::from_ne_bytes(bytes[count_offset..count_offset + 4].try_into().unwrap()) as usize;
        assert!(count <= DEPTH, "invalid signal {signal} queue count {count}");
        for index in 0..count {
            let offset = QUEUE_OFFSET + (signal * DEPTH + index) * ENTRY_SIZE + SLOT_OFFSET;
            let slots = u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap());
            assert_eq!(
                slots, 0,
                "signal {signal} queue entry {index} serialized transient slots"
            );
        }
    }
}

fn plan(executable: &Path, release: &Path, final_release: &Path, options_to_set: &[&str]) -> RuntimePlan {
    let mut options = Options::default();
    for option in options_to_set {
        options.set(option, "1", true).unwrap();
    }
    RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: [
            executable.as_os_str().as_encoded_bytes().to_vec(),
            release.as_os_str().as_encoded_bytes().to_vec(),
            final_release.as_os_str().as_encoded_bytes().to_vec(),
        ]
        .into(),
        environment: Vec::new(),
        result_path: None,
        options,
    }
}

fn wait_ready(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let output = std::fs::read_to_string(path).unwrap_or_default();
        if ["READY 1", "READY 2", "READY 3"]
            .iter()
            .all(|marker| output.contains(marker))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("guest process tree did not become ready");
}

fn wait_for(path: &Path, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::fs::read_to_string(path).unwrap_or_default().contains(marker) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("guest did not publish {marker}");
}

fn wait_cycle_ready(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let output = std::fs::read_to_string(path).unwrap_or_default();
        if ["CYCLE-READY 1", "CYCLE-READY 2", "CYCLE-READY 3"]
            .iter()
            .all(|marker| output.contains(marker))
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn checkpoint_deadline() -> Instant {
    Instant::now() + hl_engine::composition::DEFAULT_CHECKPOINT_TIMEOUT
}

fn capture_after_plain_engine(isa: GuestIsa, plain_executable: &Path, checkpoint_executable: &Path) {
    let temporary = tempfile::tempdir().unwrap();
    let release = temporary.path().join("release");
    let final_release = temporary.path().join("final-release");
    let output = temporary.path().join("release.output");

    // Exercise the process-global native initialization first without checkpoint
    // channels. A later engine must still arm its own broker and trigger.
    let plain = Engine::from_plan(isa, plan(plain_executable, &release, &final_release, &[])).unwrap();
    plain.start().unwrap();
    assert_eq!(plain.wait().unwrap().guest_status, 0);

    std::fs::write(&output, []).unwrap();
    let store = Arc::new(Store::default());
    let capture = Engine::with_checkpoint(
        isa,
        plan(checkpoint_executable, &release, &final_release, &["HL_CHECKPOINT"]),
        StandardStreams::default(),
        store.clone(),
        store.clone(),
    )
    .unwrap();
    capture.start().unwrap();
    wait_ready(&output);
    capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(capture.wait().unwrap().guest_status, 0);
    assert!(store.0.lock().unwrap().contains_key("MANIFEST"));
}

fn checkpoint_round_trip(
    isa: GuestIsa,
    executable: &Path,
    recapture_barrier: Option<&std::sync::Barrier>,
    terminal: bool,
) {
    let temporary = tempfile::tempdir().unwrap();
    let release = temporary.path().join("release");
    let final_release = temporary.path().join("final-release");
    let output = temporary.path().join("release.output");
    let store = Arc::new(Store::default());

    let capture = Engine::with_checkpoint(
        isa,
        plan(executable, &release, &final_release, &["HL_CHECKPOINT"]),
        streams(terminal),
        store.clone(),
        store.clone(),
    )
    .unwrap();
    capture.start().unwrap();
    wait_ready(&output);
    capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(capture.wait().unwrap().guest_status, 0);
    {
        let image = store.0.lock().unwrap();
        assert!(image.contains_key("MANIFEST"));
        assert_eq!(
            image
                .keys()
                .filter(|name| name.starts_with("proc.") && name.ends_with("/meta"))
                .count(),
            3
        );
    }

    std::fs::write(&release, []).unwrap();
    let failed_restore = Engine::with_checkpoint(
        isa,
        plan(
            executable,
            &release,
            &final_release,
            &["HL_RESTORE", "HL_CKPT_TEST_FAIL_AFTER_FORK"],
        ),
        streams(terminal),
        store.clone(),
        store.clone(),
    )
    .unwrap();
    failed_restore.start().unwrap();
    assert!(matches!(
        failed_restore.wait(),
        Err(hl_engine::engine::EngineError::NativeCreateFailed(_))
    ));
    std::thread::sleep(Duration::from_millis(100));
    let failed_output = std::fs::read_to_string(&output).unwrap();
    assert!(
        !failed_output.contains("CYCLE-READY"),
        "a descendant ran after restore rollback:\n{failed_output}"
    );

    let second_store = Arc::new(Store::default());
    let recapture = Engine::with_checkpoint(
        isa,
        plan(executable, &release, &final_release, &["HL_RESTORE", "HL_CHECKPOINT"]),
        streams(terminal),
        second_store.clone(),
        store.clone(),
    )
    .unwrap();
    recapture.start().unwrap();
    assert!(
        wait_cycle_ready(&output),
        "restored guest process tree did not reach the second checkpoint; status={:?}:\n{}",
        recapture.wait(),
        std::fs::read_to_string(&output).unwrap_or_default()
    );
    if let Some(barrier) = recapture_barrier {
        barrier.wait();
    }
    recapture
        .capture_checkpoint_until(checkpoint_deadline())
        .unwrap_or_else(|error| {
            panic!(
                "second checkpoint failed: {error:?}\n{}",
                std::fs::read_to_string(&output).unwrap_or_default()
            )
        });
    assert_eq!(recapture.wait().unwrap().guest_status, 0);

    std::fs::write(&final_release, []).unwrap();
    let restore = Engine::with_checkpoint(
        isa,
        plan(executable, &release, &final_release, &["HL_RESTORE"]),
        streams(terminal),
        second_store.clone(),
        second_store,
    )
    .unwrap();
    restore.start().unwrap();
    assert_eq!(
        restore.wait().unwrap().guest_status,
        0,
        "{}",
        std::fs::read_to_string(&output).unwrap_or_default()
    );
    let output = std::fs::read_to_string(output).unwrap();
    assert!(output.contains("RESTORED 1"));
    assert!(output.contains("RESTORED 2"));
    assert!(output.contains("RESTORED 3"));
    assert!(output.contains("TREE-RESTORED"));
}

#[test]
fn retained_c_round_trips_three_process_tree_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        assert!(
            executable.is_file(),
            "missing checkpoint fixture: {}",
            executable.display()
        );
        checkpoint_round_trip(isa, &executable, None, false);
    }
}

#[test]
fn terminal_process_tree_survives_capture_restore_and_recapture_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        checkpoint_round_trip(isa, &executable, None, true);
    }
}

#[test]
fn terminal_waiting_for_sleep_survives_capture_restore_and_recapture_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, sleep_tree_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let output = temporary.path().join("release.output");
        let first = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_CHECKPOINT"]),
            streams(true),
            first.clone(),
            first.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        wait_for(&output, "READY");
        capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(capture.wait().unwrap().guest_status, 0);

        let failed = Arc::new(Store::default());
        let failed_restore = Engine::with_checkpoint(
            isa,
            plan(
                &executable,
                &release,
                &final_release,
                &["HL_RESTORE", "HL_CKPT_TEST_FAIL_TRIGGER_REATTACH"],
            ),
            streams(true),
            failed,
            first.clone(),
        )
        .unwrap();
        failed_restore.start().unwrap();
        assert!(matches!(
            failed_restore.wait(),
            Err(hl_engine::engine::EngineError::NativeCreateFailed(_))
        ));

        let second = Arc::new(Store::default());
        let recapture = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE", "HL_CHECKPOINT"]),
            streams(true),
            second.clone(),
            first,
        )
        .unwrap();
        recapture.start().unwrap();
        std::fs::write(&release, []).unwrap();
        wait_for(&output, "CHILD-RESTORED");
        recapture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(recapture.wait().unwrap().guest_status, 0);
        {
            let image = second.0.lock().unwrap();
            assert!(image.contains_key("MANIFEST"));
            assert_eq!(
                image
                    .keys()
                    .filter(|name| name.starts_with("proc.") && name.ends_with("/meta"))
                    .count(),
                2
            );
        }

        std::fs::write(&final_release, []).unwrap();
        let restore = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE"]),
            streams(true),
            second.clone(),
            second,
        )
        .unwrap();
        restore.start().unwrap();
        assert_eq!(restore.wait().unwrap().guest_status, 0);
        let output = std::fs::read_to_string(output).unwrap();
        assert!(output.contains("CHILD-FINAL"), "{output}");
        assert!(output.contains("PARENT-FINAL"), "{output}");
    }
}

#[test]
fn checkpoint_continuation_does_not_duplicate_read_or_wait_on_both_isas() {
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let executable = continuation_fixture(isa, fixtures.path());
        let temporary = tempfile::tempdir().unwrap();
        let ready = temporary.path().join("ready");
        let release = temporary.path().join("release");
        let result = temporary.path().join("result");
        let store = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            continuation_plan(&executable, &ready, &release, &result, false),
            StandardStreams::default(),
            store.clone(),
            store.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(ready.exists(), "{isa:?} fixture did not block in read");
        capture.capture_checkpoint_until(Instant::now() + Duration::from_secs(10)).unwrap();
        assert_eq!(capture.wait().unwrap().guest_status, 0);

        let restore = Engine::with_checkpoint(
            isa,
            continuation_plan(&executable, &ready, &release, &result, true),
            StandardStreams::default(),
            store.clone(),
            store,
        )
        .unwrap();
        restore.start().unwrap();
        std::fs::write(&release, []).unwrap();
        assert_eq!(restore.wait().unwrap().guest_status, 0);
        assert_eq!(
            std::fs::read_to_string(&result).unwrap(),
            "read=1 byte=X second=0 wait=1 exit=37 duplicate=-1 errno=10\n",
            "{isa:?} duplicated an interrupted read or wait"
        );
    }
}

#[test]
fn checkpoint_continuation_preserves_relative_timeout_on_both_isas() {
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let executable = timeout_fixture(isa, fixtures.path());
        for mode in [
            "nanosleep",
            "clock_nanosleep",
            "ppoll",
            "pselect",
            "epoll_pwait",
            "epoll_pwait2",
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let ready = temporary.path().join("ready");
            let result = temporary.path().join("result");
            let store = Arc::new(Store::default());
            let capture = Engine::with_checkpoint(
                isa,
                timeout_plan(&executable, mode, &ready, &result, false),
                StandardStreams::default(),
                store.clone(),
                store.clone(),
            )
            .unwrap();
            capture.start().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(ready.exists(), "{isa:?} {mode} fixture did not enter timeout");
            capture.capture_checkpoint_until(Instant::now() + Duration::from_secs(10)).unwrap();
            assert_eq!(capture.wait().unwrap().guest_status, 0);

            let restore = Engine::with_checkpoint(
                isa,
                timeout_plan(&executable, mode, &ready, &result, true),
                StandardStreams::default(),
                store.clone(),
                store,
            )
            .unwrap();
            let started = Instant::now();
            restore.start().unwrap();
            assert_eq!(restore.wait().unwrap().guest_status, 0, "{isa:?} {mode}");
            let elapsed = started.elapsed();
            assert!(
                elapsed >= Duration::from_millis(900) && elapsed < Duration::from_millis(1850),
                "{isa:?} {mode} restored for {elapsed:?}; expected saved remainder, not full timeout"
            );
            let output = std::fs::read_to_string(&result).unwrap();
            assert!(output.starts_with("result=0 errno=0"), "{isa:?} {mode}: {output}");
            if matches!(mode, "nanosleep" | "clock_nanosleep") {
                assert!(output.contains("rem=73.000000041"), "{isa:?} {mode} mutated remainder: {output}");
            }
            if mode.starts_with("epoll_") {
                assert!(
                    output.contains("event=deadbeef/123456789abcdef0"),
                    "{isa:?} {mode} mutated the event output on checkpoint: {output}"
                );
            }
        }
    }
}

#[test]
fn restored_foreground_sleep_takes_ctrl_c_without_killing_shell_on_both_isas() {
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let executable = foreground_fixture(isa, fixtures.path());
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let store = Arc::new(Store::default());

        let capture_port = Arc::new(TestTerminal::default());
        let capture_terminal = Terminal::new(capture_port.clone(), 24, 80).unwrap();
        let capture = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_CHECKPOINT"]),
            StandardStreams::default().with_terminal(capture_terminal),
            store.clone(),
            store.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        capture_port.wait_output(b"SLEEPING");
        capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(capture.wait().unwrap().guest_status, 0);
        assert!(
            manifest_foreground_group(&store) > 1,
            "checkpoint did not retain the foreground child group"
        );

        let restore_port = Arc::new(TestTerminal::default());
        let restore_terminal = Terminal::new(restore_port.clone(), 24, 80).unwrap();
        let restore = Arc::new(
            Engine::with_checkpoint(
                isa,
                plan(&executable, &release, &final_release, &["HL_RESTORE"]),
                StandardStreams::default().with_terminal(restore_terminal),
                store.clone(),
                store.clone(),
            )
            .unwrap(),
        );
        restore.start().unwrap();
        let (finished, completion) = std::sync::mpsc::channel();
        let waiting = restore.clone();
        std::thread::spawn(move || finished.send(waiting.wait()).unwrap());
        restore_port.wait_output(b"CHILD-ALIVE");
        match completion.try_recv() {
            Ok(result) => panic!(
                "{isa:?} restore ended before input: {result:?}\n{}",
                restore_port.output()
            ),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!("restore waiter disconnected"),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        restore_port.input(&[3]);
        let restored = completion
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{isa:?} restore did not exit after Ctrl-C:\n{}", restore_port.output()))
            .unwrap_or_else(|error| panic!("{isa:?} restore failed: {error:?}\n{}", restore_port.output()));
        assert_eq!(restored.guest_status, 0, "{}", restore_port.output());
        restore_port.wait_output(b"CHILD-SIGINT");
        restore_port.wait_output(b"MASK-RESTORED");
        restore_port.wait_output(b"PROMPT-SURVIVED");
    }
}

#[test]
fn secondary_pty_cannot_redirect_the_controlling_terminal_on_both_isas() {
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let executable = secondary_tty_fixture(isa, fixtures.path());
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let port = Arc::new(TestTerminal::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let store = Arc::new(Store::default());
        let engine = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &[]),
            StandardStreams::default().with_terminal(terminal),
            store.clone(),
            store,
        )
        .unwrap();
        engine.start().unwrap();
        assert_eq!(engine.wait().unwrap().guest_status, 0, "{isa:?}: {}", port.output());
        port.wait_output(b"SECONDARY-PTY-PRESERVED");
    }
}

#[test]
fn later_sibling_pipeline_leader_restores_before_members_resume_on_both_isas() {
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let executable = pipeline_fixture(isa, fixtures.path());
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let store = Arc::new(Store::default());

        let capture_port = Arc::new(TestTerminal::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_CHECKPOINT"]),
            StandardStreams::default().with_terminal(Terminal::new(capture_port.clone(), 24, 80).unwrap()),
            store.clone(),
            store.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        capture_port.wait_output(b"MEMBER-ALIVE");
        capture_port.wait_output(b"LEADER-ALIVE");
        capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(capture.wait().unwrap().guest_status, 0);

        let restore_port = Arc::new(TestTerminal::default());
        let restore = Arc::new(
            Engine::with_checkpoint(
                isa,
                plan(&executable, &release, &final_release, &["HL_RESTORE"]),
                StandardStreams::default().with_terminal(Terminal::new(restore_port.clone(), 24, 80).unwrap()),
                store.clone(),
                store.clone(),
            )
            .unwrap(),
        );
        restore.start().unwrap();
        let (finished, completion) = std::sync::mpsc::channel();
        let waiting = restore.clone();
        std::thread::spawn(move || finished.send(waiting.wait()).unwrap());
        restore_port.wait_output(b"MEMBER-ALIVE");
        restore_port.wait_output(b"LEADER-ALIVE");
        assert!(matches!(
            completion.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        restore_port.input(&[3]);
        let restored = completion
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("{isa:?} pipeline did not exit:\n{}", restore_port.output()))
            .unwrap_or_else(|error| panic!("{isa:?} pipeline restore failed: {error:?}\n{}", restore_port.output()));
        assert_eq!(restored.guest_status, 0, "{isa:?}: {}", restore_port.output());
        restore_port.wait_output(b"PIPELINE-PROMPT-SURVIVED");
    }
}

#[test]
fn terminal_claim_mask_failure_aborts_before_any_restored_process_resumes() {
    let fixtures = tempfile::tempdir().unwrap();
    let isa = GuestIsa::Aarch64;
    let executable = foreground_fixture(isa, fixtures.path());
    let temporary = tempfile::tempdir().unwrap();
    let release = temporary.path().join("release");
    let final_release = temporary.path().join("final-release");
    let store = Arc::new(Store::default());

    let capture_port = Arc::new(TestTerminal::default());
    let capture = Engine::with_checkpoint(
        isa,
        plan(&executable, &release, &final_release, &["HL_CHECKPOINT"]),
        StandardStreams::default().with_terminal(Terminal::new(capture_port.clone(), 24, 80).unwrap()),
        store.clone(),
        store.clone(),
    )
    .unwrap();
    capture.start().unwrap();
    capture_port.wait_output(b"SLEEPING");
    capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(capture.wait().unwrap().guest_status, 0);

    let restore_port = Arc::new(TestTerminal::default());
    let restore = Engine::with_checkpoint(
        isa,
        plan(
            &executable,
            &release,
            &final_release,
            &["HL_RESTORE", "HL_CKPT_TEST_FAIL_TTY_MASK"],
        ),
        StandardStreams::default().with_terminal(Terminal::new(restore_port.clone(), 24, 80).unwrap()),
        store.clone(),
        store,
    )
    .unwrap();
    restore.start().unwrap();
    assert!(matches!(
        restore.wait(),
        Err(hl_engine::engine::EngineError::NativeCreateFailed(_))
    ));
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !restore_port.output().contains("CHILD-ALIVE"),
        "a descendant resumed after atomic terminal-claim failure:\n{}",
        restore_port.output()
    );
}

#[test]
fn checkpoint_arms_after_a_plain_engine_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64]
        .map(|isa| (isa, exit_fixture(isa, fixtures.path()), fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, plain, checkpoint) in executables {
        capture_after_plain_engine(isa, &plain, &checkpoint);
    }
}

#[test]
fn concurrent_engines_keep_second_generation_checkpoint_channels_private() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [
        (GuestIsa::Aarch64, signalfd_fixture(GuestIsa::Aarch64, fixtures.path())),
        (GuestIsa::X86_64, signalfd_fixture(GuestIsa::X86_64, fixtures.path())),
    ];
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    let (first_ready, second_start) = std::sync::mpsc::channel();
    let (second_ready, first_capture) = std::sync::mpsc::channel();
    let (first_done, second_capture) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let [(first_isa, first_executable), (second_isa, second_executable)] = executables;
        scope.spawn(move || {
            concurrent_signalfd_recapture(
                first_isa,
                &first_executable,
                None,
                Some(first_ready),
                Some(first_capture),
                Some(first_done),
            );
        });
        scope.spawn(move || {
            concurrent_signalfd_recapture(
                second_isa,
                &second_executable,
                Some(second_start),
                Some(second_ready),
                Some(second_capture),
                None,
            );
        });
    });
}

fn concurrent_signalfd_recapture(
    isa: GuestIsa,
    executable: &Path,
    start_gate: Option<std::sync::mpsc::Receiver<()>>,
    ready_signal: Option<std::sync::mpsc::Sender<()>>,
    capture_gate: Option<std::sync::mpsc::Receiver<()>>,
    done_signal: Option<std::sync::mpsc::Sender<()>>,
) {
    let temporary = tempfile::tempdir().unwrap();
    let release = temporary.path().join("release");
    let final_release = temporary.path().join("final-release");
    let output = temporary.path().join("release.output");
    let first = Arc::new(Store::default());
    let capture = Engine::with_checkpoint(
        isa,
        plan(executable, &release, &final_release, &["HL_CHECKPOINT"]),
        StandardStreams::default(),
        first.clone(),
        first.clone(),
    )
    .unwrap();
    capture.start().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline
        && !std::fs::read_to_string(&output)
            .unwrap_or_default()
            .contains("READY targeted_wrong_read=1")
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(capture.wait().unwrap().guest_status, 0);

    if let Some(start_gate) = start_gate {
        start_gate.recv().unwrap();
    }
    std::fs::write(&release, []).unwrap();
    let second = Arc::new(Store::default());
    let recapture = Engine::with_checkpoint(
        isa,
        plan(executable, &release, &final_release, &["HL_RESTORE", "HL_CHECKPOINT"]),
        StandardStreams::default(),
        second.clone(),
        first,
    )
    .unwrap();
    recapture.start().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline
        && !std::fs::read_to_string(&output)
            .unwrap_or_default()
            .contains("CYCLE-READY")
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        std::fs::read_to_string(&output)
            .unwrap_or_default()
            .contains("CYCLE-READY")
    );
    if let Some(ready_signal) = ready_signal {
        ready_signal.send(()).unwrap();
    }
    if let Some(capture_gate) = capture_gate {
        capture_gate.recv().unwrap();
    }
    recapture
        .capture_checkpoint_until(checkpoint_deadline())
        .unwrap_or_else(|error| {
            panic!(
                "concurrent second checkpoint failed: {error:?}\n{}",
                std::fs::read_to_string(&output).unwrap_or_default()
            )
        });
    assert_eq!(recapture.wait().unwrap().guest_status, 0);
    if let Some(done_signal) = done_signal {
        done_signal.send(()).unwrap();
    }
}

#[test]
fn signalfd_readiness_and_signal64_defer_survive_two_generations_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, signalfd_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let output = temporary.path().join("release.output");
        let first = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_CHECKPOINT"]),
            StandardStreams::default(),
            first.clone(),
            first.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && !std::fs::read_to_string(&output)
                .unwrap_or_default()
                .contains("READY targeted_wrong_read=1 targeted_wrong_ready=1")
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let before_capture = std::fs::read_to_string(&output).unwrap_or_default();
        assert!(
            before_capture.contains("READY targeted_wrong_read=1 targeted_wrong_ready=1"),
            "guest did not reach decisive first checkpoint state: {before_capture}"
        );
        capture
            .capture_checkpoint_until(checkpoint_deadline())
            .unwrap_or_else(|error| {
                panic!(
                    "first signalfd checkpoint failed: {error:?}: {}",
                    std::fs::read_to_string(&output).unwrap_or_default()
                )
            });
        assert_transient_signalfd_slots_absent(&first);
        let capture_result = capture.wait();
        assert!(
            matches!(&capture_result, Ok(result) if result.guest_status == 0),
            "first capture failed: {capture_result:?}: {}",
            std::fs::read_to_string(&output).unwrap_or_default()
        );

        std::fs::write(&release, []).unwrap();
        let second = Arc::new(Store::default());
        let recapture = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE", "HL_CHECKPOINT"]),
            StandardStreams::default(),
            second.clone(),
            first,
        )
        .unwrap();
        recapture.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && !std::fs::read_to_string(&output)
                .unwrap_or_default()
                .contains("CYCLE-READY")
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let before_recapture = std::fs::read_to_string(&output).unwrap_or_default();
        assert!(
            before_recapture.contains("CYCLE-READY"),
            "guest did not enter parked signal handler: {before_recapture}"
        );
        recapture
            .capture_checkpoint_until(checkpoint_deadline())
            .unwrap_or_else(|error| {
                panic!(
                    "signalfd second checkpoint failed: {error:?}\n{}",
                    std::fs::read_to_string(&output).unwrap_or_default()
                )
            });
        assert_transient_signalfd_slots_absent(&second);
        let recapture_result = recapture.wait();
        assert!(
            matches!(&recapture_result, Ok(result) if result.guest_status == 0),
            "second capture failed: {recapture_result:?}: {}",
            std::fs::read_to_string(&output).unwrap_or_default()
        );

        std::fs::write(&final_release, []).unwrap();
        assert!(final_release.is_file(), "final release marker was not published");
        let restore = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE"]),
            StandardStreams::default(),
            second.clone(),
            second,
        )
        .unwrap();
        restore.start().unwrap();
        assert_eq!(
            restore.wait().unwrap().guest_status,
            0,
            "{}",
            std::fs::read_to_string(&output).unwrap_or_default()
        );
        let observed = std::fs::read_to_string(&output).unwrap();
        assert!(
            observed.contains("READY targeted_wrong_read=1 targeted_wrong_ready=1"),
            "{observed}"
        );
        assert!(observed.contains("TARGETED-RESTORED"), "{observed}");
        assert!(observed.contains("DEFER-RESTORED seen=1 nested=1"), "{observed}");
    }
}

#[test]
#[ignore = "50-round checkpoint activation stress"]
fn signalfd_first_capture_activation_stress_on_both_isas() {
    let fixtures = tempfile::tempdir().unwrap();
    for round in 0..50 {
        for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
            let executable = signalfd_fixture(isa, fixtures.path());
            let temporary = tempfile::tempdir().unwrap();
            let release = temporary.path().join("release");
            let final_release = temporary.path().join("final-release");
            let output = temporary.path().join("release.output");
            let store = Arc::new(Store::default());
            let capture = Engine::with_checkpoint(
                isa,
                plan(&executable, &release, &final_release, &["HL_CHECKPOINT"]),
                StandardStreams::default(),
                store.clone(),
                store,
            )
            .unwrap();
            capture.start().unwrap();
            let ready = Instant::now() + Duration::from_secs(10);
            while Instant::now() < ready
                && !std::fs::read_to_string(&output)
                    .unwrap_or_default()
                    .contains("READY targeted_wrong_read=1 targeted_wrong_ready=1")
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            let before = std::fs::read_to_string(&output).unwrap_or_default();
            assert!(
                before.contains("READY targeted_wrong_read=1 targeted_wrong_ready=1"),
                "round {round} {isa:?} did not reach READY: {before}"
            );
            capture
                .capture_checkpoint_until(checkpoint_deadline())
                .unwrap_or_else(|error| {
                    panic!(
                        "round {round} {isa:?} first checkpoint failed: {error:?}: {}",
                        std::fs::read_to_string(&output).unwrap_or_default()
                    )
                });
            let result = capture.wait();
            assert!(
                matches!(result, Ok(result) if result.guest_status == 0),
                "round {round} {isa:?} capture exit failed: {result:?}: {}",
                std::fs::read_to_string(&output).unwrap_or_default()
            );
        }
    }
}
