#![cfg(target_os = "linux")]

use hl_engine::native::{AuthorityAccess, ChildExit, LinuxHost, ProcessAuthority};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hl_checkpoint::{ImageLimits, MemorySink, MemorySource, Section};
use hl_descriptor::{DescriptorFlags, ObjectKind, StatusFlags};
use hl_engine::runtime::CheckpointRuntime;
use hl_network::{
    AcceptedSocketCheckpoint, AddressFamily, NetworkPolicy, NetworkResourceKey, ShutdownState, SocketAddress, SocketId,
    SocketProtocol, SocketSnapshot, SocketState, SocketType,
};
use hl_runtime::{
    CheckpointParticipant, CheckpointRole, DescriptorCheckpointParticipant, DescriptorObjectCatalog, RuntimeAssembly,
    RuntimeAssemblyConfig, RuntimeCheckpointCoordinator,
};

struct Death<'a> {
    authority: &'a ProcessAuthority<LinuxHost>,
    token: u64,
    failed: &'a AtomicUsize,
}

impl Death<'_> {
    fn observe(&self, worker: &hl_engine::native::AuthorityWorker) {
        let done = std::sync::atomic::AtomicBool::new(false);
        let failure = || {
            self.failed.fetch_add(1, Ordering::SeqCst);
        };
        std::thread::scope(|scope| {
            let health = worker.health().unwrap();
            let monitor = scope.spawn(move || health.monitor(&done, failure));
            assert!(matches!(
                self.authority.terminate(self.token).unwrap(),
                ChildExit::Signal(_)
            ));
            assert!(monitor.join().unwrap().is_err());
        });
    }
}

fn authority() -> ProcessAuthority<LinuxHost> {
    ProcessAuthority::new(Path::new(env!("CARGO_BIN_EXE_hl-authority-child")), Arc::new(LinuxHost)).unwrap()
}

fn network_policy() -> NetworkPolicy {
    NetworkPolicy::from_launch(false, b"", b"", b"").expect("default network policy")
}

fn listener_closed(address: std::net::SocketAddr) {
    // Reaping closes the last listener owner, but TCP can finish a handshake
    // before the asynchronous reset is observed by this nonblocking client.
    // A leaked listener keeps the connected stream live through the deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(25)) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                return;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => panic!("unexpected post-authority connect error: {error}"),
            Ok(stream) => {
                stream.set_nonblocking(true).unwrap();
                let mut byte = [0];
                loop {
                    match stream.peek(&mut byte) {
                        Ok(0) => return,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                                    | std::io::ErrorKind::NotConnected
                            ) =>
                        {
                            return;
                        }
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => panic!("listener remained reachable after authority reap: {error}"),
                        Ok(_) => panic!("unexpected data from listener after authority reap"),
                    }
                }
            }
        }
    }
}

#[test]
fn process_handshake() {
    let authority = authority();
    let channel = authority.open([1, 2]).unwrap();
    let token = channel.session()[0];
    let mut worker = authority.worker(channel).unwrap();
    let entries = AtomicUsize::new(0);
    worker.enter(|| entries.fetch_add(1, Ordering::SeqCst)).unwrap();
    assert_eq!(entries.load(Ordering::SeqCst), 1);
    assert_eq!(worker.ping(b"health").unwrap(), b"health");
    authority.commit(channel).unwrap();
    assert!(authority.healthy(token).unwrap());
    worker.close().unwrap();
    assert_eq!(authority.reap(token).unwrap(), ChildExit::Code(0));
    assert!(authority.healthy(token).is_err());
}

#[test]
fn process_death() {
    let authority = authority();
    let channel = authority.open([1, 2]).unwrap();
    let token = channel.session()[0];
    let mut worker = authority.worker(channel).unwrap();
    worker.enter(|| ()).unwrap();
    authority.commit(channel).unwrap();
    let failed = AtomicUsize::new(0);
    Death {
        authority: &authority,
        token,
        failed: &failed,
    }
    .observe(&worker);
    assert_eq!(failed.load(Ordering::SeqCst), 1);
}

#[test]
fn listener_roundtrip() {
    let authority = authority();
    let channel = authority.open([21, 22]).unwrap();
    let token = channel.session()[0];
    let mut worker = authority.worker(channel).unwrap();
    worker.enter(|| ()).unwrap();
    authority.commit(channel).unwrap();

    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    worker.network().capture_prepare().unwrap();
    let aborted = worker.network().retain_listener(listener.as_raw_fd(), 40).unwrap();
    worker.network().capture_abort().unwrap();
    worker.network().capture_prepare().unwrap();
    let key = worker.network().retain_listener(listener.as_raw_fd(), 41).unwrap();
    assert_eq!(key.slot(), aborted.slot());
    assert!(key.generation() > aborted.generation());
    let digest = [7_u8; 32];
    worker.network().capture_publish(digest).unwrap();
    worker.network().capture_finish().unwrap();
    drop(listener);

    assert!(worker.network().restore_begin(digest, 4097).is_err());
    worker.network().restore_begin([8; 32], 1).unwrap();
    assert!(worker.network().restore_stage(key).is_err());
    worker.network().restore_abort().unwrap();

    worker.network().restore_begin(digest, 1).unwrap();
    assert!(worker.network().restore_stage(aborted).is_err());
    worker.network().restore_abort().unwrap();

    worker.network().restore_begin(digest, 1).unwrap();
    let staged = worker.network().restore_stage(key).unwrap();
    worker.network().restore_abort().unwrap();
    drop(staged);

    worker.network().restore_begin(digest, 1).unwrap();
    let staged = worker.network().restore_stage(key).unwrap();
    worker.network().restore_commit().unwrap();
    worker.network().restore_resume().unwrap();
    let descriptor: OwnedFd = staged.into();
    let restored = std::net::TcpListener::from(descriptor);
    let client = std::net::TcpStream::connect(address).unwrap();
    let (accepted, _) = restored.accept().unwrap();
    drop(client);
    drop(accepted);
    drop(restored);

    assert!(matches!(authority.terminate(token).unwrap(), ChildExit::Signal(_)));
    listener_closed(address);
}

struct EmptyCheckpoint(CheckpointRole);

impl CheckpointParticipant for EmptyCheckpoint {
    fn role(&self) -> CheckpointRole {
        self.0
    }
    fn version(&self) -> u32 {
        1
    }
    fn dependencies(&self) -> &[CheckpointRole] {
        &[]
    }
    fn freeze(&self) -> Result<(), ()> {
        Ok(())
    }
    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        Ok(Vec::new())
    }
    fn thaw(&self) -> Result<(), ()> {
        Ok(())
    }
    fn validate(&self, _: &hl_checkpoint::CheckpointImage, _: &Section) -> Result<(), ()> {
        Ok(())
    }
    fn stage(&self, _: &Section) -> Result<u64, ()> {
        Ok(1)
    }
    fn commit(&self, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn rollback(&self, _: u64) {}
    fn resume(&self, _: u64) -> Result<(), ()> {
        Ok(())
    }
}

fn listener_coordinator(assembly: &RuntimeAssembly, network: &CheckpointRuntime) -> RuntimeCheckpointCoordinator {
    let descriptor = Arc::new(DescriptorCheckpointParticipant::new(
        assembly.checkpoint_descriptors(),
        Arc::new(DescriptorObjectCatalog::rejecting().bind(ObjectKind::Socket, network.descriptor_binding())),
    ));
    RuntimeCheckpointCoordinator::new(
        vec![
            Arc::new(EmptyCheckpoint(CheckpointRole::Task)),
            descriptor,
            Arc::new(EmptyCheckpoint(CheckpointRole::Memory)),
            Arc::new(EmptyCheckpoint(CheckpointRole::Provider)),
            Arc::new(EmptyCheckpoint(CheckpointRole::Event)),
            network.participant(),
        ],
        ImageLimits::new(8, 1024 * 1024, 1024 * 1024),
    )
    .unwrap()
}

#[test]
fn checkpoint_listener() {
    let authority = authority();
    let channel = authority.open([31, 32]).unwrap();
    let token = channel.session()[0];
    let mut worker = authority.worker(channel).unwrap();
    worker.enter(|| ()).unwrap();
    authority.commit(channel).unwrap();
    let worker = Arc::new(std::sync::Mutex::new(worker));

    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let local = SocketAddress::Inet4 {
        address: std::net::Ipv4Addr::LOCALHOST.octets(),
        port: address.port(),
    };
    let snapshot = SocketSnapshot {
        id: SocketId { slot: 1, generation: 1 },
        family: AddressFamily::Inet4,
        socket_type: SocketType::Stream,
        protocol: SocketProtocol::Tcp,
        state: SocketState::Listening { backlog: 8 },
        local: Some(local.clone()),
        peer: None,
        connect_error: None,
        nonblocking: true,
        shutdown: ShutdownState::default(),
    };
    let status = StatusFlags::from_bits(StatusFlags::NONBLOCKING);

    let rejected_assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let rejected = CheckpointRuntime::new(
        rejected_assembly.checkpoint_network(),
        rejected_assembly.checkpoint_descriptors(),
        Some(Arc::clone(&worker)),
        network_policy(),
    );
    rejected_assembly
        .network()
        .insert_host(
            snapshot.clone(),
            NetworkResourceKey::new(90).unwrap(),
            Arc::new(()),
            vec![AcceptedSocketCheckpoint {
                resource: NetworkResourceKey::new(91).unwrap(),
                local: snapshot.local.clone().unwrap(),
                peer: SocketAddress::Inet4 {
                    address: std::net::Ipv4Addr::LOCALHOST.octets(),
                    port: 32000,
                },
            }],
        )
        .unwrap();
    let rejected_coordinator = listener_coordinator(&rejected_assembly, &rejected);
    assert!(rejected_coordinator.checkpoint(&mut MemorySink::new()).is_err());
    drop(rejected_coordinator);
    drop(rejected);
    drop(rejected_assembly);

    let first_assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let first = CheckpointRuntime::new(
        first_assembly.checkpoint_network(),
        first_assembly.checkpoint_descriptors(),
        Some(Arc::clone(&worker)),
        network_policy(),
    );
    assert!(
        first
            .adopt_listener(
                listener.try_clone().unwrap(),
                SocketSnapshot {
                    state: SocketState::Connected,
                    ..snapshot.clone()
                },
                status,
                DescriptorFlags::default(),
            )
            .is_err()
    );
    let descriptor = first
        .adopt_listener(listener, snapshot, status, DescriptorFlags::default())
        .unwrap();
    assert_eq!(first.listener_address(descriptor).unwrap(), local);
    let first_coordinator = listener_coordinator(&first_assembly, &first);
    let mut sink = MemorySink::new();
    first_coordinator.checkpoint(&mut sink).unwrap();
    let image = sink.committed().unwrap().to_vec();
    drop(first_coordinator);
    drop(first);
    drop(first_assembly);
    let authority_probe = std::net::TcpStream::connect(address).unwrap();
    drop(authority_probe);

    let second_assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let second = CheckpointRuntime::new(
        second_assembly.checkpoint_network(),
        second_assembly.checkpoint_descriptors(),
        Some(Arc::clone(&worker)),
        network_policy(),
    );
    let second_coordinator = listener_coordinator(&second_assembly, &second);
    second_coordinator.restore(&mut MemorySource::new(image)).unwrap();
    assert_eq!(second.listener_address(descriptor).unwrap(), local);
    let client = std::net::TcpStream::connect(address).unwrap();
    drop(client);
    drop(second_coordinator);
    drop(second);
    drop(second_assembly);
    assert!(matches!(authority.terminate(token).unwrap(), ChildExit::Signal(_)));
    drop(worker);
}

#[test]
fn secret_not_environment() {
    let authority = authority();
    let channel = authority.open([1, 2]).unwrap();
    assert!(std::env::vars_os().all(|(name, _)| !name.to_string_lossy().contains("AUTHORITY_SECRET")));
    authority.rollback(channel);
}

#[test]
fn endpoint_rejected() {
    let authority = ProcessAuthority::new(Path::new("/bin/true"), Arc::new(LinuxHost)).unwrap();
    let channel = authority.open([1, 2]).unwrap();
    let entries = AtomicUsize::new(0);
    let result = authority
        .worker(channel)
        .and_then(|mut worker| worker.enter(|| entries.fetch_add(1, Ordering::SeqCst)));
    assert!(result.is_err());
    assert_eq!(entries.load(Ordering::SeqCst), 0);
    authority.rollback(channel);
}

#[test]
fn health_idle_blocks() {
    let authority = authority();
    let channel = authority.open([1, 2]).unwrap();
    let mut worker = authority.worker(channel).unwrap();
    worker.enter(|| ()).unwrap();
    authority.commit(channel).unwrap();
    let health = worker.health().unwrap();
    let stop = worker.health().unwrap();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let failed = Arc::new(AtomicUsize::new(0));
    let monitor_done = Arc::clone(&done);
    let monitor_failed = Arc::clone(&failed);
    let monitor = std::thread::spawn(move || {
        health.monitor(&monitor_done, || {
            monitor_failed.fetch_add(1, Ordering::SeqCst);
        })
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(failed.load(Ordering::SeqCst), 0);
    done.store(true, Ordering::Release);
    stop.stop().unwrap();
    monitor.join().unwrap().unwrap();
    worker.close().unwrap();
    assert_eq!(authority.reap(channel.session()[0]).unwrap(), ChildExit::Code(0));
}

#[test]
fn rollback_releases_descriptors() {
    if !std::env::args_os().any(|argument| argument == "--exact") {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["rollback_releases_descriptors", "--exact", "--test-threads=1"])
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let count = || std::fs::read_dir("/proc/self/fd").unwrap().count();
    let before = count();
    {
        let authority = authority();
        for domain in 1..=16 {
            let channel = authority.open([domain, 2]).unwrap();
            authority.rollback(channel);
        }
    }
    assert!(count() <= before + 2);
}

#[test]
fn root_is_pinned() {
    let root = std::env::temp_dir().join(format!("hl-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).unwrap();
    let image = root.join("image");
    let data = root.join("data");
    std::fs::write(&image, b"image").unwrap();
    std::fs::write(&data, b"original").unwrap();
    std::os::unix::fs::symlink("data", root.join("link")).unwrap();
    let outside = root.with_file_name(format!("hl-tree-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"host-secret").unwrap();
    let relative_escape = format!("../{}/secret", outside.file_name().unwrap().to_string_lossy());
    std::os::unix::fs::symlink(relative_escape, root.join("relative-escape")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), root.join("absolute-escape")).unwrap();
    let authority = ProcessAuthority::projected_root(
        Path::new(env!("CARGO_BIN_EXE_hl-authority-child")),
        &root,
        &image,
        Arc::new(LinuxHost),
    )
    .unwrap();
    let channel = authority.open([7, 8]).unwrap();
    let mut worker = authority.worker(channel).unwrap();
    worker.enter(|| ()).unwrap();
    let handle = worker.tree_open(b"/data", false).unwrap();
    std::fs::remove_file(&data).unwrap();
    std::fs::write(&data, b"replacement").unwrap();
    assert_eq!(worker.tree_read(handle, 0, 32).unwrap(), b"original");
    assert_eq!(worker.tree_stat(handle).unwrap().size, 8);
    worker.tree_close(handle).unwrap();
    assert_eq!(
        worker.tree_stat(handle),
        Err(hl_engine::native::ProjectionError::Linux(libc::EBADF))
    );
    assert!(worker.tree_open(b"/../etc/passwd", false).is_err());
    let direct_escape = format!("/../../{}/secret", outside.file_name().unwrap().to_string_lossy());
    assert!(worker.tree_open(direct_escape.as_bytes(), false).is_err());
    assert!(worker.tree_open(b"/relative-escape", false).is_err());
    assert!(worker.tree_open(b"/absolute-escape", false).is_err());
    let link = worker.tree_open_link(b"/link").unwrap();
    assert_eq!(worker.tree_read_link(link, 32).unwrap(), b"data");
    worker.tree_close(link).unwrap();
    let directory = worker.tree_open(b"/", true).unwrap();
    let entries = worker.tree_entries(directory, 1024).unwrap();
    assert!(entries.windows(4).any(|value| value == b"data"));
    worker.tree_close(directory).unwrap();
    authority.commit(channel).unwrap();
    worker.close().unwrap();
    assert_eq!(authority.reap(channel.session()[0]).unwrap(), ChildExit::Code(0));
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn root_write_capability() {
    let root = std::env::temp_dir().join(format!("hl-tree-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).unwrap();
    let image = root.join("image");
    std::fs::write(&image, b"image").unwrap();
    let program = Path::new(env!("CARGO_BIN_EXE_hl-authority-child"));
    let read_only = ProcessAuthority::projected_root(program, &root, &image, Arc::new(LinuxHost)).unwrap();
    let channel = read_only.open([9, 10]).unwrap();
    let mut worker = read_only.worker(channel).unwrap();
    worker.enter(|| ()).unwrap();
    let options = hl_provider::TreeOpen {
        kind: hl_provider::TreeKind::File,
        read: true,
        write: true,
        create: true,
        truncate: false,
        append: false,
        exclusive: true,
        mode: 0o640,
    };
    assert_eq!(
        worker.tree_open_options(0, b"/created", options),
        Err(hl_engine::native::ProjectionError::Linux(libc::EROFS)),
    );
    worker.close().unwrap();
    read_only.commit(channel).unwrap();
    assert_eq!(read_only.reap(channel.session()[0]).unwrap(), ChildExit::Code(0));

    let writable = ProcessAuthority::projected_root_writable(program, &root, &image, Arc::new(LinuxHost)).unwrap();
    let channel = writable.open([11, 12]).unwrap();
    let mut worker = writable.worker(channel).unwrap();
    worker.enter(|| ()).unwrap();
    let handle = worker.tree_open_options(0, b"/created", options).unwrap();
    assert_eq!(worker.tree_write(handle, 0, b"ab").unwrap(), 2);
    assert_eq!(worker.tree_append(handle, b"cd").unwrap(), (2, 4));
    worker.tree_truncate(handle, 3).unwrap();
    assert_eq!(worker.tree_read(handle, 0, 8).unwrap(), b"abc");
    worker.tree_close(handle).unwrap();
    worker.close().unwrap();
    writable.commit(channel).unwrap();
    assert_eq!(writable.reap(channel.session()[0]).unwrap(), ChildExit::Code(0));
    assert_eq!(std::fs::read(root.join("created")).unwrap(), b"abc");
    std::fs::remove_dir_all(root).unwrap();
}
