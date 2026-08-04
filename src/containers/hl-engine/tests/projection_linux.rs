#![cfg(target_os = "linux")]

use hl_engine::native::{
    AuthorityAccess, ChildExit, FileAction, HostDescriptor, LinuxHost, ProcessAuthority, ProcessGroup, ProcessHandle,
    SpawnRequest,
};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Bounded;

impl Bounded {
    fn worker(process: &ProcessHandle<LinuxHost>) -> (ChildExit, bool) {
        Self::worker_for(process, Duration::from_secs(5))
    }

    fn worker_for(process: &ProcessHandle<LinuxHost>, duration: Duration) -> (ChildExit, bool) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if let Some(exit) = process.wait().unwrap() {
                return (exit, false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        process.signal(hl_engine::native::ProcessSignal::Kill).unwrap();
        (process.wait_blocking().unwrap(), true)
    }

    fn authority(
        authority: &ProcessAuthority<LinuxHost>,
        token: u64,
    ) -> Result<(Option<ChildExit>, bool), hl_engine::engine::EngineError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(exit) = Self::authority_state(authority, token) {
                return exit;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        authority.cleanup(token).map(|exit| (exit, true))
    }

    fn authority_state(
        authority: &ProcessAuthority<LinuxHost>,
        token: u64,
    ) -> Option<Result<(Option<ChildExit>, bool), hl_engine::engine::EngineError>> {
        match authority.healthy(token) {
            Ok(true) => None,
            Ok(false) => Some(authority.reap(token).map(|exit| (Some(exit), false))),
            Err(_) => Some(match authority.cleanup(token) {
                Ok(value) => Ok((value, false)),
                Err(error) => Err(error),
            }),
        }
    }
}

#[test]
fn confined_projection() {
    let root = std::env::temp_dir().join(format!("hl-projection-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("projected");
    std::fs::write(&path, b"authority-projection").unwrap();
    let authority = ProcessAuthority::projected(
        std::path::Path::new(env!("CARGO_BIN_EXE_hl-authority-child")),
        &path,
        Arc::new(LinuxHost),
    )
    .unwrap();
    let channel = authority.open([3, 4]).unwrap();
    let descriptor = channel.descriptor();
    let health = channel.health();
    let request = SpawnRequest {
        program: CString::new(env!("CARGO_BIN_EXE_hl-projection-worker")).unwrap(),
        arguments: vec![
            CString::new("--authority-fd").unwrap(),
            CString::new(descriptor.get().to_string()).unwrap(),
            CString::new("--health-fd").unwrap(),
            CString::new(health.get().to_string()).unwrap(),
            CString::new("--replace").unwrap(),
            CString::new(path.as_os_str().as_bytes()).unwrap(),
        ],
        environment: Vec::new(),
        process_group: ProcessGroup::New,
        file_actions: vec![
            FileAction::Inherit(HostDescriptor::new(descriptor.get()).unwrap()),
            FileAction::Inherit(health),
        ],
    };
    let worker = ProcessHandle::spawn(Arc::new(LinuxHost), &request).unwrap();
    authority.commit(channel).unwrap();
    assert_eq!(worker.wait_blocking().unwrap(), ChildExit::Code(0));
    assert_eq!(authority.reap(channel.session()[0]).unwrap(), ChildExit::Code(0));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn confined_static_guest() {
    let root = std::env::temp_dir().join(format!("hl-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).unwrap();
    let path = root.join("guest");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/runtime/legacy/artifacts/full/process/x86_64/uname-boundary");
    std::fs::copy(fixture, &path).unwrap();
    let authority = ProcessAuthority::projected(
        std::path::Path::new(env!("CARGO_BIN_EXE_hl-authority-child")),
        &path,
        Arc::new(LinuxHost),
    )
    .unwrap();
    let channel = authority.open([5, 6]).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"replacement-not-elf").unwrap();
    let descriptor = channel.descriptor();
    let health = channel.health();
    let request = SpawnRequest {
        program: CString::new(env!("CARGO_BIN_EXE_hl-x86_64")).unwrap(),
        arguments: vec![
            CString::new("--guest-isa").unwrap(),
            CString::new("x86_64").unwrap(),
            CString::new("/guest/original").unwrap(),
        ],
        environment: vec![
            CString::new(format!("HL_AUTHORITY_FD={}", descriptor.get())).unwrap(),
            CString::new(format!("HL_AUTHORITY_HEALTH_FD={}", health.get())).unwrap(),
            CString::new("RUST_BACKTRACE=full").unwrap(),
        ],
        process_group: ProcessGroup::New,
        file_actions: vec![FileAction::Inherit(descriptor), FileAction::Inherit(health)],
    };
    let worker = ProcessHandle::spawn(Arc::new(LinuxHost), &request).unwrap();
    authority.commit(channel).unwrap();
    let (worker_exit, worker_timeout) = Bounded::worker(&worker);
    let (authority_exit, authority_timeout) = Bounded::authority(&authority, channel.session()[0])
        .unwrap_or_else(|error| panic!("authority cleanup failed: {error:?}"));
    std::fs::remove_dir_all(root).unwrap();
    assert!(!worker_timeout, "confined worker timed out: {worker_exit:?}");
    assert!(!authority_timeout, "authority timed out: {authority_exit:?}");
    assert_eq!(worker_exit, ChildExit::Code(0));
    assert!(matches!(authority_exit, None | Some(ChildExit::Code(0))));
}

#[test]
fn confined_projected_file() {
    let root = std::env::temp_dir().join(format!("hl-root-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).unwrap();
    let guest = root.join("guest");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/runtime/legacy/artifacts/full/filesystem/x86_64/projected-read");
    std::fs::copy(fixture, &guest).unwrap();
    std::fs::write(root.join("data"), b"original").unwrap();
    let authority = ProcessAuthority::projected_root(
        std::path::Path::new(env!("CARGO_BIN_EXE_hl-authority-child")),
        &root,
        &guest,
        Arc::new(LinuxHost),
    )
    .unwrap();
    let channel = authority.open([9, 10]).unwrap();
    std::fs::remove_file(&guest).unwrap();
    std::fs::write(&guest, b"replacement-not-elf").unwrap();
    let descriptor = channel.descriptor();
    let health = channel.health();
    let request = SpawnRequest {
        program: CString::new(env!("CARGO_BIN_EXE_hl-x86_64")).unwrap(),
        arguments: vec![
            CString::new("--guest-isa").unwrap(),
            CString::new("x86_64").unwrap(),
            CString::new("/guest").unwrap(),
        ],
        environment: vec![
            CString::new(format!("HL_AUTHORITY_FD={}", descriptor.get())).unwrap(),
            CString::new(format!("HL_AUTHORITY_HEALTH_FD={}", health.get())).unwrap(),
        ],
        process_group: ProcessGroup::New,
        file_actions: vec![FileAction::Inherit(descriptor), FileAction::Inherit(health)],
    };
    let worker = ProcessHandle::spawn(Arc::new(LinuxHost), &request).unwrap();
    authority.commit(channel).unwrap();
    let (worker_exit, worker_timeout) = Bounded::worker(&worker);
    let (authority_exit, authority_timeout) = Bounded::authority(&authority, channel.session()[0]).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(!worker_timeout, "projected worker timed out: {worker_exit:?}");
    assert!(!authority_timeout, "projected authority timed out: {authority_exit:?}");
    assert_eq!(worker_exit, ChildExit::Code(0));
    assert!(matches!(authority_exit, None | Some(ChildExit::Code(0))));
}

fn projected_directory(isa: &str, engine: &str) {
    let root = std::env::temp_dir().join(format!("hl-directory-{isa}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("tree/base/child")).unwrap();
    std::fs::write(root.join("tree/base/value"), b"v").unwrap();
    std::fs::write(root.join("tree/base/child/value"), b"v").unwrap();
    std::os::unix::fs::symlink("child", root.join("tree/base/relative")).unwrap();
    std::os::unix::fs::symlink("/tree/base/child", root.join("tree/base/absolute")).unwrap();
    let guest = root.join("guest");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../../tests/runtime/legacy/artifacts/full/filesystem/{isa}/projected-directory"
    ));
    std::fs::copy(fixture, &guest).unwrap();
    let authority = ProcessAuthority::projected_root(
        std::path::Path::new(env!("CARGO_BIN_EXE_hl-authority-child")),
        &root,
        &guest,
        Arc::new(LinuxHost),
    )
    .unwrap();
    let channel = authority.open([13, 14]).unwrap();
    std::fs::remove_file(&guest).unwrap();
    std::fs::write(&guest, b"replacement-not-elf").unwrap();
    let descriptor = channel.descriptor();
    let health = channel.health();
    let request = SpawnRequest {
        program: CString::new(engine).unwrap(),
        arguments: vec![
            CString::new("--guest-isa").unwrap(),
            CString::new(isa).unwrap(),
            CString::new("/guest").unwrap(),
        ],
        environment: vec![
            CString::new(format!("HL_AUTHORITY_FD={}", descriptor.get())).unwrap(),
            CString::new(format!("HL_AUTHORITY_HEALTH_FD={}", health.get())).unwrap(),
        ],
        process_group: ProcessGroup::New,
        file_actions: vec![FileAction::Inherit(descriptor), FileAction::Inherit(health)],
    };
    let worker = ProcessHandle::spawn(Arc::new(LinuxHost), &request).unwrap();
    authority.commit(channel).unwrap();
    let (worker_exit, worker_timeout) = Bounded::worker(&worker);
    let (authority_exit, authority_timeout) = Bounded::authority(&authority, channel.session()[0]).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(!worker_timeout, "projected directory worker timed out: {worker_exit:?}");
    assert!(
        !authority_timeout,
        "projected directory authority timed out: {authority_exit:?}"
    );
    assert_eq!(worker_exit, ChildExit::Code(0));
    assert!(matches!(authority_exit, None | Some(ChildExit::Code(0))));
}

fn projected_write(isa: &str, engine: &str) {
    let root = std::env::temp_dir().join(format!("hl-write-{isa}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let guest = root.join("guest");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../../tests/runtime/legacy/artifacts/full/filesystem/{isa}/projected-write"
    ));
    std::fs::copy(fixture, &guest).unwrap();
    let authority = ProcessAuthority::projected_root_writable(
        std::path::Path::new(env!("CARGO_BIN_EXE_hl-authority-child")),
        &root,
        &guest,
        Arc::new(LinuxHost),
    )
    .unwrap();
    let channel = authority.open([15, 16]).unwrap();
    let descriptor = channel.descriptor();
    let health = channel.health();
    let request = SpawnRequest {
        program: CString::new(engine).unwrap(),
        arguments: vec![
            CString::new("--guest-isa").unwrap(),
            CString::new(isa).unwrap(),
            CString::new("/guest").unwrap(),
        ],
        environment: vec![
            CString::new(format!("HL_AUTHORITY_FD={}", descriptor.get())).unwrap(),
            CString::new(format!("HL_AUTHORITY_HEALTH_FD={}", health.get())).unwrap(),
        ],
        process_group: ProcessGroup::New,
        file_actions: vec![FileAction::Inherit(descriptor), FileAction::Inherit(health)],
    };
    let worker = ProcessHandle::spawn(Arc::new(LinuxHost), &request).unwrap();
    authority.commit(channel).unwrap();
    let (worker_exit, worker_timeout) = Bounded::worker(&worker);
    let (authority_exit, authority_timeout) = Bounded::authority(&authority, channel.session()[0]).unwrap();
    assert!(!worker_timeout, "projected write worker timed out: {worker_exit:?}");
    assert!(
        !authority_timeout,
        "projected write authority timed out: {authority_exit:?}"
    );
    assert_eq!(worker_exit, ChildExit::Code(0));
    assert!(matches!(authority_exit, None | Some(ChildExit::Code(0))));
    assert_eq!(std::fs::read(root.join("created")).unwrap(), b"Zb");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn directory_x86() {
    projected_directory("x86_64", env!("CARGO_BIN_EXE_hl-x86_64"));
}

#[test]
fn directory_arm() {
    projected_directory("aarch64", env!("CARGO_BIN_EXE_hl-aarch64"));
}

#[test]
fn write_x86() {
    projected_write("x86_64", env!("CARGO_BIN_EXE_hl-x86_64"));
}

#[test]
fn write_arm() {
    projected_write("aarch64", env!("CARGO_BIN_EXE_hl-aarch64"));
}

#[test]
fn confined_dynamic_guest() {
    let root = std::env::temp_dir().join(format!("hl-dynamic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib64")).unwrap();
    std::fs::create_dir_all(root.join("lib/x86_64-linux-gnu")).unwrap();
    let artifacts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/runtime/legacy/artifacts");
    let guest = root.join("guest");
    std::fs::copy(artifacts.join("full/process/x86_64/nonpie-dladdr"), &guest).unwrap();
    std::fs::copy(
        artifacts.join("runtime/x86_64/lib64/ld-linux-x86-64.so.2"),
        root.join("lib64/ld-linux-x86-64.so.2"),
    )
    .unwrap();
    std::fs::copy(
        artifacts.join("runtime/x86_64/lib/x86_64-linux-gnu/libc.so.6"),
        root.join("lib/x86_64-linux-gnu/libc.so.6"),
    )
    .unwrap();
    let authority = ProcessAuthority::projected_root(
        std::path::Path::new(env!("CARGO_BIN_EXE_hl-authority-child")),
        &root,
        &guest,
        Arc::new(LinuxHost),
    )
    .unwrap();
    let channel = authority.open([11, 12]).unwrap();
    std::fs::remove_file(&guest).unwrap();
    std::fs::write(&guest, b"replacement-not-elf").unwrap();
    let descriptor = channel.descriptor();
    let health = channel.health();
    let request = SpawnRequest {
        program: CString::new(env!("CARGO_BIN_EXE_hl-x86_64")).unwrap(),
        arguments: vec![
            CString::new("--guest-isa").unwrap(),
            CString::new("x86_64").unwrap(),
            CString::new("/guest").unwrap(),
        ],
        environment: vec![
            CString::new(format!("HL_AUTHORITY_FD={}", descriptor.get())).unwrap(),
            CString::new(format!("HL_AUTHORITY_HEALTH_FD={}", health.get())).unwrap(),
        ],
        process_group: ProcessGroup::New,
        file_actions: vec![FileAction::Inherit(descriptor), FileAction::Inherit(health)],
    };
    let worker = ProcessHandle::spawn(Arc::new(LinuxHost), &request).unwrap();
    authority.commit(channel).unwrap();
    let (worker_exit, worker_timeout) = Bounded::worker(&worker);
    let (authority_exit, authority_timeout) = Bounded::authority(&authority, channel.session()[0]).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(!worker_timeout, "dynamic worker timed out: {worker_exit:?}");
    assert!(!authority_timeout, "dynamic authority timed out: {authority_exit:?}");
    assert_eq!(worker_exit, ChildExit::Code(0));
    assert!(matches!(authority_exit, None | Some(ChildExit::Code(0))));
}

#[test]
fn confined_dynamic_arm() {
    let root = std::env::temp_dir().join(format!("hl-dynamic-arm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib/aarch64-linux-gnu")).unwrap();
    let artifacts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/runtime/legacy/artifacts");
    let guest = root.join("guest");
    std::fs::copy(artifacts.join("full/process/aarch64/nonpie-dladdr"), &guest).unwrap();
    std::fs::copy(
        artifacts.join("runtime/aarch64/lib/ld-linux-aarch64.so.1"),
        root.join("lib/ld-linux-aarch64.so.1"),
    )
    .unwrap();
    std::fs::copy(
        artifacts.join("runtime/aarch64/lib/aarch64-linux-gnu/libc.so.6"),
        root.join("lib/aarch64-linux-gnu/libc.so.6"),
    )
    .unwrap();
    let authority = ProcessAuthority::projected_root(
        std::path::Path::new(env!("CARGO_BIN_EXE_hl-authority-child")),
        &root,
        &guest,
        Arc::new(LinuxHost),
    )
    .unwrap();
    let channel = authority.open([13, 14]).unwrap();
    std::fs::remove_file(&guest).unwrap();
    std::fs::write(&guest, b"replacement-not-elf").unwrap();
    let descriptor = channel.descriptor();
    let health = channel.health();
    let request = SpawnRequest {
        program: CString::new(env!("CARGO_BIN_EXE_hl-aarch64")).unwrap(),
        arguments: vec![
            CString::new("--guest-isa").unwrap(),
            CString::new("aarch64").unwrap(),
            CString::new("/guest").unwrap(),
        ],
        environment: vec![
            CString::new(format!("HL_AUTHORITY_FD={}", descriptor.get())).unwrap(),
            CString::new(format!("HL_AUTHORITY_HEALTH_FD={}", health.get())).unwrap(),
        ],
        process_group: ProcessGroup::New,
        file_actions: vec![FileAction::Inherit(descriptor), FileAction::Inherit(health)],
    };
    let worker = ProcessHandle::spawn(Arc::new(LinuxHost), &request).unwrap();
    authority.commit(channel).unwrap();
    let (worker_exit, worker_timeout) = Bounded::worker(&worker);
    let (authority_exit, authority_timeout) = Bounded::authority(&authority, channel.session()[0]).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(!worker_timeout, "dynamic arm worker timed out: {worker_exit:?}");
    assert!(
        !authority_timeout,
        "dynamic arm authority timed out: {authority_exit:?}"
    );
    assert_eq!(worker_exit, ChildExit::Code(0));
    assert!(matches!(authority_exit, None | Some(ChildExit::Code(0))));
}

#[test]
fn timeout_cleanup() {
    let request = SpawnRequest {
        program: CString::new("/bin/sleep").unwrap(),
        arguments: vec![CString::new("30").unwrap()],
        environment: Vec::new(),
        process_group: ProcessGroup::New,
        file_actions: Vec::new(),
    };
    let worker = ProcessHandle::spawn(Arc::new(LinuxHost), &request).unwrap();
    let (exit, timed_out) = Bounded::worker_for(&worker, Duration::from_millis(50));
    assert!(timed_out);
    assert!(matches!(exit, ChildExit::Signal(_)));
    assert!(matches!(worker.wait(), Err(_) | Ok(Some(_))));
}
