#![cfg(target_os = "linux")]

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PRIMARY: &str = "while :; do sleep 1000; done";

struct RunningJourney {
    application: std::process::Child,
    domain: hl::runtime::domain::Domain,
}

impl Drop for RunningJourney {
    fn drop(&mut self) {
        let _ = self.domain.close_handover(hl::runtime::domain::Close::Kill, || Ok(()));
        let _ = self.application.kill();
        let _ = self.application.wait();
    }
}

#[tokio::test]
async fn production_gui_closes_through_continue_dialog_and_reopens_from_manager() {
    let Some(archive) = std::env::var_os("HL_ALPINE_ARCHIVE") else {
        assert!(
            std::env::var_os("HL_PRODUCT_CHECKPOINT_REQUIRED").is_none(),
            "GUI checkpoint journey requires HL_ALPINE_ARCHIVE"
        );
        eprintln!("gui-checkpoint skipped: HL_ALPINE_ARCHIVE is unavailable");
        return;
    };
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let cache = temporary.path().join("cache");
    let storage = temporary.path().join("workspace");
    let rootfs = temporary.path().join("rootfs");
    std::fs::create_dir_all(&cache).unwrap();
    unpack(&Path::new(&archive), &rootfs);
    let seeded = hl::runtime::domain::test_workspace::seed(&home, &storage, &rootfs, hl_ws::Arch::Amd64, PRIMARY)
        .await
        .unwrap();
    let evidence = std::env::var_os("HL_GUI_CHECKPOINT_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&evidence).unwrap();
    let receipt = evidence.join("journey.receipt");
    let cycle_ready = std::path::PathBuf::from(format!("{}.cycle1-ready", receipt.display()));
    for stale in [&receipt, &cycle_ready, &evidence.join("result.receipt")] {
        remove_if_exists(stale);
    }
    let output = std::fs::File::create(evidence.join("husklet.out")).unwrap();
    let errors = std::fs::File::create(evidence.join("husklet.err")).unwrap();
    let application = Command::new(env!("CARGO_BIN_EXE_husklet"))
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .env("GTK_A11Y", "none")
        .env("HL_TERM_VIEW", "terminal")
        .env("HL_TERM_WS", &seeded.workspace.name)
        .env("HL_APP_INSTANCE", format!("gui-checkpoint-{}", std::process::id()))
        .env("HL_GUI_CHECKPOINT_JOURNEY", &receipt)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errors))
        .spawn()
        .unwrap();
    let mut journey = RunningJourney {
        application,
        domain: hl::runtime::domain::Domain::in_root(&home.join(".hl"), &seeded.workspace),
    };

    wait_receipt(
        &mut journey.application,
        &receipt,
        "reopen_command_typed",
        Duration::from_secs(60),
    );
    let before = wait_text(
        &rootfs.join("tmp/husklet-gui-continuity-before"),
        Duration::from_secs(10),
    );
    let after = wait_text(
        &rootfs.join("tmp/husklet-gui-continuity-after-1"),
        Duration::from_secs(10),
    );
    assert_eq!(before, after, "reopen lost shell-local continuity state");
    let progress = rootfs.join("tmp/husklet-gui-progress");
    let first = std::fs::metadata(&progress).unwrap().len();
    wait_for_growth(&progress, first, Duration::from_secs(5));
    let events = std::fs::read_to_string(&receipt).unwrap();
    let restored_ready_ms = unix_millis() - event_time(&events, "manager_reopen_clicked");
    std::fs::write(&cycle_ready, b"ready\n").unwrap();

    wait_receipt(
        &mut journey.application,
        &receipt,
        "reopen_command_typed_cycle2",
        Duration::from_secs(60),
    );
    let after_second = wait_text(
        &rootfs.join("tmp/husklet-gui-continuity-after-2"),
        Duration::from_secs(10),
    );
    assert_eq!(before, after_second, "second reopen lost shell-local continuity state");
    let second_start = std::fs::metadata(&progress).unwrap().len();
    wait_for_growth(&progress, second_start, Duration::from_secs(5));

    let events = std::fs::read_to_string(&receipt).unwrap();
    for required in [
        "close_requested",
        "dialog_continue_clicked",
        "domain_offline",
        "manager_reopen_clicked",
        "reopen_command_typed",
        "close_requested_cycle2",
        "dialog_continue_clicked_cycle2",
        "domain_offline_cycle2",
        "manager_reopen_clicked_cycle2",
        "reopen_command_typed_cycle2",
        "journey_complete",
    ] {
        assert!(
            events.lines().any(|line| line.ends_with(required)),
            "missing {required}:\n{events}"
        );
    }
    let close_ms = event_time(&events, "domain_offline") - event_time(&events, "dialog_continue_clicked");
    let close_second_ms =
        event_time(&events, "domain_offline_cycle2") - event_time(&events, "dialog_continue_clicked_cycle2");
    let restored_second_ready_ms = unix_millis() - event_time(&events, "manager_reopen_clicked_cycle2");
    assert!(
        close_ms <= 5_000,
        "checkpoint close exceeded 5s acceptance bound: {close_ms}ms"
    );
    assert!(
        restored_ready_ms <= 5_000,
        "restored terminal exceeded 5s acceptance bound: {restored_ready_ms}ms"
    );
    assert!(
        close_second_ms <= 5_000,
        "second checkpoint close exceeded 5s acceptance bound: {close_second_ms}ms"
    );
    assert!(
        restored_second_ready_ms <= 5_000,
        "second restored terminal exceeded 5s acceptance bound: {restored_second_ready_ms}ms"
    );
    std::fs::write(
        evidence.join("result.receipt"),
        format!(
            "close_ms={close_ms}\nrestored_ready_ms={restored_ready_ms}\nclose_second_ms={close_second_ms}\nrestored_second_ready_ms={restored_second_ready_ms}\ncontinuity={after_second}\nbackground=progressing\ncycles=2\n"
        ),
    )
    .unwrap();
    eprintln!(
        "gui-checkpoint close_ms={close_ms} restored_ready_ms={restored_ready_ms} close_second_ms={close_second_ms} restored_second_ready_ms={restored_second_ready_ms} continuity={after_second}"
    );
}

fn unpack(archive: &Path, destination: &Path) {
    std::fs::create_dir(destination).unwrap();
    let source = std::fs::File::open(archive).unwrap();
    tar::Archive::new(flate2::read::GzDecoder::new(source))
        .unpack(destination)
        .unwrap();
}

fn remove_if_exists(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove stale GUI journey evidence {}: {error}", path.display()),
    }
}

fn wait_receipt(child: &mut std::process::Child, path: &Path, event: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if std::fs::read_to_string(path).is_ok_and(|text| text.lines().any(|line| line.ends_with(event))) {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let receipt = std::fs::read_to_string(path).unwrap_or_else(|error| format!("<unreadable: {error}>"));
            let errors = path
                .parent()
                .and_then(|parent| std::fs::read_to_string(parent.join("husklet.err")).ok())
                .unwrap_or_else(|| "<unreadable>".to_owned());
            panic!("production husklet exited before {event}: {status}; receipt:\n{receipt}\nstderr:\n{errors}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {event}; receipt:\n{}",
            std::fs::read_to_string(path).unwrap_or_else(|error| format!("<unreadable: {error}>"))
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_text(path: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(mut file) = std::fs::File::open(path) {
            let mut value = String::new();
            file.read_to_string(&mut value).unwrap();
            if !value.trim().is_empty() {
                return value.trim().to_owned();
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {}", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_growth(path: &Path, initial: u64, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > initial) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "restored child process did not resume progress"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn event_time(receipt: &str, event: &str) -> u128 {
    receipt
        .lines()
        .find(|line| line.ends_with(event))
        .and_then(|line| line.split_ascii_whitespace().next())
        .and_then(|field| field.strip_prefix("time_ms="))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("missing timestamp for {event}:\n{receipt}"))
}
