use super::Failure;
use hl_engine::activation::GuestIsa;
use hl_engine::composition::{CheckpointSink, CheckpointSource, CompositionError, StandardStreams};
use hl_engine::engine::{EngineExit, ExitKind, StopRequest};
use hl_engine::launcher::plan::RuntimePlan;
use hl_engine::runtime::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::new(), |mut text, byte| {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[derive(Default)]
struct Image {
    state: Mutex<ImageState>,
}

#[derive(Default)]
struct ImageState {
    committed: BTreeMap<String, Vec<u8>>,
    staged: BTreeMap<String, Vec<u8>>,
    owner: Option<NonZeroU64>,
    next: u64,
}

impl Image {
    fn bytes(&self) -> Result<Vec<u8>, Failure> {
        let state = self
            .state
            .lock()
            .map_err(|_| Failure::Request("checkpoint image lock poisoned".into()))?;
        if !state.committed.contains_key("MANIFEST") {
            return Err(Failure::Request("checkpoint capture published no MANIFEST".into()));
        }
        let mut bytes = Vec::new();
        for (name, value) in &state.committed {
            bytes.extend_from_slice(&(name.len() as u64).to_be_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
            bytes.extend_from_slice(value);
        }
        Ok(bytes)
    }

    fn validate(state: &ImageState, owner: NonZeroU64, deadline: Instant) -> Result<(), CompositionError> {
        if state.owner == Some(owner) && Instant::now() < deadline {
            Ok(())
        } else {
            Err(CompositionError::RuntimeConstruction)
        }
    }

    fn identity(&self) -> Result<(usize, String), Failure> {
        let state = self
            .state
            .lock()
            .map_err(|_| Failure::Request("checkpoint image lock poisoned".into()))?;
        if !state.committed.contains_key("MANIFEST") {
            return Err(Failure::Request("checkpoint capture published no MANIFEST".into()));
        }
        let members = state.committed.len();
        drop(state);
        Ok((members, sha256_hex(&self.bytes()?)))
    }
}

impl CheckpointSink for Image {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn begin_until(&self, deadline: Instant) -> Result<NonZeroU64, CompositionError> {
        let mut state = self.state.lock().map_err(|_| CompositionError::RuntimeConstruction)?;
        if state.owner.is_some() || Instant::now() >= deadline {
            return Err(CompositionError::TransactionBusy);
        }
        state.next = state.next.wrapping_add(1).max(1);
        let owner = NonZeroU64::new(state.next).ok_or(CompositionError::RuntimeConstruction)?;
        state.staged.clear();
        state.owner = Some(owner);
        Ok(owner)
    }

    fn put_until(
        &self,
        owner: NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), CompositionError> {
        let mut state = self.state.lock().map_err(|_| CompositionError::RuntimeConstruction)?;
        Self::validate(&state, owner, deadline)?;
        state.staged.insert(name.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn abort_until(&self, owner: NonZeroU64, deadline: Instant) -> Result<(), CompositionError> {
        let mut state = self.state.lock().map_err(|_| CompositionError::RuntimeConstruction)?;
        Self::validate(&state, owner, deadline)?;
        state.staged.clear();
        state.owner = None;
        Ok(())
    }

    fn commit_until(&self, owner: NonZeroU64, manifest: &[u8], deadline: Instant) -> Result<(), CompositionError> {
        let mut state = self.state.lock().map_err(|_| CompositionError::RuntimeConstruction)?;
        Self::validate(&state, owner, deadline)?;
        state.staged.insert("MANIFEST".into(), manifest.to_vec());
        state.committed = std::mem::take(&mut state.staged);
        state.owner = None;
        Ok(())
    }
}

impl CheckpointSource for Image {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CompositionError> {
        self.state
            .lock()
            .map_err(|_| CompositionError::RuntimeConstruction)?
            .committed
            .get(name)
            .cloned()
            .ok_or(CompositionError::RuntimeConstruction)
    }

    fn get_until(&self, name: &str, deadline: Instant) -> Result<Vec<u8>, CompositionError> {
        if Instant::now() >= deadline {
            return Err(CompositionError::DeadlineExceeded);
        }
        self.get(name)
    }

    fn list(&self) -> Result<Vec<String>, CompositionError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| CompositionError::RuntimeConstruction)?
            .committed
            .keys()
            .cloned()
            .collect())
    }

    fn list_until(&self, deadline: Instant) -> Result<Vec<String>, CompositionError> {
        if Instant::now() >= deadline {
            return Err(CompositionError::DeadlineExceeded);
        }
        self.list()
    }
}

fn host_control(rootfs: &Path, guest: &Path) -> Result<std::path::PathBuf, Failure> {
    if !guest.is_absolute()
        || guest
            .components()
            .skip(1)
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(Failure::Request(
            "--checkpoint-cycle must be an absolute normalized guest directory".into(),
        ));
    }
    let relative = guest
        .strip_prefix("/")
        .map_err(|_| Failure::Request("invalid checkpoint control path".into()))?;
    let canonical_root = std::fs::canonicalize(rootfs)
        .map_err(|error| Failure::Request(format!("cannot resolve rootfs {}: {error}", rootfs.display())))?;
    let host = std::fs::canonicalize(rootfs.join(relative)).map_err(|error| {
        Failure::Request(format!(
            "cannot resolve checkpoint control directory {} inside rootfs: {error}",
            guest.display()
        ))
    })?;
    if !host.is_dir() || !host.starts_with(&canonical_root) {
        return Err(Failure::Request(format!(
            "checkpoint control directory {} is not a directory contained by rootfs",
            guest.display()
        )));
    }
    Ok(host)
}

fn wait_contains(path: &Path, needle: &str, deadline: Instant) -> Result<(), Failure> {
    while Instant::now() < deadline {
        if std::fs::read_to_string(path).is_ok_and(|value| value.contains(needle)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(Failure::Request(format!("checkpoint probe did not publish {needle:?}")))
}

fn phase_plan(mut plan: RuntimePlan, restore: bool, capture: bool) -> Result<RuntimePlan, Failure> {
    for (enabled, name) in [(restore, "HL_RESTORE"), (capture, "HL_CHECKPOINT")] {
        if enabled {
            plan.options
                .set(name, "1", true)
                .map_err(|error| Failure::Request(format!("cannot set {name}: {error:?}")))?;
        }
    }
    Ok(plan)
}

fn construct(isa: GuestIsa, plan: RuntimePlan, image: Arc<Image>) -> Result<Arc<Engine>, Failure> {
    Ok(Arc::new(Engine::with_checkpoint(
        isa,
        plan,
        StandardStreams::default(),
        image.clone(),
        image,
    )?))
}

struct DeadlineBomb {
    cancel: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DeadlineBomb {
    fn arm(timeout: Duration) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            if matches!(
                receiver.recv_timeout(timeout),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                eprintln!("hl-engine: checkpoint cycle exceeded its hard process deadline");
                std::process::exit(124);
            }
        });
        Self {
            cancel: Some(sender),
            thread: Some(thread),
        }
    }
}

impl Drop for DeadlineBomb {
    fn drop(&mut self) {
        drop(self.cancel.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct StartedEngine {
    engine: Arc<Engine>,
    finished: bool,
}

impl StartedEngine {
    fn start(engine: Arc<Engine>) -> Result<Self, Failure> {
        engine.start()?;
        Ok(Self {
            engine,
            finished: false,
        })
    }

    fn finish(mut self) -> Result<EngineExit, Failure> {
        let exit = self.engine.wait()?;
        self.engine.destroy()?;
        self.finished = true;
        Ok(exit)
    }
}

impl Drop for StartedEngine {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.engine.stop(StopRequest::Force);
            let _ = self.engine.wait();
            let _ = self.engine.destroy();
        }
    }
}

pub(super) fn run(
    isa: GuestIsa,
    base: RuntimePlan,
    rootfs: &Path,
    guest_control: &Path,
    loader_receipt: bool,
) -> Result<(EngineExit, String), Failure> {
    // The lifecycle and its cleanup use synchronous native calls. This watchdog is deliberately
    // process-fatal: it is the only truthful upper bound if a native wait or destroy stops making
    // progress, and the worker is already the isolation/process boundary selected by the caller.
    let _deadline = DeadlineBomb::arm(TIMEOUT * 4);
    let control = host_control(rootfs, guest_control)?;
    if std::fs::read_dir(&control)
        .map_err(|error| Failure::Request(format!("cannot inspect checkpoint control directory: {error}")))?
        .next()
        .is_some()
    {
        return Err(Failure::Request(
            "checkpoint control directory must be empty before the cycle".into(),
        ));
    }
    let backend = if base.options.get("HL_TRANSLIT").is_some() {
        "translated"
    } else {
        "native"
    };
    let output = control.join("output");
    let state = control.join("state");
    let image = Arc::new(Image::default());

    let fresh = construct(isa, phase_plan(base.clone(), false, true)?, image.clone())?;
    if loader_receipt {
        super::emit_loader_receipt()?;
    }
    let fresh = StartedEngine::start(fresh)?;
    let deadline = Instant::now() + TIMEOUT;
    wait_contains(&output, "READY leader=", deadline)?;
    #[cfg(feature = "native-test-hooks")]
    if std::env::var_os("HL_CHECKPOINT_CYCLE_TEST_FAIL_AFTER_START").is_some() {
        return Err(Failure::Request("injected checkpoint-cycle failure after start".into()));
    }
    wait_contains(&state, "5", deadline)?;
    fresh.engine.capture_checkpoint_until(deadline)?;
    let captured_exit = fresh.finish()?;
    if captured_exit.guest_status != 0 {
        return Err(Failure::Request(format!(
            "captured probe exited {}",
            captured_exit.guest_status
        )));
    }
    let (members, digest) = image.identity()?;

    let killed = StartedEngine::start(construct(isa, phase_plan(base.clone(), true, false)?, image.clone())?)?;
    std::fs::write(control.join("cycle1"), [])
        .map_err(|error| Failure::Request(format!("cannot continue first restore: {error}")))?;
    wait_contains(&output, "CYCLE 1 progress=", Instant::now() + TIMEOUT)?;
    killed.engine.stop(StopRequest::Force)?;
    let killed_exit = killed.finish()?;
    if killed_exit.kind != ExitKind::Signal || killed_exit.guest_status != 9 {
        return Err(Failure::Request(format!(
            "forced restore stop returned {:?} {}, expected Signal 9",
            killed_exit.kind, killed_exit.guest_status
        )));
    }
    let _ = std::fs::remove_file(control.join("cycle1"));

    let restored = StartedEngine::start(construct(isa, phase_plan(base, true, false)?, image.clone())?)?;
    std::fs::write(control.join("cycle1"), [])
        .map_err(|error| Failure::Request(format!("cannot continue final restore: {error}")))?;
    wait_contains(&output, "CYCLE 1 progress=", Instant::now() + TIMEOUT)?;
    std::fs::write(control.join("cycle2"), [])
        .map_err(|error| Failure::Request(format!("cannot continue second cycle: {error}")))?;
    wait_contains(&output, "CYCLE 2 progress=", Instant::now() + TIMEOUT)?;
    std::fs::write(control.join("stop"), [])
        .map_err(|error| Failure::Request(format!("cannot stop final restore: {error}")))?;
    let exit = restored.finish()?;
    if exit.guest_status != 0 {
        return Err(Failure::Request(format!("restored probe exited {}", exit.guest_status)));
    }
    let (after_members, after_digest) = image.identity()?;
    if (members, &digest) != (after_members, &after_digest) {
        return Err(Failure::Request(
            "restore mutated the committed checkpoint image".into(),
        ));
    }
    let transcript = std::fs::read(&output)
        .map_err(|error| Failure::Request(format!("cannot read checkpoint probe transcript: {error}")))?;
    let text = String::from_utf8_lossy(&transcript);
    for (marker, expected) in [("READY leader=", 1), ("CYCLE 1 progress=", 2), ("CYCLE 2 progress=", 1)] {
        let actual = text.matches(marker).count();
        if actual != expected {
            return Err(Failure::Request(format!(
                "checkpoint probe marker {marker:?} appeared {actual} times, expected {expected}"
            )));
        }
    }
    let transcript_sha256 = sha256_hex(&transcript);
    let image_bytes = image.bytes()?;
    std::fs::write(control.join("checkpoint-image.bin"), &image_bytes)
        .map_err(|error| Failure::Request(format!("cannot export checkpoint image: {error}")))?;
    std::fs::write(control.join("checkpoint-transcript.bin"), &transcript)
        .map_err(|error| Failure::Request(format!("cannot export checkpoint transcript: {error}")))?;
    let receipt = serde_json::json!({
        "schema": "husklet-checkpoint-cycle-v1",
        "guest_isa": match isa { GuestIsa::Aarch64 => "aarch64", GuestIsa::X86_64 => "x86_64" },
        "backend": backend,
        "captured": true,
        "original_reaped": true,
        "restored_killed": true,
        "kill_exit_kind": format!("{:?}", killed_exit.kind),
        "kill_guest_status": killed_exit.guest_status,
        "restored_continued": true,
        "member_count": members,
        "image_sha256": digest,
        "transcript_sha256": transcript_sha256,
        "final_guest_status": exit.guest_status,
    })
    .to_string();
    Ok((exit, receipt))
}

#[cfg(test)]
mod tests {
    use super::{CheckpointSink, CheckpointSource, Image, host_control, sha256_hex};
    use std::num::NonZeroU64;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    #[test]
    fn sha256_hex_is_exact_lowercase_and_known() {
        for (input, expected) in [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        ] {
            let actual = sha256_hex(input);
            assert_eq!(actual, expected);
            assert_eq!(actual.len(), 64);
            assert!(
                actual
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
        }
    }

    #[test]
    fn control_path_is_absolute_normalized_and_rootfs_bound() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("run/cycle")).unwrap();
        assert_eq!(
            host_control(root.path(), std::path::Path::new("/run/cycle")).unwrap(),
            root.path().join("run/cycle")
        );
        for invalid in ["run/cycle", "/run/../cycle"] {
            assert!(
                host_control(root.path(), std::path::Path::new(invalid)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn checkpoint_image_is_atomic_and_hashes_the_committed_generation() {
        let image = Image::default();
        let deadline = Instant::now() + Duration::from_secs(1);
        let first = image.begin_until(deadline).unwrap();
        image.put_until(first, "member-1", b"one", deadline).unwrap();
        image.commit_until(first, b"manifest-one", deadline).unwrap();
        let identity = image.identity().unwrap();
        assert_eq!(identity.0, 2);

        let second = image.begin_until(deadline).unwrap();
        image.put_until(second, "member-2", b"two", deadline).unwrap();
        image.abort_until(second, deadline).unwrap();
        assert_eq!(image.identity().unwrap(), identity);
        assert_eq!(image.get("member-1").unwrap(), b"one");
        assert!(image.get("member-2").is_err());
        assert!(image.put_until(NonZeroU64::MIN, "late", b"bad", deadline).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn control_path_cannot_escape_through_a_symlink() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(host_control(root.path(), std::path::Path::new("/escape")).is_err());
    }
}
