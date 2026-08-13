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
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/checkpoint_tree.c");
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

    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CompositionError> {
        self.0.lock().unwrap().insert(name.into(), bytes.into());
        Ok(())
    }

    fn commit(&self, manifest: &[u8]) -> Result<(), CompositionError> {
        self.put("MANIFEST", manifest)
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

fn wait_cycle_ready(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let output = std::fs::read_to_string(path).unwrap_or_default();
        if ["CYCLE-READY 1", "CYCLE-READY 2", "CYCLE-READY 3"]
            .iter()
            .all(|marker| output.contains(marker))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("restored guest process tree did not reach the second checkpoint");
}

fn checkpoint_round_trip(isa: GuestIsa, executable: &Path) {
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
    capture.capture_checkpoint().unwrap();
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
    let recapture = Engine::with_checkpoint(
        isa,
        plan(executable, &release, &final_release, &["HL_RESTORE", "HL_CHECKPOINT"]),
        StandardStreams::default(),
        store.clone(),
        store.clone(),
    )
    .unwrap();
    recapture.start().unwrap();
    wait_cycle_ready(&output);
    recapture.capture_checkpoint().unwrap_or_else(|error| {
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
        store.clone(),
        store,
    )
    .unwrap();
    restore.start().unwrap();
    assert_eq!(restore.wait().unwrap().guest_status, 0);
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
        checkpoint_round_trip(isa, &executable);
    }
}
