#[path = "workspace_guest.rs"]
mod guest;

use hl_engine::activation::GuestIsa;
use hl_engine::runtime::{Builder, Input, Rootfs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// A file that is not an executable image, so the guest launch always fails.
fn unlaunchable(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hl-workspace-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("guest");
    std::fs::write(&path, b"not-an-elf").unwrap();
    path
}

#[test]
fn failed_launch_cleanup() {
    let guest = unlaunchable("cleanup");
    let engine = Builder::new(GuestIsa::X86_64, &guest)
        .with_input(Input::File {
            source: guest.clone(),
            relative: PathBuf::from("fixtures/status.out"),
            executable: false,
        })
        .build()
        .unwrap();
    let workspace = engine.workspace().to_owned();
    assert!(workspace.join("fixtures/status.out").is_file());
    engine.start().unwrap();
    assert!(engine.wait().is_err());
    drop(engine);
    assert!(!workspace.exists());
    std::fs::remove_dir_all(guest.parent().unwrap()).unwrap();
}

#[test]
fn rejects_input_traversal() {
    let guest = unlaunchable("traversal");
    let result = Builder::new(GuestIsa::X86_64, &guest)
        .with_input(Input::File {
            source: guest.clone(),
            relative: PathBuf::from("../escape"),
            executable: false,
        })
        .build();
    assert!(result.is_err());
    std::fs::remove_dir_all(guest.parent().unwrap()).unwrap();
}

#[test]
fn rejects_symlink_escape() {
    let guest = unlaunchable("symlink");
    let result = Builder::new(GuestIsa::X86_64, &guest)
        .with_input(Input::Symlink {
            relative: PathBuf::from("entry"),
            target: PathBuf::from("../guest"),
        })
        .build();
    assert!(result.is_err());
    std::fs::remove_dir_all(guest.parent().unwrap()).unwrap();
}

#[test]
fn rejects_input_collision() {
    let guest = unlaunchable("collision");
    let result = Builder::new(GuestIsa::X86_64, &guest)
        .with_input(Input::File {
            source: guest.clone(),
            relative: PathBuf::from("guest"),
            executable: true,
        })
        .build();
    assert!(result.is_err());
    std::fs::remove_dir_all(guest.parent().unwrap()).unwrap();
}

#[test]
fn rejects_rootfs_traversal() {
    let guest = unlaunchable("rootfs");
    let result = Builder::new(GuestIsa::X86_64, &guest)
        .with_rootfs(Rootfs::scratch("../guest"))
        .build();
    assert!(result.is_err());
    std::fs::remove_dir_all(guest.parent().unwrap()).unwrap();
}

#[test]
fn socket_teardown_child() {
    let Ok(mode) = std::env::var("HL_SOCKET_TEARDOWN_CHILD") else {
        return;
    };
    let (isa, name) = match mode.as_str() {
        "aarch64" | "aarch64-isolation" => (GuestIsa::Aarch64, "aarch64"),
        "x86_64" | "x86_64-isolation" => (GuestIsa::X86_64, "x86_64"),
        _ => panic!("unknown teardown mode"),
    };
    let root = std::env::temp_dir().join(format!("hl-socket-stop-{}-{mode}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let guest = root.join("socket-stop");
    guest::socket_stop(name, &guest);
    let count = if mode.ends_with("isolation") { 2 } else { 1 };
    let mut engines = Vec::new();
    for _ in 0..count {
        let engine = Builder::new(isa, &guest)
            .with_argument("blocking-read")
            .build()
            .unwrap();
        engine.start().unwrap();
        engines.push(engine);
    }
    thread::sleep(Duration::from_millis(100));
    for (index, engine) in engines.into_iter().enumerate() {
        let workspace = engine.workspace().to_owned();
        engine.stop(hl_engine::engine::StopRequest::Force).unwrap();
        let exit = engine.wait().unwrap();
        assert_eq!(exit.kind, hl_engine::engine::ExitKind::Signal);
        assert_eq!(exit.guest_status, 9);
        assert!(!workspace.exists(), "socket workspace leaked");
        engine.destroy().unwrap();
        if count == 2 && index == 0 {
            // Give an incorrectly process-global cancellation enough time to
            // terminate the unrelated engine before its own stop request.
            thread::sleep(Duration::from_millis(50));
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn blocked_socket_teardown() {
    let executable = std::env::current_exe().unwrap();
    for mode in ["aarch64", "x86_64", "aarch64-isolation", "x86_64-isolation"] {
        run_teardown(&executable, mode);
    }
}

fn run_teardown(executable: &Path, mode: &str) {
    let mut child = Command::new(executable)
        .args(["--exact", "socket_teardown_child", "--nocapture"])
        .env("HL_SOCKET_TEARDOWN_CHILD", mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "socket teardown child failed: {mode}");
                return;
            }
            None if started.elapsed() < Duration::from_secs(30) => {
                thread::sleep(Duration::from_millis(5));
            }
            None => {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("socket teardown child timed out: {mode}");
            }
        }
    }
}
