#![cfg(target_os = "linux")]

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PRIMARY: &str = "cd /root; token=$(cat /proc/sys/kernel/random/uuid); \
rm -f /tmp/husklet-container-probe; mkfifo /tmp/husklet-container-probe; \
(while :; do IFS= read -r probe < /tmp/husklet-container-probe && printf '%s|%s\\n' \"$token\" \"$probe\" >> /tmp/husklet-container-progress; done) & child=$!; \
printf 'token=%s\\npid=%s\\nchild=%s\\ncwd=%s\\n' \"$token\" \"$$\" \"$child\" \"$PWD\" > /tmp/husklet-container-state; \
wait \"$child\"; exit 91";
const SECONDARY: &str = "cd /var; token=$(cat /proc/sys/kernel/random/uuid); \
rm -f /tmp/husklet-container-probe; mkfifo /tmp/husklet-container-probe; \
(while :; do IFS= read -r probe < /tmp/husklet-container-probe && printf '%s|%s\\n' \"$token\" \"$probe\" >> /tmp/husklet-container-progress; done) & child=$!; \
printf 'token=%s\\npid=%s\\nchild=%s\\ncwd=%s\\n' \"$token\" \"$$\" \"$child\" \"$PWD\" > /tmp/husklet-container-state; \
wait \"$child\"; exit 92";

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
    let retained = std::env::var_os("HL_GUI_CHECKPOINT_FIXTURE").map(std::path::PathBuf::from);
    if let Some(path) = &retained {
        std::fs::create_dir(path).unwrap();
    }
    let fixture = retained.as_deref().unwrap_or_else(|| temporary.path());
    let home = fixture.join("home");
    let cache = fixture.join("cache");
    let storage = fixture.join("workspace");
    let rootfs = fixture.join("rootfs");
    let secondary_rootfs = fixture.join("rootfs-secondary");
    std::fs::create_dir_all(&cache).unwrap();
    unpack(&Path::new(&archive), &rootfs);
    unpack(&Path::new(&archive), &secondary_rootfs);
    let seeded = hl::runtime::domain::test_workspace::seed(&home, &storage, &rootfs, hl_ws::Arch::Amd64, PRIMARY)
        .await
        .unwrap();
    let secondary = hl::runtime::domain::test_workspace::seed_additional(
        &storage,
        &secondary_rootfs,
        &seeded.workspace,
        "workspace-secondary",
        SECONDARY,
    )
    .await
    .unwrap();
    let evidence = std::env::var_os("HL_GUI_CHECKPOINT_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| fixture.to_owned());
    std::fs::create_dir_all(&evidence).unwrap();
    let receipt = evidence.join("journey.receipt");
    let initial_ready = std::path::PathBuf::from(format!("{}.initial-ready", receipt.display()));
    let cycle_ready = std::path::PathBuf::from(format!("{}.cycle1-ready", receipt.display()));
    for stale in [&receipt, &initial_ready, &cycle_ready, &evidence.join("result.receipt")] {
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
        "initial_topology_typed tabs=2 panes=3 selected=split focused=1 geometry=913x617",
        Duration::from_secs(60),
    );
    assert_eq!(
        container_inventory(&storage),
        vec!["workspace", secondary.name.as_str()]
    );
    let containers_before = container_states(&rootfs, &secondary.rootfs);
    assert_ne!(
        containers_before[0], containers_before[1],
        "containers did not carry distinct state"
    );
    let before = slot_state(&rootfs, "before");
    let background_before = background_state(&rootfs, "before");
    probe_containers(&rootfs, &secondary.rootfs, &containers_before, "before");
    std::fs::write(&initial_ready, b"ready\n").unwrap();

    wait_receipt(
        &mut journey.application,
        &receipt,
        "reopen_command_typed",
        Duration::from_secs(60),
    );
    let after = slot_state(&rootfs, "after-1");
    assert_eq!(before, after, "reopen lost per-pane shell state or cwd");
    assert_eq!(containers_before, container_states(&rootfs, &secondary.rootfs));
    assert_eq!(
        container_inventory(&storage),
        vec!["workspace", secondary.name.as_str()]
    );
    assert_eq!(background_before, background_state(&rootfs, "after-1"));
    probe_containers(&rootfs, &secondary.rootfs, &containers_before, "after-1");
    let events = std::fs::read_to_string(&receipt).unwrap();
    let restored_ready_ms = unix_millis() - event_time(&events, "manager_reopen_clicked");
    std::fs::write(&cycle_ready, b"ready\n").unwrap();

    wait_receipt(
        &mut journey.application,
        &receipt,
        "reopen_command_typed_cycle2",
        Duration::from_secs(60),
    );
    let after_second = slot_state(&rootfs, "after-2");
    assert_eq!(before, after_second, "second reopen lost per-pane shell state or cwd");
    assert_eq!(containers_before, container_states(&rootfs, &secondary.rootfs));
    assert_eq!(
        container_inventory(&storage),
        vec!["workspace", secondary.name.as_str()]
    );
    assert_eq!(background_before, background_state(&rootfs, "after-2"));
    probe_containers(&rootfs, &secondary.rootfs, &containers_before, "after-2");

    let events = std::fs::read_to_string(&receipt).unwrap();
    for required in [
        "close_requested",
        "dialog_continue_clicked",
        "domain_offline",
        "manager_reopen_clicked",
        "history_probe_tab_selected",
        "reopen_receipts_visible",
        "reopen_command_typed",
        "topology_restored tabs=2 panes=3 selected=split focused=1 geometry=913x617",
        "close_requested_cycle2",
        "dialog_continue_clicked_cycle2",
        "domain_offline_cycle2",
        "manager_reopen_clicked_cycle2",
        "history_probe_tab_selected_cycle2",
        "reopen_receipts_visible_cycle2",
        "reopen_command_typed_cycle2",
        "topology_restored tabs=2 panes=3 selected=split focused=1 geometry=913x617_cycle2",
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
            "close_ms={close_ms}\nrestored_ready_ms={restored_ready_ms}\nclose_second_ms={close_second_ms}\nrestored_second_ready_ms={restored_second_ready_ms}\ncontinuity={after_second:?}\ncontainer_continuity={containers_before:?}\ncontainers=2/2-running\nbackground=5/5-running-sleeps\ntabs=2\npanes=3\nselected=split\nfocused=1\ngeometry=913x617\ncycles=2\n"
        ),
    )
    .unwrap();
    eprintln!(
        "gui-checkpoint close_ms={close_ms} restored_ready_ms={restored_ready_ms} close_second_ms={close_second_ms} restored_second_ready_ms={restored_second_ready_ms} continuity={after_second:?}"
    );
}

fn container_states(primary: &Path, secondary: &Path) -> [String; 2] {
    [primary, secondary].map(|root| wait_text(&root.join("tmp/husklet-container-state"), Duration::from_secs(10)))
}

fn probe_containers(primary: &Path, secondary: &Path, states: &[String; 2], probe: &str) {
    for (root, state) in [primary, secondary].into_iter().zip(states) {
        let token = state
            .lines()
            .find_map(|line| line.strip_prefix("token="))
            .unwrap_or_else(|| panic!("container state carries no in-memory token: {state:?}"));
        let fifo = root.join("tmp/husklet-container-probe");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            use std::os::unix::fs::OpenOptionsExt as _;
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
            {
                Ok(mut file) => {
                    use std::io::Write as _;
                    writeln!(file, "{probe}").unwrap();
                    break;
                }
                Err(error)
                    if matches!(error.raw_os_error(), Some(libc::ENXIO) | Some(libc::ENOENT))
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!(
                    "could not reach restored container child through {}: {error}",
                    fifo.display()
                ),
            }
        }
        let expected = format!("{token}|{probe}");
        let progress = root.join("tmp/husklet-container-progress");
        loop {
            if std::fs::read_to_string(&progress).is_ok_and(|text| text.lines().any(|line| line == expected)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "restored container child did not acknowledge {probe:?} with its captured token {token:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn container_inventory(storage: &Path) -> Vec<String> {
    let directory = storage.join("containers/state/containers");
    let mut names = std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json"))
        .map(|entry| {
            let bytes = std::fs::read(entry.path()).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            value["container"]["spec"]["name"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names.len(),
        2,
        "workspace inventory duplicated or lost a container: {names:?}"
    );
    names
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
        if let Some(errors) = path
            .parent()
            .and_then(|parent| std::fs::read_to_string(parent.join("husklet.err")).ok())
            .filter(|errors| errors.contains("could not close workspace"))
        {
            panic!("production checkpoint failed before {event}:\n{errors}");
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

fn slot_state(rootfs: &Path, suffix: &str) -> Vec<String> {
    (0..3)
        .map(|slot| {
            wait_text(
                &rootfs.join(format!("tmp/husklet-gui-slot-{slot}-{suffix}")),
                Duration::from_secs(10),
            )
        })
        .collect()
}

fn background_state(rootfs: &Path, suffix: &str) -> Vec<String> {
    (0..3)
        .map(|slot| {
            wait_text(
                &rootfs.join(format!("tmp/husklet-gui-background-{slot}-{suffix}")),
                Duration::from_secs(10),
            )
        })
        .collect()
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
