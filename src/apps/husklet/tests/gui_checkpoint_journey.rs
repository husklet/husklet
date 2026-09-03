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
    let receipt = temporary.path().join("journey.receipt");
    let output = std::fs::File::create(temporary.path().join("husklet.out")).unwrap();
    let errors = std::fs::File::create(temporary.path().join("husklet.err")).unwrap();
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
        "journey_complete",
        Duration::from_secs(60),
    );
    let before = wait_text(&rootfs.join("tmp/husklet-gui-shell-before"), Duration::from_secs(10));
    let after = wait_text(&rootfs.join("tmp/husklet-gui-shell-after"), Duration::from_secs(10));
    assert_eq!(before, after, "reopen launched a different terminal shell");
    assert!(
        !rootfs.join("tmp/husklet-gui-fresh").exists(),
        "reopen ran the shell setup a second time"
    );
    let progress = rootfs.join("tmp/husklet-gui-progress");
    let first = std::fs::metadata(&progress).unwrap().len();
    std::thread::sleep(Duration::from_millis(250));
    let second = std::fs::metadata(&progress).unwrap().len();
    assert!(second > first, "restored child process did not resume progress");

    let events = std::fs::read_to_string(&receipt).unwrap();
    for required in [
        "close_requested",
        "dialog_continue_clicked",
        "domain_offline",
        "manager_reopen_clicked",
        "reopen_command_typed",
        "journey_complete",
    ] {
        assert!(
            events.lines().any(|line| line.ends_with(required)),
            "missing {required}:\n{events}"
        );
    }
    let close_ms = event_time(&events, "domain_offline") - event_time(&events, "dialog_continue_clicked");
    let reopen_ms = event_time(&events, "journey_complete") - event_time(&events, "manager_reopen_clicked");
    eprintln!("gui-checkpoint close_ms={close_ms} reopen_ms={reopen_ms} shell_pid={after}");
}

fn unpack(archive: &Path, destination: &Path) {
    std::fs::create_dir(destination).unwrap();
    let source = std::fs::File::open(archive).unwrap();
    tar::Archive::new(flate2::read::GzDecoder::new(source))
        .unpack(destination)
        .unwrap();
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

fn event_time(receipt: &str, event: &str) -> u128 {
    receipt
        .lines()
        .find(|line| line.ends_with(event))
        .and_then(|line| line.split_ascii_whitespace().next())
        .and_then(|field| field.strip_prefix("time_ms="))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("missing timestamp for {event}:\n{receipt}"))
}
