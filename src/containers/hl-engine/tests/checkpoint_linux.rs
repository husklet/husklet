#![cfg(target_os = "linux")]

use hl_engine::{
    activation::GuestIsa,
    composition::{
        CheckpointSink, CheckpointSource, CompositionError, StandardStream, StandardStreamPort, StandardStreams,
        Terminal, TerminalPort,
    },
    engine::{EngineError, StopRequest},
    launcher::plan::RuntimePlan,
    options::Options,
    runtime::Engine,
};
use hl_process::unix_descriptor::{self as descriptor, Identity, Lock, StandardDescriptor};
use hl_process::{Capture as ProcessCapture, Command as ProcessCommand, EnvironmentEntry, Outcome as ProcessOutcome};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::NonZeroU64,
    os::fd::{AsRawFd, IntoRawFd},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard, atomic::AtomicBool},
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

fn blocked_tree_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-blocked-tree-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-blocked-tree-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/blocked_tree.c");
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

/// A peer that exited before it could ever join the capture does not stall it and does not fail it.
///
/// The peer is enumerated and interrupted like every other participant and then exits at its safepoint
/// without ever sending `REGISTER_READY`, which is what a transient fork child -- a shell's `sleep .05`,
/// a short `make` job -- does whenever it loses the race. The rendezvous used to wait out the whole ~5s
/// tree budget for it and then refuse the capture with `participant <pid> never committed proc.<pid>`.
/// That refusal was false: the broker refuses every byte-publishing operation from a connection that has
/// not registered, so a peer that never registered has nothing in the image to lose.
///
/// The process count is EXACT, so this also fails if the exemption ever dropped a peer that had committed
/// a group: the manifest must name the init and nobody else.
#[test]
fn a_peer_that_exited_before_joining_does_not_stall_or_fail_the_capture_on_both_isas() {
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
        let image = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(
                &executable,
                &release,
                &final_release,
                &["HL_CHECKPOINT", "HL_CKPT_TEST_PEER_EXIT_BEFORE_JOIN"],
            ),
            streams(true),
            image.clone(),
            image.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        wait_for(&output, "CHILD-READY");
        let started = Instant::now();
        capture
            .capture_checkpoint_until(checkpoint_deadline())
            .unwrap_or_else(|error| panic!("{isa:?} refused a capture over a peer that had already exited: {error:?}"));
        assert_eq!(capture.wait().unwrap().guest_status, 0);
        // The rendezvous budget is ~5s and the old failure burned all of it before refusing, so a fix that
        // merely made the refusal quieter still fails here.
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{isa:?} capture took {:?}, which is the rendezvous budget being burned on a departed peer",
            started.elapsed()
        );
        let stored = image.0.lock().unwrap();
        assert!(stored.contains_key("MANIFEST"), "{isa:?} published no manifest");
        assert_eq!(
            stored
                .keys()
                .filter(|name| name.starts_with("proc.") && name.ends_with("/meta"))
                .count(),
            1,
            "{isa:?} captured a process count that is not exactly the init"
        );
    }
}

/// The half that stops the exemption from being a loophole: a peer that PROVED membership and then died
/// before committing its group still refuses the whole capture.
///
/// It registered, so the broker had already admitted its publications; its state is genuinely lost, and
/// silently dropping it would publish a manifest that reports success while a member's state is unsaved.
/// Liveness alone cannot tell this peer from the one above -- both are gone -- which is precisely why the
/// discriminator is "gone AND never registered for this generation" rather than "gone".
#[test]
fn a_peer_that_died_after_joining_still_refuses_the_capture_on_both_isas() {
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
        let image = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(
                &executable,
                &release,
                &final_release,
                &["HL_CHECKPOINT", "HL_CKPT_TEST_PEER_EXIT_AFTER_JOIN"],
            ),
            streams(true),
            image.clone(),
            image.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        wait_for(&output, "CHILD-READY");
        let refusal = capture
            .capture_checkpoint_until(checkpoint_deadline())
            .expect_err("a capture missing a member that had already published was reported as successful");
        let _ = capture.wait();
        let stored = image.0.lock().unwrap();
        assert!(
            !stored.contains_key("MANIFEST"),
            "{isa:?} published a manifest for a capture missing a member: {refusal:?}"
        );
    }
}

/// A process that appears after the coordinator's enumeration is a member, not a surplus.
///
/// THE RACE, MADE DETERMINISTIC. `HL_CKPT_TEST_PEER_HIDDEN_FROM_ENUMERATION` withholds one live member
/// from the coordinator's FIRST scan and from that scan only, which is indistinguishable downstream from
/// a process forked one instruction after the scan returned -- the `sleep .05` in a shell loop, a `make`
/// job, any ordinary fork that straddles the freeze. Both production symptoms of this race come from that
/// single hole, and both are pinned here:
///
///   - the peer commits its group anyway (it observes the trigger generation at its own safepoint), and a
///     count taken against an enumeration that predates it reads one group too many:
///     `process-count mismatch: expected exactly 1 committed groups, captured 2` on a healthy close.
///   - or it never reaches a safepoint unkicked, and a manifest is published WITHOUT it while it is a
///     live guest process whose state is unsaved -- a capture reporting success with a member missing,
///     which is the failure this whole design treats as worse than a failed close.
///
/// So the assertions are deliberately both-sided: the capture must SUCCEED (no false refusal, and inside
/// the rendezvous budget rather than by burning it), and the manifest must contain EXACTLY the two
/// processes that exist. Either half alone can be satisfied by a broken implementation.
#[test]
fn a_peer_that_appeared_after_the_enumeration_is_captured_rather_than_miscounted_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, blocked_tree_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let output = temporary.path().join("release.output");
        let image = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(
                &executable,
                &release,
                &final_release,
                &["HL_CHECKPOINT", "HL_CKPT_TEST_PEER_HIDDEN_FROM_ENUMERATION"],
            ),
            streams(true),
            image.clone(),
            image.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        wait_for(&output, "CHILD-READY");
        let started = Instant::now();
        capture
            .capture_checkpoint_until(checkpoint_deadline())
            .unwrap_or_else(|error| {
                panic!("{isa:?} refused a capture over a member that appeared after the enumeration: {error:?}")
            });
        assert_eq!(capture.wait().unwrap().guest_status, 0);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{isa:?} capture took {:?}, which is the rendezvous budget being burned on a late-discovered member",
            started.elapsed()
        );
        let stored = image.0.lock().unwrap();
        assert!(stored.contains_key("MANIFEST"), "{isa:?} published no manifest");
        assert_eq!(
            stored
                .keys()
                .filter(|name| name.starts_with("proc.") && name.ends_with("/meta"))
                .count(),
            2,
            "{isa:?} published a manifest whose process count is not exactly the init and its child"
        );
    }
}

/// A member the coordinator never enumerated, whose state is genuinely lost, still refuses the capture.
///
/// This is the silent-data-loss shape, reproduced deterministically. `HL_CKPT_TEST_PEER_FORGOTTEN_AFTER_KICK`
/// kicks one member -- so it really does reach its safepoint and really does prove membership -- and then
/// drops it from the coordinator's enumeration for the rest of the capture, which is the reported hole
/// verbatim: a peer that registered, exited, and was then enumerated as 0 peers.
/// `HL_CKPT_TEST_PEER_EXIT_AFTER_JOIN` makes that member die after registering and before committing its
/// group, so it published under an admitted membership and its state IS unsaved. The rendezvous cannot
/// help here by construction: it only waits for peers it enumerated, and it enumerated none.
///
/// The only party that knows the process was a member is the broker, which is why the manifest's expected
/// process set is taken from the sealed `REGISTER_READY` ledger rather than from anything the coordinator
/// counted. Without that, this capture reports `checkpoint OK: 1 process(es)` with a registered member
/// missing -- a successful close over an image that has lost state, which this design treats as strictly
/// worse than a failed close.
#[test]
fn a_registered_member_the_enumeration_never_saw_still_refuses_the_capture_on_both_isas() {
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
        let image = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(
                &executable,
                &release,
                &final_release,
                &[
                    "HL_CHECKPOINT",
                    "HL_CKPT_TEST_PEER_FORGOTTEN_AFTER_KICK",
                    "HL_CKPT_TEST_PEER_EXIT_AFTER_JOIN",
                ],
            ),
            streams(true),
            image.clone(),
            image.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        wait_for(&output, "CHILD-READY");
        let refusal = capture
            .capture_checkpoint_until(checkpoint_deadline())
            .expect_err("a capture missing a member the enumeration never saw was reported as successful");
        let _ = capture.wait();
        let stored = image.0.lock().unwrap();
        assert!(
            !stored.contains_key("MANIFEST"),
            "{isa:?} published a manifest for a capture whose registered member never committed: {refusal:?}"
        );
    }
}

/// A member that is slow but is still working is waited for, however long the box makes it take.
///
/// THE RENDEZVOUS USED TO ASK THE WRONG QUESTION. It ran at most 500 passes of 10 ms and refused whatever
/// had not committed by then -- a fixed ~5 s wall budget for every enumerated guest process to reach a
/// safepoint. Under load that budget is not a property of the tree at all: the same workspace close that
/// passes in 7.5 s at load 8 refused at load ~16 with `participant ... never committed proc.6 (it did not
/// reach a checkpoint safepoint)`, because a descheduled member and a wedged member are the same thing to
/// a clock. `HL_CKPT_TEST_PEER_SLOW_SAFEPOINT` makes that deterministic: the member works, burning real
/// CPU, for eight seconds -- well past the old budget -- and then commits normally.
///
/// The assertions are deliberately both-sided. The capture must SUCCEED, which the old fixed budget cannot
/// do, and it must contain EXACTLY the init and its child, so a "fix" that waits longer and then publishes
/// a manifest without the slow member fails here rather than passing quietly. The elapsed lower bound
/// pins that the wait really happened over the old budget rather than the fixture having gone soft.
#[test]
fn a_slow_but_progressing_member_is_waited_for_rather_than_refused_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, blocked_tree_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let output = temporary.path().join("release.output");
        let image = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(
                &executable,
                &release,
                &final_release,
                &["HL_CHECKPOINT", "HL_CKPT_TEST_PEER_SLOW_SAFEPOINT"],
            ),
            streams(true),
            image.clone(),
            image.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        wait_for(&output, "CHILD-READY");
        let started = Instant::now();
        capture
            .capture_checkpoint_until(checkpoint_deadline())
            .unwrap_or_else(|error| {
                panic!("{isa:?} refused a capture over a member that was still working: {error:?}")
            });
        assert_eq!(capture.wait().unwrap().guest_status, 0);
        assert!(
            started.elapsed() >= Duration::from_millis(7500),
            "{isa:?} capture finished in {:?}, so the slow member never made the rendezvous wait past the \
             old fixed budget and this fixture proves nothing",
            started.elapsed()
        );
        let stored = image.0.lock().unwrap();
        assert!(stored.contains_key("MANIFEST"), "{isa:?} published no manifest");
        assert_eq!(
            stored
                .keys()
                .filter(|name| name.starts_with("proc.") && name.ends_with("/meta"))
                .count(),
            2,
            "{isa:?} waited for the slow member and then published a manifest without it"
        );
    }
}

/// A member that will never reach a safepoint is still refused, bounded, and named for what it did.
///
/// The other half of waiting on progress instead of on a clock: waiting forever for a member that is not
/// coming is a workspace close that hangs, which is not an improvement on one that fails.
/// `HL_CKPT_TEST_PEER_STALLS_AT_SAFEPOINT` is that member exactly -- alive, so it is never exempted as a
/// departed transient, deaf to any further kick, never registering, never committing, and consuming no
/// host CPU time from that point on. The stall window is the only thing that can end this capture, so this
/// test fails by HANGING if the rendezvous ever loses its give-up condition, and the upper bound below is
/// what converts that hang into a failure.
///
/// No manifest may be published: the member's state is unsaved, and this design treats a successful close
/// over an image that has lost state as strictly worse than a failed close.
#[test]
fn a_member_that_never_reaches_a_safepoint_still_refuses_the_capture_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, blocked_tree_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let output = temporary.path().join("release.output");
        let image = Arc::new(Store::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(
                &executable,
                &release,
                &final_release,
                &["HL_CHECKPOINT", "HL_CKPT_TEST_PEER_STALLS_AT_SAFEPOINT"],
            ),
            streams(true),
            image.clone(),
            image.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        wait_for(&output, "CHILD-READY");
        let started = Instant::now();
        let refusal = capture
            .capture_checkpoint_until(checkpoint_deadline())
            .expect_err("a capture whose member never reached a safepoint was reported as successful");
        let elapsed = started.elapsed();
        let cleanup = format!("stop={:?}", capture.stop(StopRequest::Force));
        let _ = capture.wait();
        assert!(
            elapsed < Duration::from_secs(25),
            "{isa:?} took {elapsed:?} to refuse a member that never moves, which is the embedder's deadline \
             expiring rather than the rendezvous deciding: {refusal:?} ({cleanup})"
        );
        let stored = image.0.lock().unwrap();
        assert!(
            !stored.contains_key("MANIFEST"),
            "{isa:?} published a manifest for a capture whose member never committed: {refusal:?}"
        );
    }
}

fn rejected_member_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-rejected-member-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-rejected-member-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/rejected_member.c");
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

fn daily_dev_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-daily-dev-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-daily-dev-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/daily_dev.c");
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

fn connected_unix_stream_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    shared_state_fixture(isa, directory, "connected_unix_stream")
}

fn socket_half_close_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    shared_state_fixture(isa, directory, "socket_half_close")
}

fn shared_state_fixture(isa: GuestIsa, directory: &Path, source_name: &str) -> PathBuf {
    let compiler = match isa {
        GuestIsa::Aarch64 => "aarch64-linux-gnu-gcc",
        GuestIsa::X86_64 => "x86_64-linux-gnu-gcc",
    };
    let output = directory.join(format!(
        "checkpoint-{}-{}",
        source_name,
        match isa {
            GuestIsa::Aarch64 => "aarch64",
            GuestIsa::X86_64 => "x86_64",
        }
    ));
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/checkpoint")
        .join(format!("{source_name}.c"));
    let status = std::process::Command::new(compiler)
        .args(["-static", "-O2", "-pthread", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    assert!(status.success(), "{compiler} failed with {status}");
    output
}

fn ambient_fd_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-ambient-fd-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-ambient-fd-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/ambient_fd.c");
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

fn ambient_fd_launcher(directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/ambient_fd_launcher.c");
    let output = directory.join("checkpoint-ambient-fd-launcher");
    let status = std::process::Command::new("cc")
        .args(["-O2", "-o"])
        .arg(&output)
        .arg(source)
        .status()
        .unwrap_or_else(|error| panic!("cannot compile ambient fd launcher: {error}"));
    assert!(status.success(), "ambient fd launcher compiler failed with {status}");
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
    options
        .set(if restore { "HL_RESTORE" } else { "HL_CHECKPOINT" }, "1", true)
        .unwrap();
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
    options
        .set(if restore { "HL_RESTORE" } else { "HL_CHECKPOINT" }, "1", true)
        .unwrap();
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

fn daily_dev_plan(executable: &Path, directory: &Path, restore: bool, capture: bool) -> RuntimePlan {
    let mut options = Options::default();
    let phase_probe = std::env::args_os().any(|argument| {
        matches!(
            argument.to_str(),
            Some("checkpoint_phase_ledger_probe_child" | "checkpoint_phase_ledger_clock_failure_probe_child")
        )
    });
    let exact_probe = std::env::args_os().any(|argument| argument == "checkpoint_phase_ledger_exact_probe_child");
    if phase_probe || exact_probe {
        options.set("HL_CHECKPOINT_PHASE_LEDGER", "1", true).unwrap();
    }
    if std::env::args_os().any(|argument| argument == "checkpoint_phase_ledger_clock_failure_probe_child") {
        options.set("HL_CHECKPOINT_PHASE_CLOCK_FAIL", "1", true).unwrap();
    }
    if restore {
        options.set("HL_RESTORE", "1", true).unwrap();
    }
    if capture {
        options.set("HL_CHECKPOINT", "1", true).unwrap();
    }
    RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: [executable, directory]
            .into_iter()
            .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
            .collect(),
        environment: Vec::new(),
        result_path: None,
        options,
    }
}

fn daily_dev_phase_plan(
    isa: GuestIsa,
    executable: &Path,
    directory: &Path,
    restore: bool,
    capture: bool,
) -> RuntimePlan {
    let mut plan = daily_dev_plan(executable, directory, restore, capture);
    if let Some(path) = std::env::var_os("HL_CHECKPOINT_PHASE_LEDGER_PATH") {
        let descriptor = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .into_raw_fd();
        plan.options
            .set("HL_DIAGNOSTIC_PORT", &descriptor.to_string(), true)
            .unwrap();
        plan.options
            .set(
                "HL_CHECKPOINT_PHASE_ISA",
                match isa {
                    GuestIsa::Aarch64 => "aarch64",
                    GuestIsa::X86_64 => "x86_64",
                },
                true,
            )
            .unwrap();
        if restore {
            plan.options.set("HL_CHECKPOINT_PHASE_GENERATION", "1", true).unwrap();
        }
    }
    plan
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

fn nested_pipeline_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-nested-pipeline-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-nested-pipeline-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/nested_pipeline.c");
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

fn inherited_pipe_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-inherited-pipe-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-inherited-pipe-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/inherited_pipe.c");
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

fn reaped_group_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-reaped-group-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-reaped-group-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/reaped_group.c");
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

fn dynamic_identity_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-dynamic-identity-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-dynamic-identity-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/dynamic_identity.c");
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

fn identity_churn_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-identity-churn-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-identity-churn-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/identity_churn.c");
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

#[derive(Default)]
struct CapturedOutput {
    bytes: Mutex<Vec<u8>>,
    changed: Condvar,
}

impl CapturedOutput {
    fn wait(&self, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut bytes = self.bytes.lock().unwrap();
        while !bytes.windows(marker.len()).any(|window| window == marker.as_bytes()) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "standard output did not contain {marker:?}:\n{}",
                String::from_utf8_lossy(&bytes)
            );
            let (next, timeout) = self.changed.wait_timeout(bytes, remaining).unwrap();
            bytes = next;
            assert!(
                !timeout.timed_out(),
                "standard output did not contain {marker:?}:\n{}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().unwrap()).into_owned()
    }
}

impl StandardStreamPort for CapturedOutput {
    fn write(&self, _: StandardStream, input: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(input);
        self.changed.notify_all();
        Ok(input.len())
    }

    fn close(&self) {
        self.changed.notify_all();
    }
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
struct AtomicStore(Mutex<AtomicStoreState>);

#[derive(Default)]
struct AtomicStoreState {
    committed: BTreeMap<String, Vec<u8>>,
    staging: BTreeMap<String, Vec<u8>>,
    owner: Option<NonZeroU64>,
    next: u64,
}

impl AtomicStore {
    fn from_committed(committed: BTreeMap<String, Vec<u8>>) -> Self {
        Self(Mutex::new(AtomicStoreState {
            committed,
            ..AtomicStoreState::default()
        }))
    }

    fn snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        self.0.lock().unwrap().committed.clone()
    }

    fn validate(state: &AtomicStoreState, owner: NonZeroU64, deadline: Instant) -> Result<(), CompositionError> {
        if state.owner == Some(owner) && Instant::now() < deadline {
            Ok(())
        } else {
            Err(CompositionError::RuntimeConstruction)
        }
    }
}

fn write_checkpoint_snapshot(path: &Path, image: &BTreeMap<String, Vec<u8>>) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(image.len() as u64).to_ne_bytes());
    for (name, value) in image {
        bytes.extend_from_slice(&(name.len() as u64).to_ne_bytes());
        bytes.extend_from_slice(&(value.len() as u64).to_ne_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(value);
    }
    std::fs::write(path, bytes).unwrap();
}

fn read_checkpoint_snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
    fn word(bytes: &[u8], offset: &mut usize) -> usize {
        let value = u64::from_ne_bytes(bytes[*offset..*offset + 8].try_into().unwrap());
        *offset += 8;
        value as usize
    }
    let bytes = std::fs::read(path).unwrap();
    let mut offset = 0;
    let count = word(&bytes, &mut offset);
    let mut image = BTreeMap::new();
    for _ in 0..count {
        let name_size = word(&bytes, &mut offset);
        let value_size = word(&bytes, &mut offset);
        let name = std::str::from_utf8(&bytes[offset..offset + name_size])
            .unwrap()
            .to_owned();
        offset += name_size;
        let value = bytes[offset..offset + value_size].to_vec();
        offset += value_size;
        image.insert(name, value);
    }
    assert_eq!(offset, bytes.len());
    image
}

impl CheckpointSink for AtomicStore {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn begin_until(&self, deadline: Instant) -> Result<NonZeroU64, CompositionError> {
        let mut state = self.0.lock().unwrap();
        if state.owner.is_some() || Instant::now() >= deadline {
            return Err(CompositionError::TransactionBusy);
        }
        state.next = state.next.wrapping_add(1).max(1);
        let owner = NonZeroU64::new(state.next).unwrap();
        state.staging.clear();
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
        let mut state = self.0.lock().unwrap();
        Self::validate(&state, owner, deadline)?;
        state.staging.insert(name.into(), bytes.into());
        Ok(())
    }

    fn abort_until(&self, owner: NonZeroU64, deadline: Instant) -> Result<(), CompositionError> {
        let mut state = self.0.lock().unwrap();
        Self::validate(&state, owner, deadline)?;
        state.staging.clear();
        state.owner = None;
        Ok(())
    }

    fn commit_until(&self, owner: NonZeroU64, manifest: &[u8], deadline: Instant) -> Result<(), CompositionError> {
        let mut state = self.0.lock().unwrap();
        Self::validate(&state, owner, deadline)?;
        state.staging.insert("MANIFEST".into(), manifest.into());
        state.committed = std::mem::take(&mut state.staging);
        state.owner = None;
        Ok(())
    }
}

impl CheckpointSource for AtomicStore {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CompositionError> {
        self.0
            .lock()
            .unwrap()
            .committed
            .get(name)
            .cloned()
            .ok_or(CompositionError::RuntimeConstruction)
    }

    fn list(&self) -> Result<Vec<String>, CompositionError> {
        Ok(self.0.lock().unwrap().committed.keys().cloned().collect())
    }

    fn get_until(&self, name: &str, deadline: Instant) -> Result<Vec<u8>, CompositionError> {
        (Instant::now() < deadline)
            .then(|| self.get(name))
            .ok_or(CompositionError::DeadlineExceeded)?
    }

    fn list_until(&self, deadline: Instant) -> Result<Vec<String>, CompositionError> {
        (Instant::now() < deadline)
            .then(|| self.list())
            .ok_or(CompositionError::DeadlineExceeded)?
    }
}

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

fn set_manifest_foreground_group(store: &Store, group: i32) {
    let mut image = store.0.lock().unwrap();
    let manifest = image.get_mut("MANIFEST").expect("checkpoint manifest");
    manifest[60..64].copy_from_slice(&group.to_ne_bytes());
}

fn process_ids_for_executable(executable: &Path) -> Vec<u32> {
    let needle = executable.as_os_str().as_encoded_bytes();
    let mut pids = std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|command| command.split(|byte| *byte == 0).any(|argument| argument == needle))
        })
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids
}

#[derive(Clone, Copy, Debug)]
struct ProcessIdentity {
    pid: u32,
    parent: u32,
    session: u32,
    start_time: u64,
    executable_device: u64,
    executable_inode: u64,
}

fn verified_process_identity(pid: u32) -> Option<ProcessIdentity> {
    let (before, before_state) = process_identity(pid)?;
    if before_state == 'Z' {
        return None;
    }
    let executable = std::fs::metadata(format!("/proc/{pid}/exe")).ok()?;
    let (after, after_state) = process_identity(pid)?;
    (before.pid == after.pid
        && before.parent == after.parent
        && before.session == after.session
        && before.start_time == after.start_time
        && after_state != 'Z')
        .then_some(ProcessIdentity {
            executable_device: executable.dev(),
            executable_inode: executable.ino(),
            ..after
        })
}

#[derive(Clone, Copy, Debug)]
struct SharedProcessTree {
    harness: ProcessIdentity,
    root: ProcessIdentity,
    child: ProcessIdentity,
}

fn revalidate_process_identity(expected: ProcessIdentity) -> ProcessIdentity {
    verified_process_identity(expected.pid)
        .filter(|actual| {
            actual.parent == expected.parent
                && actual.session == expected.session
                && actual.start_time == expected.start_time
        })
        .filter(|actual| {
            actual.executable_device == expected.executable_device
                && actual.executable_inode == expected.executable_inode
        })
        .unwrap_or_else(|| panic!("shared fixture identity is stale, replaced, dead, or zombie: {expected:?}"))
}

fn revalidate_shared_process_tree(expected: SharedProcessTree) {
    let harness = revalidate_process_identity(expected.harness);
    let root = revalidate_process_identity(expected.root);
    let child = revalidate_process_identity(expected.child);
    assert_eq!(
        root.parent, harness.pid,
        "shared fixture harness/root lineage changed: {expected:?}"
    );
    assert_eq!(
        root.session, root.pid,
        "shared fixture root is not its session leader: {expected:?}"
    );
    assert_eq!(
        child.parent, root.pid,
        "shared fixture root/child lineage changed: {expected:?}"
    );
    assert_eq!(
        child.session, root.pid,
        "shared fixture workers changed session: {expected:?}"
    );
    assert_eq!(root.executable_device, harness.executable_device);
    assert_eq!(root.executable_inode, harness.executable_inode);
    assert_eq!(child.executable_device, harness.executable_device);
    assert_eq!(child.executable_inode, harness.executable_inode);
}

struct SharedSession {
    engine: Arc<Engine>,
    exit: std::sync::mpsc::Receiver<Result<hl_engine::engine::EngineExit, EngineError>>,
    processes: Option<SharedProcessTree>,
    armed: bool,
}

impl SharedSession {
    fn start(engine: Arc<Engine>) -> Result<Self, EngineError> {
        engine.start()?;
        let (sender, exit) = std::sync::mpsc::sync_channel(1);
        let waiting = engine.clone();
        std::thread::spawn(move || {
            let _ = sender.send(waiting.wait());
        });
        Ok(Self {
            engine,
            exit,
            processes: None,
            armed: true,
        })
    }

    fn record(&mut self, processes: SharedProcessTree) {
        self.processes = Some(processes);
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn finish(&mut self, context: &str) -> Result<hl_engine::engine::EngineExit, String> {
        let exit = self
            .exit
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| format!("{context} did not exit within 10 seconds: {error}"))?
            .map_err(|error| format!("{context} failed: {error:?}"))?;
        if let Some(processes) = self.processes {
            wait_for_shared_child_reap_result(processes)?;
        }
        self.disarm();
        Ok(exit)
    }
}

impl Drop for SharedSession {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.engine.stop(StopRequest::Force);
        if let Some(processes) = self.processes {
            let _ = wait_for_shared_child_reap_result(processes);
        }
    }
}

fn process_identity(pid: u32) -> Option<(ProcessIdentity, char)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(')')?
        .1
        .trim_start()
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    Some((
        ProcessIdentity {
            pid,
            parent: fields.get(1)?.parse().ok()?,
            session: fields.get(3)?.parse().ok()?,
            start_time: fields.get(19)?.parse().ok()?,
            executable_device: 0,
            executable_inode: 0,
        },
        fields.first()?.chars().next()?,
    ))
}

fn processes_holding_file(path: &Path) -> Vec<ProcessIdentity> {
    let file = std::fs::metadata(path).expect("shared fixture output metadata");
    let mut holders = std::fs::read_dir("/proc")
        .expect("host process directory")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_dir(format!("/proc/{pid}/fd")).is_ok_and(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    std::fs::metadata(entry.path())
                        .is_ok_and(|candidate| candidate.dev() == file.dev() && candidate.ino() == file.ino())
                })
            })
        })
        .filter_map(verified_process_identity)
        .collect::<Vec<_>>();
    holders.sort_by_key(|identity| identity.pid);
    holders
}

fn shared_process_tree(engine: &Arc<Engine>, output: &Path) -> SharedProcessTree {
    let deadline = Instant::now() + Duration::from_secs(10);
    let harness = verified_process_identity(std::process::id()).expect("stable checkpoint harness identity");
    let (root, child) = loop {
        revalidate_process_identity(harness);
        let holders = processes_holding_file(output);
        if holders.len() == 2 {
            if let Some(root) = holders.iter().find(|candidate| candidate.session == candidate.pid) {
                if let Some(child) = holders.iter().find(|candidate| candidate.parent == root.pid) {
                    if child.session == root.pid {
                        break (*root, *child);
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            let transcript = std::fs::read_to_string(output).unwrap_or_default();
            let _ = engine.stop(StopRequest::Force);
            let _ = wait_bounded(engine, "incomplete shared-state inode-holder cleanup");
            panic!("expected one session-leader root and its child holding output: {holders:?}\n{transcript}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let processes = SharedProcessTree { harness, root, child };
    revalidate_shared_process_tree(processes);
    processes
}

fn wait_for_shared_child_reap_result(processes: SharedProcessTree) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        revalidate_process_identity(processes.harness);
        let live = [processes.root, processes.child]
            .into_iter()
            .filter_map(|expected| {
                process_identity(expected.pid).filter(|(actual, _)| actual.start_time == expected.start_time)
            })
            .collect::<Vec<_>>();
        if live.is_empty() {
            return Ok(());
        }
        if let Some((actual, _)) = live.iter().find(|(_, state)| *state == 'Z') {
            return Err(format!("guest worker became an unreaped zombie: {actual:?}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("guest worker identities did not disappear: {live:?}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_exact_process_reap(executable: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pids = process_ids_for_executable(executable);
        if pids.is_empty() {
            return;
        }
        assert!(Instant::now() < deadline, "guest processes were not reaped: {pids:?}");
        std::thread::sleep(Duration::from_millis(10));
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

fn wait_for(path: &Path, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::fs::read_to_string(path).unwrap_or_default().contains(marker) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "guest did not publish {marker}:\n{}",
        std::fs::read_to_string(path).unwrap_or_default()
    );
}

fn wait_for_connected_restore(engine: &Arc<Engine>, isa: GuestIsa, ready: &Path, fallback: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if fallback.exists() {
            let cleanup = force_and_reap_bounded(engine);
            panic!(
                "{isa:?} restore re-entered the fixture instead of preserving fd 10: {}; cleanup={cleanup}",
                std::fs::read_to_string(fallback).unwrap_or_default(),
            );
        }
        if std::fs::read_to_string(ready)
            .unwrap_or_default()
            .contains("DONE fd=10 connected=1")
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let cleanup = force_and_reap_bounded(engine);
    panic!(
        "{isa:?} restored fd 10 did not carry AFTER across its original connected stream; ready={} fallback={} cleanup={cleanup}",
        std::fs::read_to_string(ready).unwrap_or_default(),
        std::fs::read_to_string(fallback).unwrap_or_default()
    );
}

fn force_and_reap_bounded(engine: &Arc<Engine>) -> String {
    let stop = engine.stop(StopRequest::Force);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let waiting = engine.clone();
    std::thread::spawn(move || {
        let _ = sender.send(waiting.wait());
    });
    match receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(wait) => format!("stop={stop:?} wait={wait:?}"),
        Err(error) => format!("stop={stop:?} reap_timeout={error}"),
    }
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

fn connected_unix_stream_plan(
    executable: &Path,
    ready: &Path,
    finish: &Path,
    guard: &Path,
    fallback: &Path,
    restore: bool,
) -> RuntimePlan {
    let mut options = Options::default();
    options
        .set(if restore { "HL_RESTORE" } else { "HL_CHECKPOINT" }, "1", true)
        .unwrap();
    RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: [
            executable.as_os_str().as_encoded_bytes().to_vec(),
            ready.as_os_str().as_encoded_bytes().to_vec(),
            finish.as_os_str().as_encoded_bytes().to_vec(),
            guard.as_os_str().as_encoded_bytes().to_vec(),
            fallback.as_os_str().as_encoded_bytes().to_vec(),
        ]
        .into(),
        environment: Vec::new(),
        result_path: None,
        options,
    }
}

fn socket_half_close_plan(executable: &Path, ready: &Path, mode: &str) -> RuntimePlan {
    let mut options = Options::default();
    options.set("HL_CHECKPOINT", "1", true).unwrap();
    RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: [
            executable.as_os_str().as_encoded_bytes().to_vec(),
            ready.as_os_str().as_encoded_bytes().to_vec(),
            mode.as_bytes().to_vec(),
        ]
        .into(),
        environment: Vec::new(),
        result_path: None,
        options,
    }
}

struct AmbientDescriptors {
    records: Vec<AmbientDescriptor>,
}

struct AmbientDescriptor {
    target: i32,
    path: PathBuf,
    identity: Identity,
}

impl AmbientDescriptors {
    fn inherited(directory: &Path) -> Self {
        let records = [3, 4, 17]
            .into_iter()
            .map(|target| {
                let path = directory.join(format!("ambient-{target}.lock"));
                AmbientDescriptor {
                    target,
                    path,
                    identity: descriptor::identity(target).unwrap(),
                }
            })
            .collect();
        let guard = Self { records };
        guard.assert_preserved("launcher inheritance");
        guard
    }

    fn assert_preserved(&self, phase: &str) {
        for record in &self.records {
            assert_eq!(
                descriptor::identity(record.target).unwrap(),
                record.identity,
                "{phase}: fd {}",
                record.target
            );
            // Prove the target itself owns the locking OFD, rather than accepting a leaked duplicate as
            // evidence: releasing through target must admit an independent OFD, then target must reacquire.
            descriptor::lock(record.target, Lock::Unlock).unwrap();
            let probe = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&record.path)
                .unwrap();
            descriptor::lock(probe.as_raw_fd(), Lock::ExclusiveNonblocking).unwrap();
            descriptor::lock(probe.as_raw_fd(), Lock::Unlock).unwrap();
            descriptor::lock(record.target, Lock::ExclusiveNonblocking).unwrap();
            let excluded = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&record.path)
                .unwrap();
            let error = descriptor::lock(excluded.as_raw_fd(), Lock::ExclusiveNonblocking)
                .expect_err("lock unexpectedly released");
            assert_eq!(
                error.raw_os_error(),
                Some(libc::EWOULDBLOCK),
                "{phase}: fd {}",
                record.target
            );
        }
    }
}

fn assert_ambient_locks_released(paths: &[PathBuf]) {
    for path in paths {
        let probe = std::fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
        descriptor::lock(probe.as_raw_fd(), Lock::ExclusiveNonblocking)
            .unwrap_or_else(|error| panic!("ambient lock remains for {path:?}: {error}"));
    }
}

fn start_with_closed_standard_descriptor(engine: &Engine, descriptor: StandardDescriptor) {
    let closed = descriptor::close_standard(descriptor).expect("close standard descriptor");
    let started = engine.start();
    closed
        .restore()
        .expect("restore standard descriptor after engine start");
    started.unwrap();
}

fn ambient_fd_round_trip(isa: GuestIsa, executable: &Path, ambient: &AmbientDescriptors) {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("output");
    let first = Arc::new(Store::default());
    let capture = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_plan(executable, directory.path(), false, true),
            streams(true),
            first.clone(),
            first.clone(),
        )
        .unwrap(),
    );
    start_with_closed_standard_descriptor(&capture, StandardDescriptor::Input);
    wait_for_running_marker(&capture, &output, "BOOT fd=3");
    ambient.assert_preserved("initial start");
    capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(wait_bounded(&capture, "ambient fd initial capture").guest_status, 0);
    ambient.assert_preserved("initial capture");

    std::fs::write(directory.path().join("cycle1"), []).unwrap();
    let second = Arc::new(Store::default());
    let recapture = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_plan(executable, directory.path(), true, true),
            streams(true),
            second.clone(),
            first,
        )
        .unwrap(),
    );
    start_with_closed_standard_descriptor(&recapture, StandardDescriptor::Output);
    wait_for_running_marker(&recapture, &output, "CYCLE 1 fd=3");
    ambient.assert_preserved("first restore");
    recapture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(wait_bounded(&recapture, "ambient fd recapture").guest_status, 0);
    ambient.assert_preserved("recapture");

    std::fs::write(directory.path().join("cycle2"), []).unwrap();
    let restore = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_plan(executable, directory.path(), true, false),
            streams(true),
            second.clone(),
            second,
        )
        .unwrap(),
    );
    start_with_closed_standard_descriptor(&restore, StandardDescriptor::Error);
    wait_for_running_marker(&restore, &output, "DONE ambient-fd-ok fd=3");
    ambient.assert_preserved("second restore");
    std::fs::write(directory.path().join("finish"), []).unwrap();
    assert_eq!(wait_bounded(&restore, "ambient fd final restore").guest_status, 0);
    ambient.assert_preserved("final exit");
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "BOOT fd=3\nCYCLE 1 fd=3\nDONE ambient-fd-ok fd=3\n"
    );
}

#[test]
fn ambient_host_descriptors_do_not_shift_guest_fds_across_checkpoint_restore() {
    const CHILD: &str = "HL_AMBIENT_FD_CHECKPOINT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let fixtures = tempfile::tempdir().unwrap();
        let ambient_directory =
            PathBuf::from(std::env::var_os("HL_AMBIENT_FD_DIRECTORY").expect("ambient fd directory"));
        let ambient = AmbientDescriptors::inherited(&ambient_directory);
        for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
            ambient_fd_round_trip(isa, &ambient_fd_fixture(isa, fixtures.path()), &ambient);
        }
        return;
    }
    /* The gate must live in the parent test process. Its child has a distinct RwLock and cannot coordinate
     * with sibling checkpoint tests still running here. Hold this guard through spawn and bounded wait. */
    let _exclusive = exclusive_checkpoint_test();
    let ambient_directory = tempfile::tempdir().unwrap();
    let paths = [3, 4, 17].map(|descriptor| ambient_directory.path().join(format!("ambient-{descriptor}.lock")));
    let launcher = ambient_fd_launcher(ambient_directory.path());
    let output = tempfile::NamedTempFile::new().unwrap();
    let error = tempfile::NamedTempFile::new().unwrap();
    let mut child = std::process::Command::new(launcher)
        .arg(ambient_directory.path())
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ambient_host_descriptors_do_not_shift_guest_fds_across_checkpoint_restore",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdout(output.reopen().unwrap())
        .stderr(error.reopen().unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!(
                "ambient fd checkpoint child timed out\n{}",
                std::fs::read_to_string(error.path()).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        status.success(),
        "ambient fd checkpoint child failed with {status}\nstdout:\n{}\nstderr:\n{}",
        std::fs::read_to_string(output.path()).unwrap(),
        std::fs::read_to_string(error.path()).unwrap()
    );
    assert_ambient_locks_released(&paths);
}

fn wait_bounded(engine: &Arc<Engine>, context: &str) -> hl_engine::engine::EngineExit {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let waiting = engine.clone();
    std::thread::spawn(move || {
        let _ = sender.send(waiting.wait());
    });
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result.unwrap_or_else(|error| panic!("{context} failed: {error:?}")),
        Err(error) => {
            let _ = engine.stop(hl_engine::engine::StopRequest::Force);
            panic!("{context} did not exit within 10 seconds: {error}");
        }
    }
}

fn wait_result_bounded(
    engine: &Arc<Engine>,
    context: &str,
) -> Result<hl_engine::engine::EngineExit, hl_engine::engine::EngineError> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let waiting = engine.clone();
    std::thread::spawn(move || {
        let _ = sender.send(waiting.wait());
    });
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result,
        Err(initial) => {
            let stop = engine.stop(StopRequest::Force);
            match receiver.recv_timeout(Duration::from_secs(5)) {
                Ok(result) => {
                    panic!("{context} exceeded 10 seconds ({initial}); forced stop={stop:?}, bounded reap={result:?}")
                }
                Err(reap) => panic!(
                    "{context} exceeded 10 seconds ({initial}); forced stop={stop:?}, and did not reap within 5 seconds: {reap}"
                ),
            }
        }
    }
}

fn wait_for_running_marker(engine: &Arc<Engine>, path: &Path, marker: &str) {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let waiting = engine.clone();
    std::thread::spawn(move || {
        let _ = sender.send(waiting.wait());
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = std::fs::read_to_string(path).unwrap_or_default();
        if output.contains(marker) {
            return;
        }
        if let Ok(result) = receiver.try_recv() {
            panic!("guest exited before {marker}: {result:?}\n{output}");
        }
        if Instant::now() >= deadline {
            let _ = engine.stop(hl_engine::engine::StopRequest::Force);
            panic!("guest did not publish {marker} within 10 seconds:\n{output}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_shared_marker(session: &mut SharedSession, path: &Path, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let output = std::fs::read_to_string(path).unwrap_or_default();
        if output.ends_with(marker) {
            return;
        }
        if let Ok(result) = session.exit.try_recv() {
            session.disarm();
            panic!("guest exited before ordered marker {marker:?}: {result:?}\n{output}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = session.engine.stop(StopRequest::Force);
    let status = session.finish("forced shared-state cleanup");
    panic!(
        "guest did not publish ordered marker {marker:?}; cleanup={status:?}:\n{}",
        std::fs::read_to_string(path).unwrap_or_default()
    );
}

struct PhaseTimings {
    isa: GuestIsa,
    started: Instant,
    prior: Duration,
}

#[derive(Debug)]
struct CheckpointPhaseRow<'a> {
    component: &'a str,
    isa: &'a str,
    generation: u64,
    session: u64,
    attempt: u64,
    clock: &'a str,
    outcome: &'a str,
    status: u32,
    phase: &'a str,
    duration_us: u64,
    budget_us: u64,
}

fn checkpoint_phase_rows(output: &str) -> Vec<CheckpointPhaseRow<'_>> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("checkpoint_phase_ledger\t"))
        .map(|line| {
            let fields = line
                .split('\t')
                .filter_map(|field| field.split_once('='))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                fields.keys().copied().collect::<Vec<_>>(),
                [
                    "attempt",
                    "budget_us",
                    "clock",
                    "component",
                    "duration_us",
                    "generation",
                    "isa",
                    "outcome",
                    "phase",
                    "session",
                    "status",
                ],
                "invalid checkpoint phase-ledger schema: {line}"
            );
            CheckpointPhaseRow {
                component: fields["component"],
                isa: fields["isa"],
                generation: fields["generation"].parse().unwrap(),
                session: fields["session"].parse().unwrap(),
                attempt: fields["attempt"].parse().unwrap(),
                clock: fields["clock"],
                outcome: fields["outcome"],
                status: fields["status"].parse().unwrap(),
                phase: fields["phase"],
                duration_us: fields["duration_us"].parse().unwrap(),
                budget_us: fields["budget_us"].parse().unwrap(),
            }
        })
        .collect()
}

fn outer_phase_us(output: &str, isa: &str, phase: &str) -> Vec<u64> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("checkpoint_phase_timing\t"))
        .filter_map(|line| {
            let fields = line
                .split('\t')
                .filter_map(|field| field.split_once('='))
                .collect::<BTreeMap<_, _>>();
            (fields.get("isa") == Some(&isa) && fields.get("phase") == Some(&phase))
                .then(|| fields["duration_us"].parse().unwrap())
        })
        .collect()
}

fn validate_phase_ledger_contract(output: &str) -> Result<(), String> {
    let rows = checkpoint_phase_rows(output);
    if rows
        .iter()
        .any(|row| !matches!(row.component, "native" | "control") || !matches!(row.isa, "aarch64" | "x86_64"))
    {
        return Err("checkpoint ledger contains an unknown component or ISA".to_owned());
    }
    let capture = [
        "peer_quiescence",
        "serialization",
        "settlement",
        "manifest_publication",
        "native_reap",
    ];
    let restore = [
        "restore_validation",
        "restore_resources_memory",
        "restore_process_commit",
    ];
    let expected_native = capture
        .into_iter()
        .chain(["terminal"])
        .chain(restore)
        .chain(["terminal"])
        .chain(capture)
        .chain(["terminal"])
        .chain(restore)
        .chain(["terminal"])
        .collect::<Vec<_>>();
    let expected_control = [
        "capture_ready_wait",
        "capture_admission",
        "request_dispatch",
        "completion_wait",
        "terminal",
        "recovery_wait",
        "terminal",
        "capture_ready_wait",
        "capture_admission",
        "request_dispatch",
        "completion_wait",
        "terminal",
        "recovery_wait",
        "terminal",
    ];
    for isa in ["aarch64", "x86_64"] {
        let native = rows
            .iter()
            .filter(|row| row.component == "native" && row.isa == isa)
            .collect::<Vec<_>>();
        let control = rows
            .iter()
            .filter(|row| row.component == "control" && row.isa == isa)
            .collect::<Vec<_>>();
        if native.iter().map(|row| row.phase).collect::<Vec<_>>() != expected_native {
            return Err(format!("{isa} native phase order/count mismatch"));
        }
        if control.iter().map(|row| row.phase).collect::<Vec<_>>() != expected_control {
            return Err(format!("{isa} control phase order/count mismatch"));
        }
        for row in native.iter().chain(&control) {
            let valid_outcome = if row.phase == "terminal" {
                matches!((row.outcome, row.status), ("success", 0) | ("failure", 1..=u32::MAX))
            } else {
                row.outcome == "progress" && row.status == 0
            };
            let correlated = if row.phase == "terminal" && row.outcome == "failure" && row.generation == 0 {
                row.session != 0 && row.attempt == row.session
            } else {
                row.generation != 0 && row.session == row.generation && row.attempt == row.generation
            };
            if row.clock != "ok" || !valid_outcome || !correlated {
                return Err(format!("{isa} invalid correlated phase row: {row:?}"));
            }
        }
        let native_capture = native
            .iter()
            .filter(|row| capture.contains(&row.phase))
            .copied()
            .collect::<Vec<_>>();
        let control_capture = control
            .iter()
            .filter(|row| row.phase == "capture_admission")
            .collect::<Vec<_>>();
        for (phases, control) in native_capture.chunks_exact(capture.len()).zip(control_capture) {
            if phases.iter().any(|row| row.generation != control.generation) {
                return Err(format!("{isa} native/control capture generation mismatch"));
            }
            let settlement = phases[2];
            match settlement.budget_us {
                2_000_000 if settlement.duration_us >= 1_800_000 => {}
                0 if settlement.duration_us < 100_000 => {}
                _ => return Err(format!("{isa} settlement threshold/contract mismatch: {settlement:?}")),
            }
            let real = phases
                .iter()
                .filter(|row| row.phase != "settlement")
                .map(|row| row.duration_us)
                .sum::<u64>();
            if real >= 1_000_000 {
                return Err(format!("{isa} non-settlement work exceeded one second"));
            }
        }
        for row in native.iter().filter(|row| restore.contains(&row.phase)) {
            if row.budget_us != 0 || row.duration_us >= 1_000_000 {
                return Err(format!("{isa} restore phase threshold/contract mismatch: {row:?}"));
            }
        }
        let native_restore = native
            .iter()
            .filter(|row| row.phase == "restore_validation")
            .map(|row| row.generation)
            .collect::<Vec<_>>();
        let control_restore = control
            .iter()
            .filter(|row| row.phase == "recovery_wait")
            .map(|row| row.generation)
            .collect::<Vec<_>>();
        if native_restore != control_restore {
            return Err(format!("{isa} native/control restore generation mismatch"));
        }
    }
    Ok(())
}

fn validate_early_failure_ledger(output: &str) -> Result<(), String> {
    let rows = checkpoint_phase_rows(output);
    if rows.len() != 1 {
        return Err("early failure ledger must contain exactly one terminal row".to_owned());
    }
    let row = &rows[0];
    if row.component != "control"
        || !matches!(row.isa, "aarch64" | "x86_64")
        || row.phase != "terminal"
        || row.outcome != "failure"
        || row.status == 0
        || row.generation != 0
        || row.session == 0
        || row.attempt != row.session
        || row.duration_us != 0
        || row.budget_us != 0
        || row.clock != "ok"
    {
        return Err(format!("invalid early failure terminal: {row:?}"));
    }
    Ok(())
}

fn mutate_phase_field(
    output: &str,
    isa: Option<&str>,
    phase: &str,
    occurrence: usize,
    field: &str,
    value: &str,
) -> String {
    let marker = format!("phase={phase}");
    let prefix = format!("{field}=");
    let mut matched = 0;
    output
        .lines()
        .map(|line| {
            if !line.starts_with("checkpoint_phase_ledger\t")
                || !line.contains(&marker)
                || isa.is_some_and(|isa| !line.contains(&format!("isa={isa}\t")))
            {
                return line.to_owned();
            }
            let selected = matched == occurrence;
            matched += 1;
            if !selected {
                return line.to_owned();
            }
            line.split('\t')
                .map(|item| {
                    item.strip_prefix(&prefix)
                        .map_or_else(|| item.to_owned(), |_| format!("{field}={value}"))
                })
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn remove_or_duplicate_phase_row(output: &str, duplicate: bool) -> String {
    let mut changed = false;
    output
        .lines()
        .flat_map(|line| {
            if !changed && line.starts_with("checkpoint_phase_ledger\t") && line.contains("phase=peer_quiescence") {
                changed = true;
                if duplicate {
                    vec![line.to_owned(), line.to_owned()]
                } else {
                    Vec::new()
                }
            } else {
                vec![line.to_owned()]
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl PhaseTimings {
    fn new(isa: GuestIsa, prior: Duration) -> Self {
        Self {
            isa,
            started: Instant::now(),
            prior,
        }
    }

    fn observe(&self, phase: &str, started: Instant) {
        eprintln!(
            "checkpoint_phase_timing\tisa={}\tphase={phase}\tduration_us={}",
            match self.isa {
                GuestIsa::Aarch64 => "aarch64",
                GuestIsa::X86_64 => "x86_64",
            },
            started.elapsed().as_micros()
        );
    }

    fn finish(&self) {
        eprintln!(
            "checkpoint_phase_timing\tisa={}\tphase=total\tduration_us={}",
            match self.isa {
                GuestIsa::Aarch64 => "aarch64",
                GuestIsa::X86_64 => "x86_64",
            },
            (self.prior + self.started.elapsed()).as_micros()
        );
    }
}

fn daily_dev_round_trip(isa: GuestIsa, executable: &Path, fixture_compile: Duration) {
    let timings = PhaseTimings::new(isa, fixture_compile);
    eprintln!(
        "checkpoint_phase_timing\tisa={}\tphase=fixture_compile\tduration_us={}",
        match isa {
            GuestIsa::Aarch64 => "aarch64",
            GuestIsa::X86_64 => "x86_64",
        },
        fixture_compile.as_micros()
    );
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("output");
    let first = Arc::new(Store::default());
    let capture = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_phase_plan(isa, executable, directory.path(), false, true),
            streams(true),
            first.clone(),
            first.clone(),
        )
        .unwrap(),
    );
    let mut capture = RestoreGateEngine::new(capture, executable);
    let initial_ready = Instant::now();
    capture.start("initial capture launch");
    capture.wait_marker(&output_path, "READY leader=", "initial guest ready");
    capture.wait_marker(&directory.path().join("state"), "5", "initial progress");
    timings.observe("initial_guest_ready", initial_ready);
    let capture_request = Instant::now();
    capture.checkpoint("initial capture");
    timings.observe("capture_request_return", capture_request);
    let capture_wait = Instant::now();
    assert_eq!(capture.wait("initial daily-dev capture").guest_status, 0);
    wait_for_exact_process_reap(executable);
    timings.observe("capture_wait_reap", capture_wait);

    std::fs::write(directory.path().join("cycle1"), []).unwrap();
    let restore1_ready = Instant::now();
    let second = Arc::new(Store::default());
    let recapture = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_phase_plan(isa, executable, directory.path(), true, true),
            streams(true),
            second.clone(),
            first,
        )
        .unwrap(),
    );
    let mut recapture = RestoreGateEngine::new(recapture, executable);
    recapture.start("first restore launch");
    recapture.wait_marker(&output_path, "CYCLE 1 progress=", "first restore ready");
    timings.observe("restore1_start_ready", restore1_ready);
    let recapture_request = Instant::now();
    recapture.checkpoint("recapture after first restore");
    timings.observe("recapture_request_return", recapture_request);
    let recapture_wait = Instant::now();
    assert_eq!(recapture.wait("daily-dev recapture").guest_status, 0);
    wait_for_exact_process_reap(executable);
    timings.observe("recapture_wait_reap", recapture_wait);

    std::fs::write(directory.path().join("cycle2"), []).unwrap();
    let restore2_ready = Instant::now();
    let restore = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_phase_plan(isa, executable, directory.path(), true, false),
            streams(true),
            second.clone(),
            second,
        )
        .unwrap(),
    );
    let mut restore = RestoreGateEngine::new(restore, executable);
    restore.start("second restore launch");
    restore.wait_marker(&output_path, "CYCLE 2 progress=", "second restore ready");
    timings.observe("restore2_start_ready", restore2_ready);
    let final_shutdown = Instant::now();
    std::fs::write(directory.path().join("stop"), []).unwrap();
    assert_eq!(restore.wait("final daily-dev restore").guest_status, 0);
    wait_for_exact_process_reap(executable);
    timings.observe("final_shutdown_reap", final_shutdown);

    let output = std::fs::read_to_string(&output_path).unwrap();
    assert_eq!(output.matches("READY leader=").count(), 1, "{output}");
    assert_eq!(output.matches("SLEEP-READY ").count(), 1, "{output}");
    assert_eq!(output.matches("CYCLE 1 ").count(), 1, "{output}");
    assert_eq!(output.matches("CYCLE 2 ").count(), 1, "{output}");
    assert_eq!(output.matches("DONE progress=").count(), 1, "{output}");
    assert!(
        !output.contains("SLEEP-RETURN"),
        "sleep(1000) returned during checkpointing:\n{output}"
    );
    let progress = output
        .lines()
        .filter_map(|line| line.strip_prefix("CYCLE "))
        .map(|line| {
            line.split("progress=")
                .nth(1)
                .and_then(|value| value.split_ascii_whitespace().next())
                .unwrap()
                .parse::<u64>()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(progress.len(), 2, "{output}");
    assert!(
        progress[1] >= progress[0] + 5,
        "workload did not progress across restore: {progress:?}\n{output}"
    );
    let persisted = std::fs::read_to_string(directory.path().join("state"))
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    assert!(
        persisted >= progress[1],
        "durable state regressed: {persisted} < {}",
        progress[1]
    );
    timings.finish();
}

struct RestoreGateEngine<'a> {
    engine: Arc<Engine>,
    executable: &'a Path,
    waiter: Option<std::thread::JoinHandle<Result<hl_engine::engine::EngineExit, EngineError>>>,
}

impl<'a> RestoreGateEngine<'a> {
    fn new(engine: Arc<Engine>, executable: &'a Path) -> Self {
        Self {
            engine,
            executable,
            waiter: None,
        }
    }

    fn start(&mut self, context: &str) {
        self.engine
            .start()
            .unwrap_or_else(|error| panic!("{context}: {error:?}; cleanup={}", self.cleanup()));
        let engine = self.engine.clone();
        self.waiter = Some(std::thread::spawn(move || engine.wait()));
    }

    fn wait_marker(&mut self, path: &Path, marker: &str, context: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let output = std::fs::read_to_string(path).unwrap_or_default();
            if output.contains(marker) {
                return;
            }
            if self.waiter.as_ref().is_some_and(std::thread::JoinHandle::is_finished) {
                let result = self.join_waiter();
                panic!(
                    "{context}: guest exited before {marker}: {result:?}; survivors={}\n{output}",
                    self.survivors()
                );
            }
            if Instant::now() >= deadline {
                panic!(
                    "{context}: guest did not publish {marker}; cleanup={}\n{output}",
                    self.cleanup()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn checkpoint(&mut self, context: &str) {
        if let Err(error) = self.engine.capture_checkpoint_until(checkpoint_deadline()) {
            panic!("{context}: {error:?}; cleanup={}", self.cleanup());
        }
    }

    fn wait(&mut self, context: &str) -> hl_engine::engine::EngineExit {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.waiter.as_ref().is_some_and(std::thread::JoinHandle::is_finished) {
            if Instant::now() >= deadline {
                panic!("{context}: wait timed out; cleanup={}", self.cleanup());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.join_waiter()
            .unwrap_or_else(|error| panic!("{context}: {error:?}; survivors={}", self.survivors()))
    }

    fn join_waiter(&mut self) -> Result<hl_engine::engine::EngineExit, EngineError> {
        self.waiter
            .take()
            .expect("started gate engine owns a waiter")
            .join()
            .unwrap_or(Err(EngineError::LaunchFailed))
    }

    fn cleanup(&mut self) -> String {
        let stop = self.engine.stop(StopRequest::Force);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.waiter.as_ref().is_some_and(|waiter| !waiter.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let bounded = self.waiter.as_ref().is_none_or(std::thread::JoinHandle::is_finished);
        let survivors_before_join = self.survivors();
        // Force-stop is the engine's cancellation primitive. Always join after
        // requesting it: the acceptance command wraps the entire test process
        // in an outer timeout, so a broken cancellation contract kills and
        // reaps that subprocess instead of detaching a Rust thread in-process.
        let wait = self.waiter.is_some().then(|| self.join_waiter());
        format!(
            "stop={stop:?} bounded={bounded} wait={wait:?} survivors_before_join={survivors_before_join} survivors_after_join={}",
            self.survivors()
        )
    }

    fn survivors(&self) -> String {
        format!("{:?}", process_ids_for_executable(self.executable))
    }
}

impl Drop for RestoreGateEngine<'_> {
    fn drop(&mut self) {
        if self.waiter.is_some() {
            let cleanup = self.cleanup();
            if !process_ids_for_executable(self.executable).is_empty() {
                eprintln!("amd64 checkpoint gate cleanup incomplete: {cleanup}");
            }
        }
    }
}

fn shared_state_round_trip(isa: GuestIsa, executable: &Path, expected: &str) {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("output");
    let first = Arc::new(Store::default());
    let capture = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_plan(executable, directory.path(), false, true),
            StandardStreams::default(),
            first.clone(),
            first.clone(),
        )
        .unwrap(),
    );
    let mut capture_session = SharedSession::start(capture.clone()).unwrap();
    wait_for_shared_marker(&mut capture_session, &output, "BOOT\nREADY\n");
    let capture_processes = shared_process_tree(&capture, &output);
    capture_session.record(capture_processes);
    revalidate_shared_process_tree(capture_processes);
    revalidate_shared_process_tree(capture_processes);
    capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(
        capture_session
            .finish("shared-state initial capture")
            .unwrap()
            .guest_status,
        0
    );

    std::fs::write(directory.path().join("cycle1"), []).unwrap();
    let second = Arc::new(Store::default());
    let recapture = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_plan(executable, directory.path(), true, true),
            StandardStreams::default(),
            second.clone(),
            first,
        )
        .unwrap(),
    );
    let mut recapture_session = SharedSession::start(recapture.clone()).unwrap();
    wait_for_shared_marker(&mut recapture_session, &output, "BOOT\nREADY\nCYCLE 1\n");
    let recapture_processes = shared_process_tree(&recapture, &output);
    recapture_session.record(recapture_processes);
    revalidate_shared_process_tree(recapture_processes);
    revalidate_shared_process_tree(recapture_processes);
    recapture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(
        recapture_session.finish("shared-state recapture").unwrap().guest_status,
        0
    );

    std::fs::write(directory.path().join("cycle2"), []).unwrap();
    let restore = Arc::new(
        Engine::with_checkpoint(
            isa,
            daily_dev_plan(executable, directory.path(), true, false),
            StandardStreams::default(),
            second.clone(),
            second,
        )
        .unwrap(),
    );
    let mut restore_session = SharedSession::start(restore.clone()).unwrap();
    wait_for_shared_marker(
        &mut restore_session,
        &output,
        &format!("BOOT\nREADY\nCYCLE 1\n{expected}\n"),
    );
    let restore_processes = shared_process_tree(&restore, &output);
    restore_session.record(restore_processes);
    revalidate_shared_process_tree(restore_processes);
    std::fs::write(directory.path().join("finish"), []).unwrap();
    assert_eq!(
        restore_session
            .finish("shared-state final restore")
            .unwrap()
            .guest_status,
        0
    );
    let output = std::fs::read_to_string(output).unwrap();
    assert_eq!(
        output,
        format!("BOOT\nREADY\nCYCLE 1\n{expected}\n"),
        "fresh start, duplicate generation, missing generation, or reordered transcript"
    );
}

fn external_access_round_trip(isa: GuestIsa, executable: &Path) {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("output");
    let inherited_create = directory.path().join("inherited-create");
    let inherited_delete = directory.path().join("inherited-delete");
    let restored_create = directory.path().join("restored-create");
    let restored_delete = directory.path().join("restored-delete");
    std::fs::write(&inherited_delete, []).unwrap();
    std::fs::write(&restored_delete, []).unwrap();

    let store = Arc::new(Store::default());
    let capture = Engine::with_checkpoint(
        isa,
        daily_dev_plan(executable, directory.path(), false, true),
        StandardStreams::default(),
        store.clone(),
        store.clone(),
    )
    .unwrap();
    capture.start().unwrap();
    wait_for(&output, "READY");
    capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(capture.wait().unwrap().guest_status, 0);

    std::fs::write(&inherited_create, []).unwrap();
    std::fs::remove_file(&inherited_delete).unwrap();
    let restore = Engine::with_checkpoint(
        isa,
        daily_dev_plan(executable, directory.path(), true, false),
        StandardStreams::default(),
        store.clone(),
        store,
    )
    .unwrap();
    restore.start().unwrap();
    std::fs::write(directory.path().join("release"), []).unwrap();
    wait_for(&output, "RESTORED-CACHED");
    std::fs::write(&restored_create, []).unwrap();
    std::fs::remove_file(&restored_delete).unwrap();
    std::fs::write(directory.path().join("mutate"), []).unwrap();
    assert_eq!(restore.wait().unwrap().guest_status, 0);
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        "READY\nRESTORED-CACHED\nDONE external-access-ok\n"
    );
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

#[test]
fn one_rejected_process_prevents_manifest_publication_on_both_isas() {
    const CHILD: &str = "HL_CHECKPOINT_REJECTED_MEMBER_ISA";
    const RESTORE_SNAPSHOT: &str = "HL_CHECKPOINT_RESTORE_SNAPSHOT";
    if let Some(snapshot) = std::env::var_os(RESTORE_SNAPSHOT) {
        let isa = match std::env::var(CHILD).as_deref() {
            Ok("aarch64") => GuestIsa::Aarch64,
            Ok("x86_64") => GuestIsa::X86_64,
            other => panic!("invalid {CHILD}={other:?}"),
        };
        let executable = PathBuf::from(std::env::var_os("HL_CHECKPOINT_RESTORE_EXECUTABLE").unwrap());
        let release = PathBuf::from(std::env::var_os("HL_CHECKPOINT_RESTORE_RELEASE").unwrap());
        let final_release = PathBuf::from(std::env::var_os("HL_CHECKPOINT_RESTORE_FINAL_RELEASE").unwrap());
        let output = PathBuf::from(format!("{}.output", release.display()));
        let store = Arc::new(AtomicStore::from_committed(read_checkpoint_snapshot(Path::new(
            &snapshot,
        ))));
        let restore = Arc::new(
            Engine::with_checkpoint(
                isa,
                plan(&executable, &release, &final_release, &["HL_RESTORE"]),
                StandardStreams::default(),
                store.clone(),
                store,
            )
            .unwrap(),
        );
        restore.start().unwrap();
        std::fs::write(&release, []).unwrap();
        for marker in ["CYCLE-READY 1", "CYCLE-READY 2", "CYCLE-READY 3"] {
            wait_for(&output, marker);
        }
        std::fs::write(&final_release, []).unwrap();
        assert_eq!(
            wait_result_bounded(&restore, "preserved generation A restore")
                .unwrap()
                .guest_status,
            0
        );
        return;
    }
    let Some(selected) = std::env::var_os(CHILD) else {
        for isa in ["aarch64", "x86_64"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "one_rejected_process_prevents_manifest_publication_on_both_isas",
                    "--nocapture",
                ])
                .env(CHILD, isa)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{isa} rejected-member child failed with {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let isa = match selected.to_str() {
        Some("aarch64") => GuestIsa::Aarch64,
        Some("x86_64") => GuestIsa::X86_64,
        other => panic!("invalid {CHILD}={other:?}"),
    };
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let valid_executable = fixture(isa, fixtures.path());
    let rejected_executable = rejected_member_fixture(isa, fixtures.path());
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let output = temporary.path().join("release.output");
        let store = Arc::new(AtomicStore::default());
        let accepted = Arc::new(
            Engine::with_checkpoint(
                isa,
                plan(&valid_executable, &release, &final_release, &["HL_CHECKPOINT"]),
                StandardStreams::default(),
                store.clone(),
                store.clone(),
            )
            .unwrap(),
        );
        accepted.start().unwrap();
        wait_ready(&output);
        accepted.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(
            wait_result_bounded(&accepted, "accepted generation A")
                .unwrap()
                .guest_status,
            0
        );
        let generation_a = store.snapshot();
        assert!(generation_a.contains_key("MANIFEST"));

        std::fs::write(&output, []).unwrap();
        let capture = Arc::new(
            Engine::with_checkpoint(
                isa,
                plan(&rejected_executable, &release, &final_release, &["HL_CHECKPOINT"]),
                StandardStreams::default(),
                store.clone(),
                store.clone(),
            )
            .unwrap(),
        );
        capture.start().unwrap();
        wait_ready(&output);
        let mut ready = std::fs::read_to_string(&output)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("READY "))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        ready.sort();
        assert_eq!(ready, ["READY 1", "READY 2", "READY 3"]);
        let transcript = std::fs::read_to_string(&output).unwrap();
        assert_eq!(transcript.lines().filter(|line| *line == "SECCOMP-ARMED 3").count(), 1);
        let mut capable = transcript
            .lines()
            .filter(|line| line.starts_with("CAPTURE-CAPABLE "))
            .collect::<Vec<_>>();
        capable.sort();
        assert_eq!(capable, ["CAPTURE-CAPABLE 1", "CAPTURE-CAPABLE 2"]);
        let live_deadline = Instant::now() + Duration::from_secs(5);
        let live = loop {
            let live = processes_holding_file(&output);
            if live.len() == 3 {
                break live;
            }
            assert!(
                Instant::now() < live_deadline,
                "{isa:?} expected exactly three live members: {live:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            capture.capture_checkpoint_until(checkpoint_deadline()).is_err(),
            "{isa:?} accepted a process tree containing a rejected member"
        );
        let _ = wait_result_bounded(&capture, "rejected-member checkpoint process tree");
        let reap_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = live
                .iter()
                .filter(|expected| {
                    process_identity(expected.pid).is_some_and(|(actual, _)| actual.start_time == expected.start_time)
                })
                .collect::<Vec<_>>();
            if remaining.is_empty() {
                break;
            }
            assert!(
                Instant::now() < reap_deadline,
                "{isa:?} leaked rejected checkpoint members: {remaining:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            store.snapshot(),
            generation_a,
            "{isa:?} changed generation A while rejecting generation B"
        );

        let snapshot = temporary.path().join("generation-a.bin");
        write_checkpoint_snapshot(&snapshot, &generation_a);
        let restored = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "one_rejected_process_prevents_manifest_publication_on_both_isas",
                "--nocapture",
            ])
            .env(
                CHILD,
                match isa {
                    GuestIsa::Aarch64 => "aarch64",
                    GuestIsa::X86_64 => "x86_64",
                },
            )
            .env(RESTORE_SNAPSHOT, &snapshot)
            .env("HL_CHECKPOINT_RESTORE_EXECUTABLE", &valid_executable)
            .env("HL_CHECKPOINT_RESTORE_RELEASE", &release)
            .env("HL_CHECKPOINT_RESTORE_FINAL_RELEASE", &final_release)
            .output()
            .unwrap();
        assert!(
            restored.status.success(),
            "{isa:?} could not restore preserved generation A in a clean process: {}\nstdout:\n{}\nstderr:\n{}",
            restored.status,
            String::from_utf8_lossy(&restored.stdout),
            String::from_utf8_lossy(&restored.stderr)
        );
    }
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
    let failed_restore_outcome = failed_restore.wait();
    assert!(
        failed_restore_outcome.is_err(),
        "injected post-fork restore failure did not fail the launch: {failed_restore_outcome:?}"
    );
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

fn process_groups(store: &Store) -> BTreeSet<String> {
    store
        .0
        .lock()
        .unwrap()
        .keys()
        .filter_map(|name| name.strip_suffix("/meta"))
        .filter(|name| name.starts_with("proc."))
        .map(str::to_owned)
        .collect()
}

fn ready_process_groups(output: &CapturedOutput) -> BTreeSet<String> {
    output
        .text()
        .lines()
        .filter(|line| line.starts_with("PIPE-READY "))
        .map(|line| {
            if line.starts_with("PIPE-READY parent ") {
                "proc.1".to_owned()
            } else {
                let (_, pid) = line.rsplit_once("pid=").expect("ready pid field");
                format!("proc.{}", pid.parse::<u32>().expect("numeric guest pid"))
            }
        })
        .collect()
}

fn wait_bounded_with_output(
    engine: &Arc<Engine>,
    context: &str,
    output: &CapturedOutput,
) -> hl_engine::engine::EngineExit {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let waiting = engine.clone();
    std::thread::spawn(move || {
        let _ = sender.send(waiting.wait());
    });
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result.unwrap_or_else(|error| panic!("{context} failed: {error:?}\n{}", output.text())),
        Err(error) => {
            let _ = engine.stop(StopRequest::Force);
            panic!("{context} did not exit within 10 seconds: {error}\n{}", output.text());
        }
    }
}

fn inherited_pipe_round_trip(isa: GuestIsa, executable: &Path) {
    let temporary = tempfile::tempdir().unwrap();
    let release = temporary.path().join("release");
    let final_release = temporary.path().join("final-release");
    let output = Arc::new(CapturedOutput::default());
    let first_store = Arc::new(Store::default());

    let capture = Arc::new(
        Engine::with_checkpoint(
            isa,
            plan(executable, &release, &final_release, &["HL_CHECKPOINT"]),
            StandardStreams::default().with_output(output.clone()),
            first_store.clone(),
            first_store.clone(),
        )
        .unwrap(),
    );
    capture.start().unwrap();
    for marker in ["PIPE-READY 0", "PIPE-READY 1", "PIPE-READY 2", "PIPE-READY parent"] {
        output.wait(marker);
    }
    let expected_groups = ready_process_groups(&output);
    assert_eq!(expected_groups.len(), 4, "{isa:?}: {}", output.text());
    capture
        .capture_checkpoint_until(checkpoint_deadline())
        .unwrap_or_else(|error| {
            panic!(
                "{isa:?} first inherited-pipe capture failed: {error:?}\n{}",
                output.text()
            )
        });
    assert_eq!(
        wait_bounded_with_output(&capture, "first inherited-pipe capture", &output).guest_status,
        0,
        "{isa:?}: {}",
        output.text()
    );
    let first_groups = process_groups(&first_store);
    assert_eq!(first_groups, expected_groups, "{isa:?}: {}", output.text());

    std::fs::write(temporary.path().join("release.go"), []).unwrap();
    let second_store = Arc::new(Store::default());
    let recapture = Arc::new(
        Engine::with_checkpoint(
            isa,
            plan(executable, &release, &final_release, &["HL_RESTORE", "HL_CHECKPOINT"]),
            StandardStreams::default().with_output(output.clone()),
            second_store.clone(),
            first_store,
        )
        .unwrap(),
    );
    recapture.start().unwrap();
    for role in 0..3 {
        output.wait(&format!("PIPE-CYCLE-READY {role}"));
    }
    recapture
        .capture_checkpoint_until(checkpoint_deadline())
        .unwrap_or_else(|error| panic!("{isa:?} inherited-pipe recapture failed: {error:?}\n{}", output.text()));
    assert_eq!(
        wait_bounded_with_output(&recapture, "second inherited-pipe capture", &output).guest_status,
        0,
        "{isa:?}: {}",
        output.text()
    );
    let second_groups = process_groups(&second_store);
    assert_eq!(second_groups, first_groups, "{isa:?} process identity set changed");

    std::fs::write(temporary.path().join("final-release.go"), []).unwrap();
    let restore = Arc::new(
        Engine::with_checkpoint(
            isa,
            plan(executable, &release, &final_release, &["HL_RESTORE"]),
            StandardStreams::default().with_output(output.clone()),
            second_store.clone(),
            second_store,
        )
        .unwrap(),
    );
    restore.start().unwrap();
    let restored = wait_bounded_with_output(&restore, "final inherited-pipe restore", &output);
    let captured = output.text();
    assert_eq!(restored.guest_status, 0, "{isa:?}: {captured}");
    assert!(captured.contains("PIPE-CONSUMED 1"), "{isa:?}: {captured}");
    assert!(captured.contains("PIPE-CONSUMED 2"), "{isa:?}: {captured}");
    assert!(captured.contains("PIPE-EOF"), "{isa:?}: {captured}");
    assert!(captured.contains("PIPE-TREE-RESTORED"), "{isa:?}: {captured}");
}

#[test]
fn inherited_pipe_ofd_survives_two_checkpoint_cycles_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, inherited_pipe_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        inherited_pipe_round_trip(isa, &executable);
    }
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
        let failed_restore_outcome = failed_restore.wait();
        assert!(
            failed_restore_outcome.is_err(),
            "injected trigger-reattachment failure did not fail the launch: {failed_restore_outcome:?}"
        );

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

fn daily_development_workload_survives_two_checkpoint_cycles(isa: GuestIsa) {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let executable = daily_dev_fixture(isa, fixtures.path());
    let fixture_compile = started.elapsed();
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    daily_dev_round_trip(isa, &executable, fixture_compile);
}

#[test]
fn aarch64_daily_development_workload_survives_two_checkpoint_cycles() {
    daily_development_workload_survives_two_checkpoint_cycles(GuestIsa::Aarch64);
}

#[test]
fn amd64_daily_development_workload_survives_two_checkpoint_cycles() {
    daily_development_workload_survives_two_checkpoint_cycles(GuestIsa::X86_64);
}

#[test]
#[ignore = "subprocess target for the checkpoint phase-ledger regression gate"]
fn checkpoint_phase_ledger_probe_child() {
    checkpoint_phase_ledger_probe();
}

#[test]
#[ignore = "subprocess target for exact-topology checkpoint phase timing"]
fn checkpoint_phase_ledger_exact_probe_child() {
    checkpoint_phase_ledger_probe();
}

#[test]
#[ignore = "subprocess target for injected checkpoint phase-clock failure"]
fn checkpoint_phase_ledger_clock_failure_probe_child() {
    checkpoint_phase_ledger_probe();
}

fn checkpoint_phase_ledger_probe() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let executable = daily_dev_fixture(GuestIsa::Aarch64, fixtures.path());
    let fixture_compile = started.elapsed();
    let x86_started = Instant::now();
    let x86_executable = daily_dev_fixture(GuestIsa::X86_64, fixtures.path());
    let x86_fixture_compile = x86_started.elapsed();
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    daily_dev_round_trip(GuestIsa::Aarch64, &executable, fixture_compile);
    daily_dev_round_trip(GuestIsa::X86_64, &x86_executable, x86_fixture_compile);
    let ledger = std::env::var_os("HL_CHECKPOINT_PHASE_LEDGER_PATH").expect("phase ledger path");
    let ledger = std::fs::read_to_string(ledger).unwrap();
    assert!(
        ledger.contains("component=native")
            && ledger.contains("phase=peer_quiescence")
            && ledger.contains("phase=restore_process_commit"),
        "native records did not survive guest stderr remapping: {ledger}"
    );
    eprint!("{ledger}");
}

fn phase_probe_output(test: &str, timeout: Duration) -> String {
    let capture = tempfile::tempdir().unwrap();
    let stdout = capture.path().join("stdout");
    let stderr_path = capture.path().join("stderr");
    let ledger_path = capture.path().join("ledger");
    let mut command = ProcessCommand::new(std::env::current_exe().unwrap());
    command.args(["--exact", test, "--ignored", "--nocapture", "--test-threads=1"]);
    let mut environment = std::env::vars_os()
        .map(|(name, value)| EnvironmentEntry::new(name.as_encoded_bytes(), value.as_encoded_bytes()).unwrap())
        .collect::<Vec<_>>();
    environment.push(
        EnvironmentEntry::new(
            b"HL_CHECKPOINT_PHASE_LEDGER_PATH",
            ledger_path.as_os_str().as_encoded_bytes(),
        )
        .unwrap(),
    );
    command.exact_environment(environment).unwrap();
    let outcome = hl_process::run(
        &command,
        &ProcessCapture {
            stdout,
            stderr: stderr_path.clone(),
            stdout_limit: 4 * 1024 * 1024,
            stderr_limit: 4 * 1024 * 1024,
        },
        timeout,
        &AtomicBool::new(false),
    )
    .unwrap();
    let stderr = std::fs::read_to_string(stderr_path).unwrap();
    assert_eq!(
        outcome,
        ProcessOutcome::Exited(Some(0)),
        "phase-ledger child {test} failed:\n{stderr}"
    );
    stderr
}

#[test]
fn checkpoint_phase_ledger_separates_the_fixed_wait_from_real_work() {
    let _exclusive = exclusive_checkpoint_test();
    let stderr = phase_probe_output("checkpoint_phase_ledger_probe_child", Duration::from_secs(20));
    validate_phase_ledger_contract(&stderr).unwrap();

    let exact_topology = phase_probe_output("checkpoint_phase_ledger_exact_probe_child", Duration::from_secs(12));
    validate_phase_ledger_contract(&exact_topology).expect("real zero-budget settlement must validate on both ISAs");
    let exact_rows = checkpoint_phase_rows(&exact_topology);
    for isa in ["aarch64", "x86_64"] {
        let settlements = exact_rows
            .iter()
            .filter(|row| row.isa == isa && row.phase == "settlement")
            .collect::<Vec<_>>();
        assert_eq!(settlements.len(), 2, "{isa} exact-topology settlement rows");
        assert!(
            settlements
                .iter()
                .all(|row| row.budget_us == 0 && row.duration_us < 100_000),
            "{isa} did not execute the real zero-budget settlement path: {settlements:?}"
        );
    }
    let failed_clock = phase_probe_output(
        "checkpoint_phase_ledger_clock_failure_probe_child",
        Duration::from_secs(20),
    );
    let failed_clock_rows = checkpoint_phase_rows(&failed_clock);
    assert!(!failed_clock_rows.is_empty());
    assert!(
        failed_clock_rows
            .iter()
            .all(|row| row.clock == "unavailable" && row.duration_us == 0),
        "injected clock failure did not reach both native and control ledgers: {failed_clock}"
    );
    assert!(validate_phase_ledger_contract(&failed_clock).is_err());
    let early_failure = "checkpoint_phase_ledger\tcomponent=control\tisa=aarch64\tsession=71\tattempt=71\tgeneration=0\tphase=terminal\tduration_us=0\tbudget_us=0\tclock=ok\toutcome=failure\tstatus=70";
    validate_early_failure_ledger(early_failure).expect("well-formed early failure terminal must be accepted");
    assert!(validate_phase_ledger_contract(early_failure).is_err());
    assert!(validate_early_failure_ledger(&early_failure.replace("status=70", "status=0")).is_err());
    assert!(validate_early_failure_ledger(&early_failure.replace("attempt=71", "attempt=72")).is_err());
    assert!(validate_early_failure_ledger(&early_failure.replace("isa=aarch64", "isa=garbage")).is_err());

    let mutations = [
        mutate_phase_field(&stderr, None, "peer_quiescence", 0, "phase", "wrong_order"),
        mutate_phase_field(&stderr, None, "peer_quiescence", 0, "component", "garbage"),
        mutate_phase_field(&stderr, None, "peer_quiescence", 0, "isa", "garbage"),
        mutate_phase_field(&stderr, None, "capture_ready_wait", 0, "generation", "0"),
        mutate_phase_field(&stderr, None, "capture_ready_wait", 0, "session", "0"),
        mutate_phase_field(&stderr, None, "capture_ready_wait", 0, "attempt", "0"),
        mutate_phase_field(&stderr, None, "settlement", 0, "budget_us", "17"),
        mutate_phase_field(&stderr, None, "serialization", 0, "clock", "unavailable"),
        mutate_phase_field(&stderr, None, "serialization", 0, "duration_us", "1000000"),
        mutate_phase_field(&stderr, None, "terminal", 0, "outcome", "failure"),
        mutate_phase_field(&stderr, Some("aarch64"), "settlement", 1, "duration_us", "1000000"),
        mutate_phase_field(&stderr, Some("x86_64"), "serialization", 0, "clock", "unavailable"),
        remove_or_duplicate_phase_row(&stderr, false),
        remove_or_duplicate_phase_row(&stderr, true),
    ];
    for mutation in mutations {
        assert!(
            std::panic::catch_unwind(|| validate_phase_ledger_contract(&mutation).unwrap()).is_err(),
            "phase-ledger mutation was not detected"
        );
    }
    let malformed = stderr.replacen("\tcomponent=", "\textra=1\tcomponent=", 1);
    assert!(
        std::panic::catch_unwind(|| validate_phase_ledger_contract(&malformed).unwrap()).is_err(),
        "phase-ledger schema mutation was not detected"
    );

    let rows = checkpoint_phase_rows(&stderr);
    assert!(
        rows.iter().all(|row| matches!(row.isa, "aarch64" | "x86_64")),
        "wrong or missing ISA tag:\n{stderr}"
    );
    assert!(
        rows.iter().all(|row| {
            row.clock == "ok"
                && row.generation != 0
                && row.session == row.generation
                && row.attempt == row.generation
                && matches!(row.component, "native" | "control")
        }),
        "uncorrelated or clock-invalid phase row:\n{stderr}"
    );
    let native = rows
        .iter()
        .filter(|row| row.component == "native" && row.isa == "aarch64")
        .collect::<Vec<_>>();
    let control = rows
        .iter()
        .filter(|row| row.component == "control" && row.isa == "aarch64")
        .collect::<Vec<_>>();
    assert_eq!(native.len(), 20, "missing or duplicate native phases:\n{stderr}");
    assert_eq!(control.len(), 14, "missing or duplicate control phases:\n{stderr}");
    let x86_native = rows
        .iter()
        .filter(|row| row.component == "native" && row.isa == "x86_64")
        .collect::<Vec<_>>();
    let x86_control = rows
        .iter()
        .filter(|row| row.component == "control" && row.isa == "x86_64")
        .collect::<Vec<_>>();
    assert_eq!(x86_native.len(), 20, "missing x86 native phases:\n{stderr}");
    assert_eq!(x86_control.len(), 14, "missing x86 control phases:\n{stderr}");

    let capture_phases = [
        "peer_quiescence",
        "serialization",
        "settlement",
        "manifest_publication",
        "native_reap",
    ];
    let restore_phases = [
        "restore_validation",
        "restore_resources_memory",
        "restore_process_commit",
    ];
    let expected_native = capture_phases
        .into_iter()
        .chain(["terminal"])
        .chain(restore_phases)
        .chain(["terminal"])
        .chain(capture_phases)
        .chain(["terminal"])
        .chain(restore_phases)
        .chain(["terminal"])
        .collect::<Vec<_>>();
    assert_eq!(
        x86_native.iter().map(|row| row.phase).collect::<Vec<_>>(),
        expected_native,
        "x86 checkpoint/restore phases were reordered:\n{stderr}"
    );
    let x86_captures = x86_native
        .iter()
        .filter(|row| capture_phases.contains(&row.phase))
        .copied()
        .collect::<Vec<_>>();
    for capture in x86_captures.chunks_exact(capture_phases.len()) {
        assert!(capture.iter().all(|row| row.generation == capture[0].generation));
        let settlement = capture[2];
        match settlement.budget_us {
            2_000_000 => assert!(settlement.duration_us >= settlement.budget_us * 9 / 10),
            0 => assert!(settlement.duration_us < 100_000),
            budget => panic!("unknown x86 settlement contract budget {budget}: {settlement:?}"),
        }
        assert!(
            capture
                .iter()
                .filter(|row| row.phase != "settlement")
                .map(|row| row.duration_us)
                .sum::<u64>()
                < 1_000_000,
            "x86 non-settlement work regressed: {capture:?}"
        );
    }
    assert_eq!(
        native.iter().map(|row| row.phase).collect::<Vec<_>>(),
        expected_native,
        "native checkpoint/restore phases were reordered:\n{stderr}"
    );
    for terminal in native.iter().chain(&x86_native).filter(|row| row.phase == "terminal") {
        assert_eq!(
            terminal.outcome, "success",
            "successful probe emitted failed terminal row: {terminal:?}"
        );
        assert_eq!(terminal.duration_us, 0);
        assert_eq!(terminal.budget_us, 0);
    }
    let captures = native
        .iter()
        .filter(|row| capture_phases.contains(&row.phase))
        .copied()
        .collect::<Vec<_>>();
    for capture in captures.chunks_exact(capture_phases.len()) {
        assert_ne!(
            capture[0].generation, 0,
            "native capture has no generation: {capture:?}"
        );
        assert!(
            capture.iter().all(|row| row.generation == capture[0].generation),
            "native capture phases crossed generations: {capture:?}"
        );
        let settlement = capture[2];
        let real_work = capture
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != 2)
            .map(|(_, row)| row.duration_us)
            .sum::<u64>();
        match settlement.budget_us {
            2_000_000 => assert!(
                settlement.duration_us >= settlement.budget_us * 9 / 10,
                "settlement did not execute its declared wait budget: {settlement:?}"
            ),
            0 => assert!(
                settlement.duration_us < 100_000,
                "exact-topology settlement regressed past 100 ms: {settlement:?}"
            ),
            budget => panic!("unknown settlement contract budget {budget}: {settlement:?}"),
        }
        assert!(
            real_work < 1_000_000,
            "checkpoint work outside settlement regressed past one second: {capture:?}"
        );
    }
    for restore in native
        .iter()
        .chain(&x86_native)
        .filter(|row| restore_phases.contains(&row.phase))
    {
        assert_eq!(
            restore.budget_us, 0,
            "restore phase unexpectedly declared a fixed wait: {restore:?}"
        );
        assert!(
            restore.duration_us < 1_000_000,
            "native restore phase regressed past one second: {restore:?}"
        );
    }
    let capture_generations = captures
        .chunks_exact(capture_phases.len())
        .map(|capture| capture[0].generation)
        .collect::<Vec<_>>();
    let control_capture_generations = control
        .iter()
        .filter(|row| row.phase == "capture_admission")
        .map(|row| row.generation)
        .collect::<Vec<_>>();
    assert_eq!(
        capture_generations, control_capture_generations,
        "native/control capture IDs diverged"
    );
    let restore_generations = native
        .iter()
        .filter(|row| row.phase == "restore_validation")
        .map(|row| row.generation)
        .collect::<Vec<_>>();
    let control_restore_generations = control
        .iter()
        .filter(|row| row.phase == "recovery_wait")
        .map(|row| row.generation)
        .collect::<Vec<_>>();
    assert_eq!(
        restore_generations, control_restore_generations,
        "native/control restore IDs diverged"
    );
    let x86_capture_generations = x86_captures
        .chunks_exact(capture_phases.len())
        .map(|capture| capture[0].generation)
        .collect::<Vec<_>>();
    let x86_control_capture_generations = x86_control
        .iter()
        .filter(|row| row.phase == "capture_admission")
        .map(|row| row.generation)
        .collect::<Vec<_>>();
    assert_eq!(
        x86_capture_generations, x86_control_capture_generations,
        "x86 native/control capture IDs diverged"
    );
    let x86_restore_generations = x86_native
        .iter()
        .filter(|row| row.phase == "restore_validation")
        .map(|row| row.generation)
        .collect::<Vec<_>>();
    let x86_control_restore_generations = x86_control
        .iter()
        .filter(|row| row.phase == "recovery_wait")
        .map(|row| row.generation)
        .collect::<Vec<_>>();
    assert_eq!(
        x86_restore_generations, x86_control_restore_generations,
        "x86 native/control restore IDs diverged"
    );

    let completion = control
        .iter()
        .filter(|row| row.phase == "completion_wait")
        .map(|row| row.duration_us)
        .collect::<Vec<_>>();
    let settlement = native
        .iter()
        .filter(|row| row.phase == "settlement")
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(completion.len(), 2, "capture completion phases:\n{stderr}");
    assert_eq!(settlement.len(), 2, "capture settlement phases:\n{stderr}");
    for (completion, settlement) in completion.into_iter().zip(settlement) {
        assert!(
            completion >= settlement.duration_us,
            "settlement escaped capture interval:\n{stderr}"
        );
        assert!(
            completion - settlement.duration_us < 1_000_000,
            "non-settlement capture work regressed past one second: completion={completion} settlement={settlement:?}"
        );
        if settlement.budget_us != 0 {
            assert!(
                settlement.duration_us * 100 / completion >= 70,
                "the known fixed wait no longer dominates capture; investigate before changing the heuristic: {stderr}"
            );
        } else {
            assert!(
                completion < 1_000_000,
                "exact-topology capture regressed past one second: {completion} us"
            );
        }
    }

    let x86_completion = x86_control
        .iter()
        .filter(|row| row.phase == "completion_wait")
        .map(|row| row.duration_us)
        .collect::<Vec<_>>();
    let x86_settlement = x86_native
        .iter()
        .filter(|row| row.phase == "settlement")
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(x86_completion.len(), 2, "x86 capture completion phases:\n{stderr}");
    assert_eq!(x86_settlement.len(), 2, "x86 settlement phases:\n{stderr}");
    for (completion, settlement) in x86_completion.into_iter().zip(x86_settlement) {
        assert!(completion >= settlement.duration_us);
        assert!(completion - settlement.duration_us < 1_000_000);
        if settlement.budget_us == 0 {
            assert!(completion < 1_000_000);
        } else {
            assert!(settlement.duration_us * 100 / completion >= 70);
        }
    }

    for isa in ["aarch64", "x86_64"] {
        for phase in [
            "restore1_start_ready",
            "restore2_start_ready",
            "capture_wait_reap",
            "recapture_wait_reap",
        ] {
            let durations = outer_phase_us(&stderr, isa, phase);
            assert_eq!(durations.len(), 1, "missing outer phase {isa}/{phase}:\n{stderr}");
            assert!(
                durations[0] < 1_000_000,
                "{isa}/{phase} regressed past one second: {} us",
                durations[0]
            );
        }
    }
}

#[test]
fn socket_half_close_refuses_checkpoint_after_guest_dup_and_fork_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, socket_half_close_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    let mut accepted = Vec::new();

    for (isa, executable) in executables {
        let clean_directory = tempfile::tempdir().unwrap();
        let clean_ready = clean_directory.path().join("ready");
        let clean_store = Arc::new(Store::default());
        let clean = Arc::new(
            Engine::with_checkpoint(
                isa,
                socket_half_close_plan(&executable, &clean_ready, "clean"),
                streams(false),
                clean_store.clone(),
                clean_store.clone(),
            )
            .unwrap(),
        );
        clean.start().unwrap();
        wait_for(&clean_ready, "READY clean");
        clean
            .capture_checkpoint_until(checkpoint_deadline())
            .unwrap_or_else(|error| panic!("{isa:?} rejected the clean socketpair control: {error:?}"));
        assert_eq!(wait_bounded(&clean, "clean socketpair capture").guest_status, 0);
        assert!(clean_store.0.lock().unwrap().contains_key("MANIFEST"));

        for mode in ["dup", "fork"] {
            let directory = tempfile::tempdir().unwrap();
            let ready = directory.path().join("ready");
            let store = Arc::new(Store::default());
            let capture = Arc::new(
                Engine::with_checkpoint(
                    isa,
                    socket_half_close_plan(&executable, &ready, mode),
                    streams(false),
                    store.clone(),
                    store.clone(),
                )
                .unwrap(),
            );
            capture.start().unwrap();
            wait_for(&ready, &format!("READY {mode}"));
            let rejected = capture.capture_checkpoint_until(checkpoint_deadline()).is_err();
            let _ = wait_result_bounded(&capture, "half-closed socket checkpoint refusal");
            let snapshot = store.0.lock().unwrap();
            if rejected {
                assert!(
                    !snapshot.contains_key("MANIFEST"),
                    "{isa:?}/{mode} committed a rejected generation"
                );
                assert!(
                    snapshot.keys().all(|name| !name.starts_with("socket.")),
                    "{isa:?}/{mode} published a socket queue before admission refusal: {:?}",
                    snapshot.keys().collect::<Vec<_>>()
                );
            } else {
                accepted.push(format!("{isa:?}/{mode}"));
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "accepted half-closed guest socket paths: {accepted:?}"
    );
}

#[test]
fn accepted_connected_unix_stream_survives_checkpoint_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, connected_unix_stream_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    for (isa, executable) in executables {
        let temporary = tempfile::tempdir().unwrap();
        let ready = temporary.path().join("ready");
        let finish = temporary.path().join("finish");
        let guard = temporary.path().join("fresh-start.guard");
        let fallback = temporary.path().join("fresh-start.fallback");
        let store = Arc::new(Store::default());
        let capture = Arc::new(
            Engine::with_checkpoint(
                isa,
                connected_unix_stream_plan(&executable, &ready, &finish, &guard, &fallback, false),
                streams(false),
                store.clone(),
                store.clone(),
            )
            .unwrap(),
        );
        capture.start().unwrap();
        wait_for(&ready, "READY fd=10 connected=1");
        capture
            .capture_checkpoint_until(checkpoint_deadline())
            .unwrap_or_else(|error| panic!("{isa:?} rejected accepted connected AF_UNIX fd 10: {error:?}"));
        assert_eq!(wait_bounded(&capture, "connected Unix stream capture").guest_status, 0);

        let restore = Arc::new(
            Engine::with_checkpoint(
                isa,
                connected_unix_stream_plan(&executable, &ready, &finish, &guard, &fallback, true),
                streams(false),
                store.clone(),
                store,
            )
            .unwrap(),
        );
        restore.start().unwrap();
        std::fs::write(&finish, []).unwrap();
        wait_for_connected_restore(&restore, isa, &ready, &fallback);
        assert_eq!(wait_bounded(&restore, "connected Unix stream restore").guest_status, 0);
    }
}

#[test]
fn shared_memory_futex_and_pshared_sync_survive_two_checkpoint_cycles_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let cases = [
        ("shared_alias", "DONE shared-alias-ok"),
        ("shared_futex", "DONE shared-futex-ok"),
        ("pshared_cond", "DONE pshared-cond-ok"),
    ];
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64]
        .into_iter()
        .flat_map(|isa| {
            cases.map(|(source, expected)| (isa, shared_state_fixture(isa, fixtures.path(), source), expected))
        })
        .collect::<Vec<_>>();
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable, expected) in executables {
        shared_state_round_trip(isa, &executable, expected);
    }
}

#[test]
fn arm64_cross_process_shared_futex_survives_two_checkpoint_cycles() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executable = shared_state_fixture(GuestIsa::Aarch64, fixtures.path(), "shared_futex");
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    shared_state_round_trip(GuestIsa::Aarch64, &executable, "DONE shared-futex-ok");
}

#[test]
fn externally_mutated_access_results_remain_coherent_across_restore_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64]
        .map(|isa| (isa, shared_state_fixture(isa, fixtures.path(), "external_access")));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();
    for (isa, executable) in executables {
        external_access_round_trip(isa, &executable);
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
        capture
            .capture_checkpoint_until(Instant::now() + Duration::from_secs(10))
            .unwrap();
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
            capture
                .capture_checkpoint_until(Instant::now() + Duration::from_secs(10))
                .unwrap();
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
                assert!(
                    output.contains("rem=73.000000041"),
                    "{isa:?} {mode} mutated remainder: {output}"
                );
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

fn nested_pipeline_round_trip(isa: GuestIsa) {
    let fixtures = tempfile::tempdir().unwrap();
    let executable = nested_pipeline_fixture(isa, fixtures.path());
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
    capture_port.wait_output(b"NESTED-MEMBER-ALIVE");
    capture_port.wait_output(b"NESTED-LEADER-ALIVE");
    capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
    assert_eq!(capture.wait().unwrap().guest_status, 0);

    let restore_port = Arc::new(TestTerminal::default());
    let restore = Arc::new(
        Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE"]),
            StandardStreams::default().with_terminal(Terminal::new(restore_port.clone(), 24, 80).unwrap()),
            store.clone(),
            store,
        )
        .unwrap(),
    );
    restore.start().unwrap();
    let (finished, completion) = std::sync::mpsc::channel();
    let waiting = restore.clone();
    std::thread::spawn(move || finished.send(waiting.wait()).unwrap());
    restore_port.wait_output(b"NESTED-MEMBER-ALIVE");
    restore_port.wait_output(b"NESTED-LEADER-ALIVE");
    assert!(matches!(
        completion.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    restore_port.input(&[3]);
    let restored = completion
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("{isa:?} nested pipeline did not exit:\n{}", restore_port.output()))
        .unwrap_or_else(|error| panic!("{isa:?} nested pipeline failed: {error:?}\n{}", restore_port.output()));
    assert_eq!(restored.guest_status, 0, "{isa:?}: {}", restore_port.output());
    restore_port.wait_output(b"NESTED-SHELL-SURVIVED");
    restore_port.wait_output(b"NESTED-INIT-SURVIVED");
}

#[test]
fn nested_pipeline_foreground_restores_on_aarch64() {
    nested_pipeline_round_trip(GuestIsa::Aarch64);
}

#[test]
fn nested_pipeline_foreground_restores_on_x86_64() {
    nested_pipeline_round_trip(GuestIsa::X86_64);
}

#[test]
fn reaped_group_leader_keeps_typed_identity_fail_closed_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, reaped_group_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    for (isa, executable) in executables {
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
        capture_port.wait_output(b"REAPED-GROUP-READY");
        capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(
            capture.wait().unwrap().guest_status,
            0,
            "{isa:?}: {}",
            capture_port.output()
        );

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
        // Release the background init directly. Terminal input would target the surviving foreground group,
        // while an init-side terminal read would be stopped with SIGTTIN and could never be a control channel.
        restore_port.wait_output(b"REAPED-GROUP-CONTROL-READY");
        let signal_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match restore.stop(StopRequest::Signal(libc::SIGUSR1)) {
                Ok(()) => break,
                Err(EngineError::NotStarted) if Instant::now() < signal_deadline => std::thread::yield_now(),
                Err(error) => panic!("{isa:?} could not release restored init: {error:?}"),
            }
        }
        let restored = restore.wait().unwrap_or_else(|error| {
            panic!(
                "{isa:?} reaped-group restore failed: {error:?}\n{}",
                restore_port.output()
            )
        });
        assert_eq!(restored.guest_status, 0, "{isa:?}: {}", restore_port.output());
    }
}

#[test]
fn identities_created_after_restore_survive_recapture_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, dynamic_identity_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    for (isa, executable) in executables {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let first = Arc::new(Store::default());
        let capture_port = Arc::new(TestTerminal::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_CHECKPOINT"]),
            StandardStreams::default().with_terminal(Terminal::new(capture_port.clone(), 24, 80).unwrap()),
            first.clone(),
            first.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        capture_port.wait_output(b"DYNAMIC-IDENTITY-READY");
        capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(capture.wait().unwrap().guest_status, 0);

        let second = Arc::new(Store::default());
        let recapture_port = Arc::new(TestTerminal::default());
        let recapture = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE", "HL_CHECKPOINT"]),
            StandardStreams::default().with_terminal(Terminal::new(recapture_port.clone(), 24, 80).unwrap()),
            second.clone(),
            first,
        )
        .unwrap();
        recapture.start().unwrap();
        recapture_port.input(b"x\n");
        recapture_port.wait_output(b"DYNAMIC-CHILDREN");
        recapture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(
            recapture.wait().unwrap().guest_status,
            0,
            "{isa:?}: {}",
            recapture_port.output()
        );
        let captured_processes = second
            .0
            .lock()
            .unwrap()
            .keys()
            .filter_map(|name| name.split_once('/').map(|(group, _)| group.to_owned()))
            .filter(|group| group.starts_with("proc."))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            captured_processes.len(),
            4,
            "{isa:?}: recapture omitted live descendants"
        );

        let restore_port = Arc::new(TestTerminal::default());
        let restore = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE"]),
            StandardStreams::default().with_terminal(Terminal::new(restore_port.clone(), 24, 80).unwrap()),
            second.clone(),
            second,
        )
        .unwrap();
        restore.start().unwrap();
        restore_port.input(b"x\n");
        let restored = restore.wait().unwrap_or_else(|error| {
            panic!(
                "{isa:?} dynamic identity restore failed: {error:?}\n{}",
                restore_port.output()
            )
        });
        assert_eq!(restored.guest_status, 0, "{isa:?}: {}", restore_port.output());
    }
}

#[test]
#[ignore = "long-running capacity and reclamation stress gate"]
fn restored_typed_identity_registry_reuses_capacity_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, identity_churn_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    for (isa, executable) in executables {
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
        capture_port.wait_output(b"IDENTITY-CHURN-READY");
        capture.capture_checkpoint_until(checkpoint_deadline()).unwrap();
        assert_eq!(capture.wait().unwrap().guest_status, 0);

        let restore_port = Arc::new(TestTerminal::default());
        let restore = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE"]),
            StandardStreams::default().with_terminal(Terminal::new(restore_port.clone(), 24, 80).unwrap()),
            store.clone(),
            store.clone(),
        )
        .unwrap();
        restore.start().unwrap();
        restore_port.input(b"x\n");
        let restored = restore
            .wait()
            .unwrap_or_else(|error| panic!("{isa:?} identity churn failed: {error:?}\n{}", restore_port.output()));
        assert_eq!(restored.guest_status, 0, "{isa:?}: {}", restore_port.output());
        restore_port.wait_output(b"IDENTITY-CHURN-COMPLETE");
    }
}

#[test]
fn terminal_claim_mask_failure_aborts_before_any_restored_process_resumes() {
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
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
        wait_for_exact_process_reap(&executable);

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
        let restore_outcome = restore.wait();
        assert!(
            restore_outcome.is_err(),
            "injected terminal-claim mask failure did not fail the launch: {restore_outcome:?}"
        );
        wait_for_exact_process_reap(&executable);
        assert!(
            !restore_port.output().contains("CHILD-ALIVE"),
            "a descendant resumed after atomic terminal-claim failure:\n{}",
            restore_port.output()
        );
    }
}

#[test]
fn stale_foreground_guest_pid_is_rejected_without_resuming_on_both_isas() {
    let fixtures = tempfile::tempdir().unwrap();
    for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
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
        wait_for_exact_process_reap(&executable);
        set_manifest_foreground_group(&store, i32::MAX - 17);

        let restore_port = Arc::new(TestTerminal::default());
        let restore = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE"]),
            StandardStreams::default().with_terminal(Terminal::new(restore_port.clone(), 24, 80).unwrap()),
            store.clone(),
            store,
        )
        .unwrap();
        restore.start().unwrap();
        assert!(
            restore.wait().is_err(),
            "stale foreground gpid restore unexpectedly succeeded"
        );
        wait_for_exact_process_reap(&executable);
        assert!(
            !restore_port.output().contains("CHILD-ALIVE"),
            "a stale foreground gpid released a descendant:\n{}",
            restore_port.output()
        );
    }
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

fn unwaited_child_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-unwaited-child-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-unwaited-child-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/unwaited_child.c");
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

/// A child exit status the capture's own reap destroys survives into the restored tree.
///
/// The coordinator reaps with `waitpid(-1, WNOHANG)` from inside the container init, which IS a guest
/// process, so a guest zombie it collects is a pending child status DELETED -- there is no engine-side
/// table behind it, the kernel corpse is the state. The observed production failure was not a refusal but
/// a workspace that reopened and never came back to life: `checkpoint OK: 4 process(es)` where healthy
/// cycles publish 5, then a restored shell resuming into `wait4` for a pid that no longer existed, 725 s
/// of `State: S`, `wchan=do_wait`, zero CPU, and nothing logged by any component.
///
/// The fixture holds that exact state deliberately (see `unwaited_child.c`), and the assertion is the
/// GUEST'S: after the restore it must reap its own child, by the pid `fork` gave it, with the status the
/// child exited with. Without the preservation the restored parent has no such child at all and the
/// fixture exits 70 -- so this cannot pass by the old and new code answering the same value.
///
/// The image assertion is the other half. `proc.1/reaped` must be present after the capture: a fix that
/// made the restore tolerant of the missing status -- returning ECHILD promptly instead of hanging -- would
/// still be a capture that reported success with a member's state unsaved, and it would fail here.
#[test]
fn a_child_status_the_capture_reaped_is_returned_to_its_parent_after_restore_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, unwaited_child_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    for (isa, executable) in executables {
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
        capture_port.wait_output(b"UNWAITED-CHILD-ZOMBIE");
        capture
            .capture_checkpoint_until(checkpoint_deadline())
            .unwrap_or_else(|error| {
                panic!(
                    "{isa:?} refused a capture over an uncollected child status: {error:?}: {}",
                    capture_port.output()
                )
            });
        assert_eq!(
            capture.wait().unwrap().guest_status,
            0,
            "{isa:?}: {}",
            capture_port.output()
        );
        {
            let stored = store.0.lock().unwrap();
            assert!(stored.contains_key("MANIFEST"), "{isa:?} published no manifest");
            assert!(
                stored.contains_key("proc.1/reaped"),
                "{isa:?} published a checkpoint that reports success while the child exit status its own \
                 reap destroyed is in no image object: {:?}",
                stored.keys().collect::<Vec<_>>()
            );
        }

        let restore_port = Arc::new(TestTerminal::default());
        let restore = Engine::with_checkpoint(
            isa,
            plan(&executable, &release, &final_release, &["HL_RESTORE"]),
            StandardStreams::default().with_terminal(Terminal::new(restore_port.clone(), 24, 80).unwrap()),
            store.clone(),
            store.clone(),
        )
        .unwrap();
        restore.start().unwrap();
        restore_port.input(b"\n");
        let restored = restore
            .wait()
            .unwrap_or_else(|error| panic!("{isa:?} restore failed: {error:?}: {}", restore_port.output()));
        assert_eq!(
            restored.guest_status,
            0,
            "{isa:?} restored parent did not reap its own child with the captured status: {}",
            restore_port.output()
        );
        assert!(
            restore_port.output().contains("UNWAITED-CHILD-REAPED"),
            "{isa:?}: {}",
            restore_port.output()
        );
    }
}

/// The other half, and the one that stops the fix from being a best-effort: a capture that CANNOT preserve
/// a status it destroyed refuses, loudly, instead of publishing a manifest.
///
/// The status is gone by the time the coordinator notices -- `waitpid` released the corpse -- so there is
/// nothing left to recover; the only honest answer is to fail the close. A refusal is a workspace that will
/// not close, which is bad; a `checkpoint OK` over a parent that can never be woken is worse, because
/// nothing tells the user anything is wrong. `HL_CKPT_TEST_REAPED_UNNAMEABLE` drives the real refusal
/// through the real recording path, which no guest can reach on purpose (a fork's guest identity is
/// published before the child can exit).
#[test]
fn a_capture_that_cannot_preserve_a_status_it_destroyed_refuses_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables =
        [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, unwaited_child_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    for (isa, executable) in executables {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        let final_release = temporary.path().join("final-release");
        let store = Arc::new(Store::default());
        let port = Arc::new(TestTerminal::default());
        let capture = Engine::with_checkpoint(
            isa,
            plan(
                &executable,
                &release,
                &final_release,
                &["HL_CHECKPOINT", "HL_CKPT_TEST_REAPED_UNNAMEABLE"],
            ),
            StandardStreams::default().with_terminal(Terminal::new(port.clone(), 24, 80).unwrap()),
            store.clone(),
            store.clone(),
        )
        .unwrap();
        capture.start().unwrap();
        port.wait_output(b"UNWAITED-CHILD-ZOMBIE");
        let refusal = capture
            .capture_checkpoint_until(checkpoint_deadline())
            .expect_err("a capture that destroyed a child exit status it could not carry was reported as successful");
        let _ = capture.wait();
        let stored = store.0.lock().unwrap();
        assert!(
            !stored.contains_key("MANIFEST"),
            "{isa:?} published a manifest over a destroyed child exit status: {refusal:?}"
        );
    }
}

fn member_exit_fixture(isa: GuestIsa, directory: &Path) -> PathBuf {
    let (compiler, name) = match isa {
        GuestIsa::Aarch64 => ("aarch64-linux-gnu-gcc", "checkpoint-member-exit-aarch64"),
        GuestIsa::X86_64 => ("x86_64-linux-gnu-gcc", "checkpoint-member-exit-x86_64"),
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint/member_exit.c");
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

fn member_exit_plan(executable: &Path, mode: &str, directory: &Path, restore: bool) -> RuntimePlan {
    let mut options = Options::default();
    options
        .set(if restore { "HL_RESTORE" } else { "HL_CHECKPOINT" }, "1", true)
        .unwrap();
    RuntimePlan {
        rootfs: None,
        executable_host: Some(executable.as_os_str().as_encoded_bytes().to_vec()),
        arguments: [
            executable.as_os_str().as_encoded_bytes().to_vec(),
            mode.as_bytes().to_vec(),
            directory.as_os_str().as_encoded_bytes().to_vec(),
        ]
        .into(),
        environment: Vec::new(),
        result_path: None,
        options,
    }
}

/// Capture a parent and one parked child, restore them, release the child into `mode`, and read back
/// what the HOST was told about that member -- plus what the restored parent's own `waitpid` saw.
///
/// The member's record is read while the restored tree is still running, because that is when a host
/// needs it: the fixture's parent parks on `finish` until this function has its answer.
fn restored_member_outcome(
    isa: GuestIsa,
    executable: &Path,
    mode: &str,
) -> (Option<hl_engine::runtime::MemberExit>, String) {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    let ready = directory.join("ready");
    let parent_ready = directory.join("parent-ready");
    let release = directory.join("release");
    let result = directory.join("result");
    let member = directory.join("member");
    let finish = directory.join("finish");
    let store = Arc::new(Store::default());

    let capture = Engine::with_checkpoint(
        isa,
        member_exit_plan(executable, mode, directory, false),
        StandardStreams::default(),
        store.clone(),
        store.clone(),
    )
    .unwrap();
    capture.start().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !(ready.exists() && parent_ready.exists()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        ready.exists() && parent_ready.exists(),
        "{isa:?}/{mode} fixture never parked both processes"
    );
    // Both markers are published just BEFORE the syscall each process parks in, so the last few
    // instructions before the park are still ahead of them. Settle before freezing the tree.
    std::thread::sleep(Duration::from_millis(50));
    capture
        .capture_checkpoint_until(checkpoint_deadline())
        .unwrap_or_else(|error| panic!("{isa:?}/{mode} refused the capture: {error:?}"));
    assert_eq!(capture.wait().unwrap().guest_status, 0);

    let guest_pid = std::fs::read_to_string(&member)
        .unwrap_or_else(|error| panic!("{isa:?}/{mode} published no member pid: {error}"))
        .trim()
        .parse::<i32>()
        .unwrap_or_else(|error| panic!("{isa:?}/{mode} published an unreadable member pid: {error}"));
    let guest_pid = std::num::NonZeroI32::new(guest_pid).expect("a guest pid is never zero");

    let restore = Engine::with_checkpoint(
        isa,
        member_exit_plan(executable, mode, directory, true),
        StandardStreams::default(),
        store.clone(),
        store,
    )
    .unwrap();
    restore.start().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let handle = loop {
        if let Some(handle) = restore.restored_member(guest_pid) {
            break handle;
        }
        assert!(
            Instant::now() < deadline,
            "{isa:?}/{mode} restore never announced member {guest_pid}"
        );
        std::thread::sleep(Duration::from_millis(2));
    };

    std::fs::write(&release, []).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reported = handle.exit();
    while Instant::now() < deadline {
        reported = handle.exit();
        // `Unreported` is what a member gone WITHOUT a report reads as, so it is not an answer yet:
        // hold until a real record arrives or the deadline decides there is not going to be one.
        if reported.is_some_and(|exit| exit != hl_engine::runtime::MemberExit::Unreported) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // The parent writes its own reaping evidence only after the child is gone, so it is necessarily
    // later than the member report and needs its own wait rather than a read taken at the same instant.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut observed = String::new();
    while Instant::now() < deadline {
        observed = std::fs::read_to_string(&result).unwrap_or_default();
        if observed.ends_with('\n') {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    std::fs::write(&finish, []).unwrap();
    assert_eq!(restore.wait().unwrap().guest_status, 0);
    (reported, observed)
}

/// A restored member killed by a fatal guest signal reports the SIGNAL, not silence.
///
/// The report a restored member sends on its way out is the only thing that can tell the host how it
/// ended -- the member is not a child of the host, so nothing reaps it. That report used to be reachable
/// only from paths a fatal signal does not take: `guest_group_fatal` reaches the host `_exit` straight from
/// `linux_abi/signal.c`, so a member the engine WATCHED take a SIGSEGV vanished having said nothing, and
/// the host recorded `Unreported` -- which `hl-container` renders as `Fault { status: -1, reason: Unknown }`.
///
/// That is the same record a member killed before it could report anything gets, and correctly so. The
/// defect was that the two were indistinguishable: a session that crashed and a session that was destroyed
/// from outside produced identical evidence.
///
/// The assertion is on the record the host holds, read over the checkpoint channel from the live restored
/// tree, not on any engine-internal field. The parent's own `waitpid` reconstruction is asserted beside it
/// because the two are independent mechanisms -- the shared signal-exit relay and the member report -- and
/// a fix to either that did not fix the other would be visible here.
#[test]
fn a_restored_member_killed_by_a_guest_signal_reports_that_signal_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, member_exit_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    for (isa, executable) in executables {
        let (reported, observed) = restored_member_outcome(isa, &executable, "signal");
        assert_eq!(
            reported,
            Some(hl_engine::runtime::MemberExit::Signal(11)),
            "{isa:?} lost the signal that killed a restored member (the guest saw: {observed:?})"
        );
        assert_eq!(
            observed, "signaled=1 signo=11 exited=0 code=0\n",
            "{isa:?} restored parent did not reap a signalled child"
        );
    }
}

/// A restored member that exits cleanly reports its CODE, and is therefore never confused with the one
/// above. This is the other half of the same distinction and it exercises the other exit path entirely:
/// `exit_group(2)` reaches the host `_exit` from the syscall handler, and reports through the shared
/// process-scoped exit hook rather than through the signal path.
#[test]
fn a_restored_member_that_exits_cleanly_reports_its_code_on_both_isas() {
    let compiling = fixture_compilation();
    let fixtures = tempfile::tempdir().unwrap();
    let executables = [GuestIsa::Aarch64, GuestIsa::X86_64].map(|isa| (isa, member_exit_fixture(isa, fixtures.path())));
    drop(compiling);
    let _exclusive = exclusive_checkpoint_test();

    for (isa, executable) in executables {
        let (reported, observed) = restored_member_outcome(isa, &executable, "code");
        assert_eq!(
            reported,
            Some(hl_engine::runtime::MemberExit::Code(37)),
            "{isa:?} lost the status a restored member exited with (the guest saw: {observed:?})"
        );
        assert_eq!(
            observed, "signaled=0 signo=0 exited=1 code=37\n",
            "{isa:?} restored parent did not reap a cleanly exited child"
        );
    }
}
