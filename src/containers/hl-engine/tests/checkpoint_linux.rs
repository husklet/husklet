#![cfg(target_os = "linux")]

use hl_engine::{
    activation::GuestIsa,
    composition::{CheckpointSink, CheckpointSource, CompositionError, StandardStreams},
    launcher::plan::RuntimePlan,
    options::Options,
    runtime::Engine,
};
use std::{
    collections::BTreeMap,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

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

#[derive(Default)]
struct Store(Mutex<BTreeMap<String, Vec<u8>>>);

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
    Instant::now() + Duration::from_secs(10)
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

fn checkpoint_round_trip(isa: GuestIsa, executable: &Path, recapture_barrier: Option<&std::sync::Barrier>) {
    let temporary = tempfile::tempdir().unwrap();
    let release = temporary.path().join("release");
    let final_release = temporary.path().join("final-release");
    let output = temporary.path().join("release.output");
    let store = Arc::new(Store::default());

    let capture = Engine::with_checkpoint(
        isa,
        plan(executable, &release, &final_release, &["HL_CHECKPOINT"]),
        StandardStreams::default(),
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
        StandardStreams::default(),
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
        StandardStreams::default(),
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
        StandardStreams::default(),
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
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let executable = fixture(isa, fixtures.path());
        assert!(
            executable.is_file(),
            "missing checkpoint fixture: {}",
            executable.display()
        );
        checkpoint_round_trip(isa, &executable, None);
    }
}

#[test]
fn checkpoint_arms_after_a_plain_engine_on_both_isas() {
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let plain = exit_fixture(isa, fixtures.path());
        let checkpoint = fixture(isa, fixtures.path());
        capture_after_plain_engine(isa, &plain, &checkpoint);
    }
}

#[test]
fn concurrent_engines_keep_second_generation_checkpoint_channels_private() {
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [
        (GuestIsa::Aarch64, signalfd_fixture(GuestIsa::Aarch64, fixtures.path())),
        (GuestIsa::X86_64, signalfd_fixture(GuestIsa::X86_64, fixtures.path())),
    ];
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
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
        let executable = signalfd_fixture(isa, fixtures.path());
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
        capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
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
