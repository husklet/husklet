use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{OperationActor, OperationContext, SeekPosition};

use super::{
    AddressSpaceView, DescriptorView, Error, LimitResource, LimitView, MemoryRegionLabel, MemoryRegionView, MemoryView,
    MountEntry, MountView, NetworkInterfaceView, NetworkView, ProcessState, ProcessView, Procfs, UnixSocketView,
};
use crate::OpenIntent;

struct Source {
    process: ProcessView,
    descriptors: Vec<DescriptorView>,
    oom_score_adj: Mutex<i16>,
    resolutions: AtomicUsize,
    thread_resolutions: AtomicUsize,
    generation: AtomicU16,
}

fn process_number(process: super::ProcessIdentity) -> u32 {
    process.slot() + 1
}

impl super::Source for Source {
    fn resolve_process(&self, process: u32) -> Result<super::ProcessIdentity, Error> {
        self.resolutions.fetch_add(1, Ordering::Relaxed);
        if process != self.process.process {
            return Err(Error::NotFound);
        }
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        super::ProcessIdentity::new(process.checked_sub(1).ok_or(Error::NotFound)?, generation).ok_or(Error::NotFound)
    }

    fn processes(&self) -> Result<Vec<u32>, Error> {
        Ok(vec![self.process.process])
    }

    fn resolve_thread(
        &self,
        process: super::ProcessIdentity,
        thread: Option<u32>,
    ) -> Result<super::ThreadIdentity, Error> {
        self.thread_resolutions.fetch_add(1, Ordering::Relaxed);
        let thread = thread.unwrap_or(7);
        (process_number(process) == self.process.process && matches!(thread, 7 | 9))
            .then(|| super::ThreadIdentity::new(thread - 1, 1).unwrap())
            .ok_or(Error::NotFound)
    }

    fn threads(&self, process: super::ProcessIdentity) -> Result<Vec<u32>, Error> {
        (process_number(process) == self.process.process)
            .then(|| vec![7, 9])
            .ok_or(Error::NotFound)
    }

    fn root(&self, process: super::ProcessIdentity) -> Result<Vec<u8>, Error> {
        (process_number(process) == self.process.process)
            .then(|| b"/sandbox".to_vec())
            .ok_or(Error::NotFound)
    }

    fn cwd(&self, process: super::ProcessIdentity) -> Result<Vec<u8>, Error> {
        (process_number(process) == self.process.process)
            .then(|| b"/sandbox/work".to_vec())
            .ok_or(Error::NotFound)
    }

    fn process(&self, process: super::ProcessIdentity) -> Result<ProcessView, Error> {
        (process_number(process) == self.process.process)
            .then(|| self.process.clone())
            .ok_or(Error::NotFound)
    }

    fn environment(&self, process: super::ProcessIdentity) -> Result<Vec<u8>, Error> {
        (process_number(process) == self.process.process)
            .then(|| b"HOME=/root\0TERM=xterm\0".to_vec())
            .ok_or(Error::NotFound)
    }

    fn oom_score_adj(
        &self,
        process: super::ProcessIdentity,
        _thread: Option<super::ThreadIdentity>,
    ) -> Result<i16, Error> {
        (process_number(process) == self.process.process)
            .then(|| *self.oom_score_adj.lock().unwrap())
            .ok_or(Error::NotFound)
    }

    fn write_oom_score_adj(
        &self,
        process: super::ProcessIdentity,
        _thread: Option<super::ThreadIdentity>,
        _actor: OperationActor,
        value: i16,
    ) -> Result<(), hl_descriptor::ObjectError> {
        if process_number(process) != self.process.process {
            return Err(hl_descriptor::ObjectError::Retired);
        }
        *self.oom_score_adj.lock().unwrap() = value;
        Ok(())
    }

    fn cpu(&self) -> Result<super::CpuView, Error> {
        super::CpuView::new(
            3,
            super::CpuModel::Aarch64 {
                hardware: 0,
                hardware_second: 0,
            },
        )
        .map(|view| {
            view.with_ticks(vec![
                super::CpuTicks {
                    user: 10,
                    nice: 1,
                    system: 2,
                    idle: 30,
                },
                super::CpuTicks {
                    user: 20,
                    nice: 2,
                    system: 4,
                    idle: 40,
                },
                super::CpuTicks {
                    user: 30,
                    nice: 3,
                    system: 6,
                    idle: 50,
                },
            ])
        })
        .ok_or(Error::Invalid)
    }

    fn system(&self) -> Result<super::SystemView, Error> {
        Ok(super::SystemView {
            uptime_seconds: 12,
            process_creations: 9,
            total_memory: 64 * 1024 * 1024,
            free_memory: 32 * 1024 * 1024,
        })
    }

    fn cgroup(&self) -> Result<super::CgroupView, Error> {
        super::CgroupView::new(3, Some(2), Some(4096), None, 1024, vec![7], vec![7, 9]).ok_or(Error::Invalid)
    }

    fn descriptor_numbers(&self, process: super::ProcessIdentity) -> Result<Vec<i32>, Error> {
        (process_number(process) == self.process.process)
            .then(|| self.descriptors.iter().map(|descriptor| descriptor.number).collect())
            .ok_or(Error::NotFound)
    }

    fn descriptor(&self, process: super::ProcessIdentity, number: i32) -> Result<DescriptorView, Error> {
        (process_number(process) == self.process.process)
            .then(|| {
                self.descriptors
                    .iter()
                    .find(|descriptor| descriptor.number == number)
                    .cloned()
            })
            .flatten()
            .ok_or(Error::NotFound)
    }

    fn mounts(&self, process: super::ProcessIdentity) -> Result<MountView, Error> {
        (process_number(process) == self.process.process)
            .then(fixed_mounts)
            .ok_or(Error::NotFound)
    }

    fn network(&self, process: super::ProcessIdentity) -> Result<NetworkView, Error> {
        (process_number(process) == self.process.process)
            .then(|| NetworkView {
                generation: 11,
                internet: Vec::new(),
                interfaces: vec![
                    NetworkInterfaceView {
                        name: b"lo".to_vec(),
                        index: 1,
                        loopback: true,
                        address: [0; 6],
                        ipv4: None,
                        prefix: 0,
                        receive: [0; 8],
                        transmit: [0; 8],
                    },
                    NetworkInterfaceView {
                        name: b"eth0".to_vec(),
                        index: 2,
                        loopback: false,
                        address: [2, 0x42, 0xac, 0x11, 0, 2],
                        ipv4: Some([172, 17, 0, 2]),
                        prefix: 16,
                        receive: [0; 8],
                        transmit: [0; 8],
                    },
                ],
                unix: vec![UnixSocketView {
                    identity: 0x2a,
                    reference_count: 2,
                    protocol: 0,
                    flags: 0x10000,
                    socket_type: 1,
                    state: 1,
                    inode: 65543,
                    path: Some(vec![b'/', b'r', b'u', b'n', b'/', 0xff]),
                }],
            })
            .ok_or(Error::NotFound)
    }

    fn memory(&self, process: super::ProcessIdentity) -> Result<super::MemoryView, Error> {
        (process_number(process) == self.process.process)
            .then_some(super::MemoryView {
                page_bytes: 4096,
                total_pages: 16,
                resident_pages: 10,
                shared_pages: 2,
                text_pages: 3,
                data_pages: 7,
            })
            .ok_or(Error::NotFound)
    }

    fn address_space(&self, process: super::ProcessIdentity) -> Result<AddressSpaceView, Error> {
        (process_number(process) == self.process.process)
            .then(|| {
                AddressSpaceView::new(
                    9,
                    4096,
                    vec![MemoryRegionView {
                        start: 0x1000,
                        end: 0x3000,
                        protection: 5,
                        shared: false,
                        backing_offset: 0x2000,
                        device: 0x108,
                        inode: 7,
                        path: Some(b"/bin/program".to_vec()),
                        label: None,
                        resident_pages: 2,
                    }],
                )
                .unwrap()
            })
            .ok_or(Error::NotFound)
    }

    fn uts(&self, process: super::ProcessIdentity) -> Result<super::UtsView, Error> {
        (process_number(process) == self.process.process)
            .then(|| super::UtsView {
                namespace: 73,
                hostname: b"guest".to_vec(),
                domainname: Vec::new(),
            })
            .ok_or(Error::NotFound)
    }
}

#[test]
fn fake_resolves_only_its_exact_process() {
    let source = fixed_source();
    assert_eq!(
        super::Source::resolve_process(&source, source.process.process),
        Ok(super::ProcessIdentity::new(source.process.process - 1, 1).unwrap())
    );
    assert_eq!(super::Source::resolve_process(&source, 0), Err(Error::NotFound));
}

fn mount(
    identity: (u32, u32),
    device: (u32, u32),
    point: &[u8],
    options: &[&[u8]],
    filesystem: &[u8],
    source: &[u8],
    super_options: &[&[u8]],
) -> MountEntry {
    MountEntry::new(
        identity.0,
        identity.1,
        device,
        b"/".to_vec(),
        point.to_vec(),
        options.iter().map(|value| value.to_vec()).collect(),
        Vec::new(),
        filesystem.to_vec(),
        source.to_vec(),
        super_options.iter().map(|value| value.to_vec()).collect(),
    )
    .unwrap()
}

fn fixed_mounts() -> MountView {
    MountView::new(vec![
        mount(
            (23, 0),
            (0, 24),
            b"/",
            &[b"rw", b"relatime"],
            b"overlay",
            b"overlay",
            &[b"rw"],
        ),
        mount(
            (24, 23),
            (0, 25),
            b"/proc",
            &[b"rw", b"nosuid", b"nodev", b"noexec", b"relatime"],
            b"proc",
            b"proc",
            &[b"rw"],
        ),
        mount(
            (25, 23),
            (0, 26),
            b"/dev",
            &[b"rw", b"nosuid"],
            b"tmpfs",
            b"tmpfs",
            &[b"rw", b"size=65536k", b"mode=755"],
        ),
        mount(
            (26, 25),
            (0, 27),
            b"/dev/pts",
            &[b"rw", b"nosuid", b"noexec", b"relatime"],
            b"devpts",
            b"devpts",
            &[b"rw", b"gid=5", b"mode=620", b"ptmxmode=666"],
        ),
        mount(
            (27, 23),
            (0, 28),
            b"/sys",
            &[b"ro", b"nosuid", b"nodev", b"noexec", b"relatime"],
            b"sysfs",
            b"sysfs",
            &[b"ro"],
        ),
        mount(
            (28, 27),
            (0, 29),
            b"/sys/fs/cgroup",
            &[b"ro", b"nosuid", b"nodev", b"noexec", b"relatime"],
            b"cgroup2",
            b"cgroup",
            &[b"rw", b"nsdelegate"],
        ),
        mount(
            (29, 25),
            (0, 30),
            b"/dev/mqueue",
            &[b"rw", b"nosuid", b"nodev", b"noexec", b"relatime"],
            b"mqueue",
            b"mqueue",
            &[b"rw"],
        ),
        mount(
            (30, 25),
            (0, 31),
            b"/dev/shm",
            &[b"rw", b"nosuid", b"nodev", b"noexec", b"relatime"],
            b"tmpfs",
            b"shm",
            &[b"rw", b"size=65536k"],
        ),
    ])
    .unwrap()
}

fn procfs() -> Procfs {
    Procfs::new(Arc::new(fixed_source()))
}

fn fixed_source() -> Source {
    Source {
        process: process(),
        descriptors: vec![DescriptorView {
            number: 4,
            offset: 19,
            flags: 0o4002,
            mount: Some(7),
            inode: 91,
            target: Some(b"/data/file".to_vec()),
        }],
        oom_score_adj: Mutex::new(0),
        resolutions: AtomicUsize::new(0),
        thread_resolutions: AtomicUsize::new(0),
        generation: AtomicU16::new(1),
    }
}

fn assert_task(source: &Source, operation: impl FnOnce() -> Result<(), Error>) {
    let processes = source.resolutions.load(Ordering::Relaxed);
    let threads = source.thread_resolutions.load(Ordering::Relaxed);
    operation().unwrap();
    assert_eq!(source.resolutions.load(Ordering::Relaxed), processes + 1);
    assert_eq!(source.thread_resolutions.load(Ordering::Relaxed), threads + 1);
}

#[test]
fn task_resolves_once() {
    let source = Arc::new(fixed_source());
    let procfs = Procfs::new(Arc::clone(&source) as Arc<dyn super::Source>);
    let operations: &[&dyn Fn() -> Result<(), Error>] = &[
        &|| {
            procfs
                .open(b"/proc/7/task/9/status", 7, OpenIntent::default())
                .map(|_| ())
        },
        &|| {
            procfs
                .open(b"/proc/7/task/9/comm", 7, OpenIntent::default())
                .map(|_| ())
        },
        &|| procfs.read_link(b"/proc/7/task/9/cwd", 7).map(|_| ()),
        &|| procfs.kind(b"/proc/7/task/9/status", 7).map(|_| ()),
        &|| procfs.metadata(b"/proc/7/task/9/comm", 7).map(|_| ()),
        &|| procfs.read_link(b"/proc/7/task/9/ns/uts", 7).map(|_| ()),
        &|| procfs.namespace_inode(b"/proc/7/task/9/ns/uts", 7).map(|_| ()),
        &|| procfs.read_link(b"/proc/thread-self", 7).map(|_| ()),
    ];
    for operation in operations {
        assert_task(&source, operation);
    }
}

fn assert_one(source: &Source, operation: impl FnOnce() -> Result<(), Error>) {
    let before = source.resolutions.load(Ordering::Relaxed);
    operation().unwrap();
    assert_eq!(source.resolutions.load(Ordering::Relaxed), before + 1);
}

#[test]
fn per_process_operations_resolve_numeric_pid_once() {
    let source = Arc::new(fixed_source());
    let procfs = Procfs::new(Arc::clone(&source) as Arc<dyn super::Source>);
    let operations: &[&dyn Fn() -> Result<(), Error>] = &[
        &|| procfs.open(b"/proc/7/status", 7, OpenIntent::default()).map(|_| ()),
        &|| procfs.read_link(b"/proc/7/cwd", 7).map(|_| ()),
        &|| procfs.kind(b"/proc/7/status", 7).map(|_| ()),
        &|| procfs.metadata(b"/proc/7/status", 7).map(|_| ()),
        &|| procfs.uts_namespace(b"/proc/7/ns/uts", 7).map(|_| ()),
        &|| procfs.namespace_inode(b"/proc/7/ns/uts", 7).map(|_| ()),
    ];
    for operation in operations {
        let before = source.resolutions.load(Ordering::Relaxed);
        operation().unwrap();
        assert_eq!(source.resolutions.load(Ordering::Relaxed), before + 1);
    }
}

#[test]
fn namespaces_resolve_once() {
    let source = Arc::new(fixed_source());
    let procfs = Procfs::new(Arc::clone(&source) as Arc<dyn super::Source>);
    let paths: &[&[u8]] = &[
        b"/proc/7/ns/uts",
        b"/proc/7/ns/net",
        b"/proc/7/ns/cgroup",
        b"/proc/7/ns/ipc",
        b"/proc/7/ns/mnt",
        b"/proc/7/ns/pid",
        b"/proc/7/ns/time",
        b"/proc/7/ns/user",
    ];
    for path in paths {
        // Every resolution returns a fresh generation, deterministically modeling
        // PID reuse. Each operation must consume only its first pinned identity.
        assert_one(&source, || procfs.open(path, 7, OpenIntent::default()).map(|_| ()));
        assert_one(&source, || procfs.read_link(path, 7).map(|_| ()));
        assert_one(&source, || procfs.kind(path, 7).map(|_| ()));
        assert_one(&source, || procfs.metadata(path, 7).map(|_| ()));
        assert_one(&source, || procfs.namespace_inode(path, 7).map(|_| ()));
    }
}

#[test]
fn membership_resolves_once() {
    let source = Arc::new(fixed_source());
    let procfs = Procfs::new(Arc::clone(&source) as Arc<dyn super::Source>);
    assert_one(&source, || {
        procfs.open(b"/proc/7/cgroup", 7, OpenIntent::default()).map(|_| ())
    });
    assert_one(&source, || procfs.kind(b"/proc/7/cgroup", 7).map(|_| ()));
    assert_one(&source, || procfs.metadata(b"/proc/7/cgroup", 7).map(|_| ()));
}

#[test]
fn oom_adjustment_is_live_and_shared_by_duplicates() {
    let procfs = procfs();
    let file = procfs
        .open(b"/proc/self/oom_score_adj", 7, OpenIntent::from_bits(OpenIntent::WRITE))
        .unwrap()
        .unwrap();
    let peer = Arc::clone(&file);
    let context = OperationContext {
        actor: Some(OperationActor {
            process: 7,
            process_generation: 1,
            thread: 7,
            thread_generation: 1,
        }),
        cancellation: None,
    };
    assert_eq!(file.write_context(b"250\n", context).unwrap(), 4);
    assert_eq!(file.seek(SeekPosition::Start(0)).unwrap(), 0);
    assert_eq!(file.write_context(b"-7\n", context).unwrap(), 3);
    assert_eq!(file.seek(SeekPosition::Start(0)).unwrap(), 0);
    let mut output = [0; 16];
    let count = peer.read(&mut output).unwrap();
    assert_eq!(&output[..count], b"-7\n");
    assert!(file.write_context(b"1001\n", context).is_err());
}

fn process() -> ProcessView {
    ProcessView {
        process: 7,
        parent: 1,
        name: *b"worker\0\0\0\0\0\0\0\0\0\0",
        state: ProcessState::Running,
        threads: 4,
        umask: Some(0o027),
        real_user: 10,
        effective_user: 11,
        saved_user: 12,
        filesystem_user: 13,
        real_group: 20,
        effective_group: 21,
        saved_group: 22,
        filesystem_group: 23,
        groups: vec![20, 30],
        inheritable: 1,
        permitted: 2,
        effective: 3,
        bounding: 4,
        ambient: 5,
        no_new_privileges: true,
        seccomp_mode: 2,
        seccomp_filters: 1,
        pending_signals: 1,
        blocked_signals: 2,
        ignored_signals: 4,
        caught_signals: 8,
        limits: vec![LimitView {
            resource: LimitResource::Core,
            soft: 0,
            hard: u64::MAX,
        }],
        allowed_mask: String::from("7"),
        allowed_list: String::from("0-2"),
        memory: Some(super::MemoryView {
            page_bytes: 4096,
            total_pages: 8,
            resident_pages: 4,
            shared_pages: 1,
            text_pages: 2,
            data_pages: 3,
        }),
    }
}

#[test]
fn process_status_projects_disabled_seccomp_baseline() {
    let mut view = process();
    view.seccomp_mode = 0;
    view.seccomp_filters = 0;
    let text = String::from_utf8(view.status()).unwrap();
    assert!(text.contains("Seccomp:\t0\n"));
    assert!(text.contains("Seccomp_filters:\t0\n"));
}

#[test]
fn snapshot_content() {
    let procfs = procfs();
    let status = procfs
        .open(b"/proc/self/status", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let limits = procfs
        .open(b"proc/7/limits", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let mut bytes = [0; 2048];
    let count = status.read(&mut bytes).unwrap();
    let text = std::str::from_utf8(&bytes[..count]).unwrap();
    assert!(text.contains("Name:\tworker\n"));
    assert!(text.contains("Umask:\t0027\n"));
    assert!(text.contains("Ngid:\t0\n"));
    assert!(text.contains("TracerPid:\t0\n"));
    assert!(text.contains("Threads:\t4\n"));
    assert!(text.contains("Uid:\t10\t11\t12\t13\n"));

    assert!(text.contains("SigPnd:\t0000000000000001\n"));
    assert!(text.contains("SigBlk:\t0000000000000002\n"));
    assert!(text.contains("SigIgn:\t0000000000000004\n"));
    assert!(text.contains("SigCgt:\t0000000000000008\n"));
    assert!(text.contains("Seccomp:\t2\n"));
    assert!(text.contains("Seccomp_filters:\t1\n"));
    assert!(text.contains("Cpus_allowed:\t7\n"));
    assert!(text.contains("Cpus_allowed_list:\t0-2\n"));
    assert!(text.contains("VmSize:\t32 kB\n"));
    assert!(text.contains("VmRSS:\t16 kB\n"));
    let statm = procfs
        .open(b"/proc/self/statm", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let count = statm.read(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"16 10 2 3 0 7 0\n");
    let count = limits.read(&mut bytes).unwrap();
    let text = std::str::from_utf8(&bytes[..count]).unwrap();
    assert!(text.contains("Max core file size"));
    assert!(text.contains('0'));
    assert!(text.contains("unlimited"));
}

#[test]
fn global_linux_identity_files_are_discoverable_and_readable() {
    let procfs = procfs();
    let root = procfs.open(b"/proc", 7, OpenIntent::from_bits(0)).unwrap().unwrap();
    let names = root
        .read_directory(64)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    for name in [b"cmdline".as_slice(), b"filesystems", b"loadavg", b"version"] {
        assert!(names.iter().any(|entry| entry == name));
        let path = [b"/proc/".as_slice(), name].concat();
        let file = procfs.open(&path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let mut output = [0_u8; 512];
        assert!(file.read(&mut output).unwrap() > 0);
    }
}

#[test]
fn process_directory_advertises_discoverable_leaves() {
    let directory = procfs()
        .open(b"/proc/self", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let names = directory
        .read_directory(64)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    for name in [
        b"mountinfo".as_slice(),
        b"limits",
        b"environ",
        b"smaps",
        b"pagemap",
        b"io",
    ] {
        assert!(names.iter().any(|entry| entry == name));
    }
}

#[test]
fn environment_snapshot_preserves_nul_records() {
    let procfs = procfs();
    let file = procfs
        .open(b"/proc/self/environ", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let mut bytes = [0; 64];
    let count = file.read(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"HOME=/root\0TERM=xterm\0");
    assert_eq!(file.metadata().unwrap().size, 0);
}

#[test]
fn common_proc_leaves_use_typed_snapshots() {
    let procfs = procfs();
    let mut bytes = [0; 1024];
    for (path, fields) in [
        (
            b"/proc/self/io".as_slice(),
            [b"rchar".as_slice(), b"read_bytes".as_slice()],
        ),
        (b"/proc/net/sockstat", [b"sockets:".as_slice(), b"TCP:".as_slice()]),
        (b"/proc/devices", [b"Block devices:".as_slice(), b"loop".as_slice()]),
        (b"/proc/meminfo", [b"AnonPages:".as_slice(), b"Inactive:".as_slice()]),
    ] {
        let file = procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let count = file.read(&mut bytes).unwrap();
        assert!(
            fields
                .iter()
                .all(|field| bytes[..count].windows(field.len()).any(|window| window == *field))
        );
    }
    let stat = procfs
        .open(b"/proc/stat", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let count = stat.read(&mut bytes).unwrap();
    let text = std::str::from_utf8(&bytes[..count]).unwrap();
    assert!(!text.contains("intr 0\n"));
    assert!(!text.contains("ctxt 0\n"));
    assert!(text.contains("processes 265\n"));
    assert!(text.contains("cpu  60 6 12 120 0 0 0 0 0 0\n"));
    assert!(text.contains("cpu1 20 2 4 40 0 0 0 0 0 0\n"));
}

#[test]
fn synthetic_discovery_directories_are_typed() {
    let procfs = procfs();
    for path in [
        b"/sys/devices/system/cpu/cpu0/topology".as_slice(),
        b"/sys/class/block",
        b"/sys/block",
        b"/dev/fd",
    ] {
        assert_eq!(procfs.kind(path, 7).unwrap(), Some(super::NodeKind::Directory));
        assert!(procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().is_some());
    }
    assert_eq!(
        procfs.read_link(b"/proc/self/ns/net", 7).unwrap(),
        Some(b"net:[11]".to_vec())
    );
    assert_eq!(procfs.read_link(b"/dev/fd/4", 7).unwrap(), Some(b"/data/file".to_vec()));
}

#[test]
fn namespace_links_and_metadata_share_identity() {
    let procfs = procfs();
    for (name, target) in [
        ("cgroup", "cgroup:[4026531835]"),
        ("ipc", "ipc:[4026531839]"),
        ("mnt", "mnt:[4026531841]"),
        ("net", "net:[11]"),
        ("pid", "pid:[4026531836]"),
        ("time", "time:[4026531834]"),
        ("user", "user:[4026531837]"),
        ("uts", "uts:[73]"),
    ] {
        let path = format!("/proc/self/ns/{name}");
        assert_eq!(
            procfs.read_link(path.as_bytes(), 7).unwrap(),
            Some(target.as_bytes().to_vec())
        );
        let inode = target.split(['[', ']']).nth(1).unwrap().parse::<u64>().unwrap();
        assert_eq!(procfs.metadata(path.as_bytes(), 7).unwrap().unwrap().inode, inode);
    }
}

#[test]
fn unix_network_snapshot_folds_and_preserves_path_bytes() {
    let procfs = procfs();
    let expected = b"Num       RefCount Protocol Flags    Type St Inode Path\n\
000000000000002a: 00000002 00000000 00010000 0001 01 65543 /run/\xff\n";
    for path in [
        b"/proc/net/unix".as_slice(),
        b"/proc/self/net/unix",
        b"/proc/7/net/unix",
    ] {
        let file = procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let mut bytes = [0; 256];
        let count = file.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], expected);
        assert_eq!(file.metadata().unwrap().size, 0);
    }
    assert_eq!(procfs.kind(b"/proc/net", 7).unwrap(), Some(super::NodeKind::Directory));
    assert_eq!(
        procfs.kind(b"/proc/self/net/unix", 7).unwrap(),
        Some(super::NodeKind::Regular)
    );
    assert_eq!(procfs.metadata(b"/proc/8/net/unix", 7), Err(Error::NotFound));
}

#[test]
fn network_interfaces_drive_procfs_and_sysfs() {
    let procfs = procfs();
    for path in [b"/proc/net/dev".as_slice(), b"/proc/self/net/dev", b"/proc/7/net/dev"] {
        let file = procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let mut bytes = [0; 1024];
        let count = file.read(&mut bytes).unwrap();
        let text = std::str::from_utf8(&bytes[..count]).unwrap();
        assert!(text.contains("Receive") && text.contains("lo:") && text.contains("eth0:"));
    }
    let root = procfs
        .open(b"/sys/class/net", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let mut bytes = [0; 1024];
    let names = root
        .read_directory(8)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&b"lo".to_vec()) && names.contains(&b"eth0".to_vec()));
    let file = procfs
        .open(b"/sys/class/net/eth0/statistics/rx_bytes", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let count = file.read(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"0\n");
    assert_eq!(procfs.kind(b"/sys/class/net/missing", 7), Err(Error::NotFound));
    assert_eq!(procfs.metadata(b"/sys/class/net/missing", 7), Err(Error::NotFound));

    let route = procfs
        .open(b"/proc/net/route", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let count = route.read(&mut bytes).unwrap();
    let text = std::str::from_utf8(&bytes[..count]).unwrap();
    assert!(text.contains("eth0\t00000000\t010011AC\t0003"));
    assert!(text.contains("eth0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF"));
}

#[test]
fn address_space_snapshot_is_ordered_and_aggregates_once() {
    let regions = vec![
        MemoryRegionView {
            start: 0x1000,
            end: 0x3000,
            protection: 5,
            shared: false,
            backing_offset: 0,
            device: 3,
            inode: 7,
            path: Some(b"/bin/program".to_vec()),
            label: None,
            resident_pages: 2,
        },
        MemoryRegionView {
            start: 0x4000,
            end: 0x5000,
            protection: 3,
            shared: true,
            backing_offset: 0,
            device: 0,
            inode: 0,
            path: None,
            label: Some(MemoryRegionLabel::Heap),
            resident_pages: 1,
        },
    ];
    let space = AddressSpaceView::new(9, 4096, regions.clone()).unwrap();
    assert_eq!(
        space.memory(),
        MemoryView {
            page_bytes: 4096,
            total_pages: 3,
            resident_pages: 3,
            shared_pages: 1,
            text_pages: 2,
            data_pages: 1,
        }
    );
    let mut overlapping = regions;
    overlapping[1].start = 0x2000;
    assert!(AddressSpaceView::new(9, 4096, overlapping).is_none());
}

#[test]
fn maps_and_smaps_share_one_snapshot() {
    let procfs = procfs();
    let maps = procfs
        .open(b"/proc/self/maps", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let smaps = procfs
        .open(b"/proc/self/smaps", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let mut bytes = [0; 4096];
    let maps_count = maps.read(&mut bytes).unwrap();
    assert_eq!(
        &bytes[..maps_count],
        b"00001000-00003000 r-xp 00002000 01:08 7 /bin/program\n"
    );
    let smaps_count = smaps.read(&mut bytes).unwrap();
    let text = std::str::from_utf8(&bytes[..smaps_count]).unwrap();
    assert!(text.starts_with("00001000-00003000 r-xp 00002000 01:08 7 /bin/program\n"));
    assert!(text.contains("Size:                  8 kB\n"));
    assert!(text.contains("Rss:                   8 kB\nPss:                   8 kB\n"));
    assert!(text.ends_with("VmFlags: rd ex mr mw me ac \n"));
    let numa = procfs
        .open(b"/proc/self/numa_maps", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let count = numa.read(&mut bytes).unwrap();
    assert_eq!(
        &bytes[..count],
        b"00001000 default file=/bin/program mapped=2 active=0 N0=2 kernelpagesize_kB=4\n"
    );
    let rollup = procfs
        .open(b"/proc/self/smaps_rollup", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let count = rollup.read(&mut bytes).unwrap();
    assert!(
        std::str::from_utf8(&bytes[..count])
            .unwrap()
            .starts_with("00001000-00003000")
    );
}

#[test]
fn map_files_projects_file_backed_ranges() {
    let procfs = procfs();
    let directory = procfs
        .open(b"/proc/self/map_files", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let batch = directory.read_directory(8).unwrap();
    assert_eq!(batch.entries[2].name, b"1000-3000");
    assert_eq!(batch.entries[2].file_type, 10);
    assert_eq!(
        procfs.read_link(b"/proc/self/map_files/1000-3000", 7).unwrap(),
        Some(b"/bin/program".to_vec())
    );
    let metadata = procfs.metadata(b"/proc/self/map_files/1000-3000", 7).unwrap().unwrap();
    assert_eq!(metadata.kind, 10);
    assert_eq!(
        procfs.read_link(b"/proc/self/map_files/1000-2000", 7),
        Err(Error::NotFound)
    );
}

#[test]
fn cpu_projection() {
    let procfs = procfs();
    let mut bytes = [0; 512];
    for path in [
        b"/sys/devices/system/cpu/online".as_slice(),
        b"/sys/devices/system/cpu/possible".as_slice(),
        b"/sys/devices/system/cpu/present".as_slice(),
        b"/sys/fs/cgroup/cpuset.cpus.effective".as_slice(),
    ] {
        let file = procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let count = file.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], b"0-2\n");
    }
    let cpuinfo = procfs
        .open(b"/proc/cpuinfo", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let count = cpuinfo.read(&mut bytes).unwrap();
    assert_eq!(
        std::str::from_utf8(&bytes[..count])
            .unwrap()
            .matches("processor\t:")
            .count(),
        3
    );
    let directory = procfs
        .open(b"/sys/devices/system/cpu", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let batch = directory.read_directory(8).unwrap();
    assert_eq!(
        batch
            .entries
            .iter()
            .map(|entry| entry.name.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b".".as_slice(),
            b"..".as_slice(),
            b"cpu0".as_slice(),
            b"cpu1".as_slice(),
            b"cpu2".as_slice(),
        ],
    );
    assert_eq!(
        procfs.kind(b"/sys/devices/system/cpu/cpu2", 7).unwrap(),
        Some(super::NodeKind::Directory),
    );
    assert_eq!(procfs.kind(b"/sys/devices/system/cpu/cpu3", 7), Err(Error::NotFound));
    for (leaf, expected) in [
        ("core_id", "2\n"),
        ("physical_package_id", "0\n"),
        ("cluster_id", "0\n"),
        ("thread_siblings", "4\n"),
        ("thread_siblings_list", "2\n"),
        ("core_cpus", "4\n"),
        ("core_cpus_list", "2\n"),
        ("core_siblings", "7\n"),
        ("core_siblings_list", "0-2\n"),
        ("package_cpus", "7\n"),
        ("package_cpus_list", "0-2\n"),
        ("cluster_cpus", "7\n"),
        ("cluster_cpus_list", "0-2\n"),
    ] {
        let path = format!("/sys/devices/system/cpu/cpu2/topology/{leaf}");
        let file = procfs
            .open(path.as_bytes(), 7, OpenIntent::from_bits(0))
            .unwrap()
            .unwrap();
        let count = file.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], expected.as_bytes());
    }
}

#[test]
fn cpu_model_contract() {
    let arm = super::CpuView::new(
        1,
        super::CpuModel::Aarch64 {
            hardware: 0x10_01fb,
            hardware_second: 0,
        },
    )
    .unwrap();
    let arm = String::from_utf8(arm.cpuinfo()).unwrap();
    assert!(arm.contains("Features\t: fp asimd aes pmull sha1 sha2 crc32 atomics asimddp"));
    assert!(arm.contains("CPU architecture: 8"));

    let x86 = super::CpuView::new(
        1,
        super::CpuModel::X86_64 {
            vendor: String::from("GenuineIntel"),
            family: 6,
            model: 44,
            stepping: 2,
            name: String::from("hl JIT x86-64 processor"),
            flags: vec!["fpu", "tsc", "sse2"],
        },
    )
    .unwrap();
    let x86 = String::from_utf8(x86.cpuinfo()).unwrap();
    assert!(x86.contains("vendor_id\t: GenuineIntel"));
    assert!(x86.contains("model name\t: hl JIT x86-64 processor"));
    assert!(x86.contains("flags\t\t: fpu tsc sse2"));
}

#[test]
fn cgroup_projection() {
    let procfs = procfs();
    let mut bytes = [0; 512];
    for (path, expected) in [
        (b"/proc/self/cgroup".as_slice(), b"0::/\n".as_slice()),
        (b"/sys/fs/cgroup/cpu.max", b"200000 100000\n"),
        (b"/sys/fs/cgroup/memory.max", b"4096\n"),
        (b"/sys/fs/cgroup/memory.current", b"1024\n"),
    ] {
        let file = procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let count = file.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], expected);
    }
    let directory = procfs
        .open(b"/sys/fs/cgroup", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let names = directory
        .read_directory(64)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 36);
    assert!(names.starts_with(&[b".".to_vec(), b"..".to_vec()]));
    assert!(names.contains(&b"cgroup.controllers".to_vec()));
    assert!(matches!(
        procfs.open(b"/sys/fs/cgroup/cpu.max", 7, OpenIntent::from_bits(OpenIntent::WRITE)),
        Err(Error::ReadOnly),
    ));
    for intent in [
        OpenIntent::WRITE,
        OpenIntent::CREATE,
        OpenIntent::TRUNCATE,
        OpenIntent::APPEND,
    ] {
        assert!(matches!(
            procfs.open(
                b"/sys/fs/cgroup/nonexistent.controller",
                7,
                OpenIntent::from_bits(intent)
            ),
            Err(Error::ReadOnly),
        ));
    }
    assert!(
        procfs
            .open(b"/sys/fs/cgroup/nonexistent.controller", 7, OpenIntent::from_bits(0))
            .unwrap()
            .is_none()
    );
}

#[test]
fn tuning_projection() {
    let procfs = procfs();
    let mut bytes = [0; 512];
    for (path, expected) in [
        (
            b"/proc/sys/kernel/sem".as_slice(),
            b"256\t131072\t500\t512\n".as_slice(),
        ),
        (b"/proc/sys/fs/inotify/max_user_instances", b"524288\n"),
        (b"/proc/sys/fs/mqueue/msgsize_max", b"8192\n"),
        (b"/proc/sys/net/ipv4/tcp_congestion_control", b"cubic\n"),
        (b"/sys/class/net/lo/flags", b"0x9\n"),
        (
            b"/sys/kernel/mm/transparent_hugepage/enabled",
            b"always [madvise] never\n",
        ),
    ] {
        let file = procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let count = file.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], expected);
    }
}

#[test]
fn mountinfo_projection() {
    let procfs = procfs();
    let file = procfs
        .open(b"/proc/self/mountinfo", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let mut bytes = [0; 2048];
    let count = file.read(&mut bytes).unwrap();
    assert_eq!(
        &bytes[..count],
        concat!(
            "23 0 0:24 / / rw,relatime - overlay overlay rw\n",
            "24 23 0:25 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw\n",
            "25 23 0:26 / /dev rw,nosuid - tmpfs tmpfs rw,size=65536k,mode=755\n",
            "26 25 0:27 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,gid=5,mode=620,ptmxmode=666\n",
            "27 23 0:28 / /sys ro,nosuid,nodev,noexec,relatime - sysfs sysfs ro\n",
            "28 27 0:29 / /sys/fs/cgroup ro,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw,nsdelegate\n",
            "29 25 0:30 / /dev/mqueue rw,nosuid,nodev,noexec,relatime - mqueue mqueue rw\n",
            "30 25 0:31 / /dev/shm rw,nosuid,nodev,noexec,relatime - tmpfs shm rw,size=65536k\n",
        )
        .as_bytes(),
    );
    assert_eq!(file.metadata().unwrap().size, 0);
    assert_eq!(
        procfs.kind(b"/proc/7/mountinfo", 7).unwrap(),
        Some(super::NodeKind::Regular)
    );
    assert_eq!(procfs.metadata(b"/proc/7/mountinfo", 7).unwrap().unwrap().size, 0);
    assert_eq!(procfs.kind(b"/proc/8/mountinfo", 7), Err(Error::NotFound));
}

#[test]
fn mounts_projection_uses_same_namespace() {
    let procfs = procfs();
    let expected = concat!(
        "overlay / overlay rw,relatime 0 0\n",
        "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n",
        "tmpfs /dev tmpfs rw,nosuid,size=65536k,mode=755 0 0\n",
        "devpts /dev/pts devpts rw,nosuid,noexec,relatime,gid=5,mode=620,ptmxmode=666 0 0\n",
        "sysfs /sys sysfs ro,nosuid,nodev,noexec,relatime 0 0\n",
        "cgroup /sys/fs/cgroup cgroup2 ro,nosuid,nodev,noexec,relatime,nsdelegate 0 0\n",
        "mqueue /dev/mqueue mqueue rw,nosuid,nodev,noexec,relatime 0 0\n",
        "shm /dev/shm tmpfs rw,nosuid,nodev,noexec,relatime,size=65536k 0 0\n",
    )
    .as_bytes();
    for path in [b"/proc/self/mounts".as_slice(), b"/proc/7/mounts"] {
        let file = procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let mut bytes = [0; 2048];
        let count = file.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], expected);
        assert_eq!(file.metadata().unwrap().size, 0);
    }
    // Linux spells /proc/mounts as a relative symlink to the caller's own table.
    assert_eq!(procfs.kind(b"/proc/mounts", 7), Ok(Some(super::NodeKind::Link)));
    assert_eq!(procfs.read_link(b"/proc/mounts", 7), Ok(Some(b"self/mounts".to_vec())));
    assert_eq!(procfs.kind(b"/proc/8/mounts", 7), Err(Error::NotFound));
}

#[test]
fn cursor_contract() {
    let procfs = procfs();
    let file = procfs
        .open(b"/proc/self/status", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let alias = Arc::clone(&file);
    let mut bytes = [0; 5];
    assert_eq!(file.read(&mut bytes).unwrap(), 5);
    assert_eq!(alias.seek(SeekPosition::Current(0)).unwrap(), 5);
    assert!(matches!(
        procfs.open(b"/proc/self/status", 7, OpenIntent::from_bits(OpenIntent::WRITE)),
        Err(Error::Access),
    ));
    assert!(
        procfs
            .open(b"/proc/self/maps", 7, OpenIntent::from_bits(0))
            .unwrap()
            .is_some()
    );
}

#[test]
fn descriptor_snapshots() {
    let procfs = procfs();
    assert_eq!(
        procfs.read_link(b"/proc/self/fd/4", 7).unwrap(),
        Some(b"/data/file".to_vec())
    );
    assert_eq!(
        procfs.read_link(b"/proc/7/fd/4", 7).unwrap(),
        Some(b"/data/file".to_vec())
    );
    let info = procfs
        .open(b"/proc/self/fdinfo/4", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let mut bytes = [0; 128];
    let count = info.read(&mut bytes).unwrap();
    assert_eq!(
        std::str::from_utf8(&bytes[..count]).unwrap(),
        "pos:\t19\nflags:\t04002\nmnt_id:\t7\nino:\t91\n"
    );
    let directory = procfs
        .open(b"/proc/self/fd", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let fd_metadata = directory.metadata().unwrap();
    assert_eq!(fd_metadata.kind, 4);
    assert_eq!(fd_metadata.permissions, 0o500);
    assert_eq!(fd_metadata.links, 2);
    assert_eq!(fd_metadata.size, 0);
    let batch = directory.read_directory(8).unwrap();
    assert_eq!(batch.entries[0].name, b".");
    assert_eq!(batch.entries[1].name, b"..");
    assert_eq!(batch.entries[2].name, b"4");
    assert_eq!(batch.entries[2].file_type, 10);
    let info_directory = procfs
        .open(b"/proc/7/fdinfo", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let info_metadata = info_directory.metadata().unwrap();
    assert_eq!(info_metadata.kind, 4);
    assert_eq!(info_metadata.permissions, 0o555);
    assert_ne!(fd_metadata.inode, info_metadata.inode);
    assert_eq!(
        procfs.metadata(b"/proc/self/fdinfo", 7).unwrap(),
        Some(info_metadata.clone())
    );
    let entry_metadata = procfs
        .open(b"/proc/self/fdinfo/4", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap()
        .metadata()
        .unwrap();
    assert_eq!(entry_metadata.kind, 8);
    assert_eq!(entry_metadata.permissions, 0o444);
    assert_eq!(
        procfs.metadata(b"/proc/self/fdinfo/4", 7).unwrap(),
        Some(entry_metadata)
    );
    let link_metadata = procfs.metadata(b"/proc/self/fd/4", 7).unwrap().unwrap();
    assert_eq!(link_metadata.kind, 10);
    assert_eq!(link_metadata.permissions, 0o777);
}

#[test]
fn self_link_live() {
    let procfs = procfs();
    assert_eq!(procfs.read_link(b"/proc/self", 7).unwrap(), Some(b"7".to_vec()));
    assert_eq!(procfs.read_link(b"proc/self", 7).unwrap(), Some(b"7".to_vec()));
    assert_eq!(procfs.kind(b"/proc/self", 7).unwrap(), Some(super::NodeKind::Link));
    assert_eq!(procfs.read_link(b"/proc/self", 8), Err(Error::NotFound));
    assert_eq!(
        procfs.read_link(b"/proc/self/root", 7).unwrap(),
        Some(b"/sandbox".to_vec()),
    );
    assert_eq!(procfs.kind(b"/proc/7/root", 7).unwrap(), Some(super::NodeKind::Link));
    assert_eq!(procfs.read_link(b"/proc/8/root", 7), Err(Error::NotFound));
    assert_eq!(
        procfs.read_link(b"/proc/self/cwd", 7).unwrap(),
        Some(b"/sandbox/work".to_vec()),
    );
    assert_eq!(procfs.kind(b"/proc/7/cwd", 7).unwrap(), Some(super::NodeKind::Link));
    assert_eq!(procfs.read_link(b"/proc/8/cwd", 7), Err(Error::NotFound));
}

#[test]
fn live_root_tasks() {
    let procfs = procfs();
    let root = procfs.open(b"/proc", 7, OpenIntent::from_bits(0)).unwrap().unwrap();
    let batch = root.read_directory(16).unwrap();
    assert!(
        batch
            .entries
            .iter()
            .any(|entry| entry.name == b"7" && entry.file_type == 4)
    );
    assert!(
        batch
            .entries
            .iter()
            .any(|entry| entry.name == b"self" && entry.file_type == 10)
    );
    assert!(
        batch
            .entries
            .iter()
            .any(|entry| entry.name == b"thread-self" && entry.file_type == 10)
    );
    assert_eq!(procfs.kind(b"/proc", 7).unwrap(), Some(super::NodeKind::Directory));
    assert_eq!(
        procfs.read_link_for(b"/proc/thread-self", 7, 9).unwrap(),
        Some(b"7/task/9".to_vec()),
    );
    assert_eq!(procfs.read_link_for(b"/proc/thread-self", 7, 10), Err(Error::NotFound));
    assert_eq!(procfs.metadata(b"/proc/thread-self", 7).unwrap().unwrap().kind, 10);
}

#[test]
fn uts_namespace_identity() {
    let procfs = procfs();
    let directory = procfs
        .open(b"/proc/self/ns", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let entries = directory.read_directory(8).unwrap();
    assert!(
        entries
            .entries
            .iter()
            .any(|entry| entry.name == b"uts" && entry.file_type == 10)
    );
    assert_eq!(
        procfs.read_link(b"/proc/7/ns/uts", 7).unwrap(),
        Some(b"uts:[73]".to_vec()),
    );
    assert_eq!(procfs.uts_namespace(b"/proc/self/ns/uts", 7), Ok(Some(73)));
    assert_eq!(
        procfs.read_link(b"/proc/7/task/9/ns/uts", 7),
        Ok(Some(b"uts:[73]".to_vec())),
    );
    assert_eq!(
        procfs.kind(b"/proc/7/task/9/ns", 7),
        Ok(Some(super::NodeKind::Directory)),
    );
    assert_eq!(procfs.kind(b"/proc/7/task/10/ns", 7), Err(Error::NotFound));
    assert_eq!(procfs.kind(b"/proc/7/ns/uts", 7), Ok(Some(super::NodeKind::Link)));
    let metadata = procfs.metadata(b"/proc/self/ns/uts", 7).unwrap().unwrap();
    assert_eq!((metadata.inode, metadata.kind, metadata.permissions), (73, 10, 0o777));
    assert_eq!(procfs.read_link(b"/proc/8/ns/uts", 7), Err(Error::NotFound));
    assert_eq!(procfs.uts_namespace(b"/proc/8/ns/uts", 7), Err(Error::NotFound));
}

#[test]
fn task_leaf_folding() {
    let procfs = procfs();
    let process = procfs
        .open(b"/proc/self", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let process_entries = process.read_directory(32).unwrap();
    assert!(
        process_entries
            .entries
            .iter()
            .any(|entry| entry.name == b"task" && entry.file_type == 4)
    );
    assert!(
        process_entries
            .entries
            .iter()
            .any(|entry| entry.name == b"cwd" && entry.file_type == 10)
    );
    assert!(
        process_entries
            .entries
            .iter()
            .any(|entry| entry.name == b"comm" && entry.file_type == 8)
    );

    let tasks = procfs
        .open(b"/proc/7/task", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let task_entries = tasks.read_directory(16).unwrap();
    assert_eq!(
        task_entries
            .entries
            .iter()
            .map(|entry| entry.name.as_slice())
            .collect::<Vec<_>>(),
        [b".".as_slice(), b"..".as_slice(), b"7".as_slice(), b"9".as_slice()],
    );
    assert_eq!(
        procfs.kind(b"/proc/self/task/9", 7).unwrap(),
        Some(super::NodeKind::Directory)
    );
    let thread = procfs
        .open(b"/proc/self/task/9", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    assert!(
        thread
            .read_directory(16)
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.name == b"comm" && entry.file_type == 8)
    );

    let folded = procfs
        .open(b"/proc/self/task/9/status", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let mut bytes = [0; 512];
    let count = folded.read(&mut bytes).unwrap();
    assert!(std::str::from_utf8(&bytes[..count]).unwrap().contains("Pid:\t7\n"));
    let comm = procfs
        .open(b"/proc/self/task/9/comm", 7, OpenIntent::from_bits(0))
        .unwrap()
        .unwrap();
    let count = comm.read(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"worker\n");
    assert!(matches!(
        procfs.open(b"/proc/7/task/8/status", 7, OpenIntent::from_bits(0)),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        procfs.open(b"/proc/8/task/9/status", 7, OpenIntent::from_bits(0)),
        Err(Error::NotFound)
    ));
    assert_eq!(
        procfs.read_link(b"/proc/7/task/9/cwd", 7).unwrap(),
        Some(b"/sandbox/work".to_vec()),
    );
}

/// Field-by-field record of `lstat`/`statx` on Linux 7.0.11: procfs and cgroup2
/// report a zero size, sysfs attributes report one page, and an `fd` link 64.
#[test]
fn stat_shape_matches_linux() {
    let procfs = procfs();
    for (path, kind, permissions, size, links) in [
        (b"/proc".as_slice(), 4, 0o555, 0, 1),
        (b"/proc/7", 4, 0o555, 0, 1),
        (b"/proc/7/status", 8, 0o444, 0, 1),
        (b"/proc/7/maps", 8, 0o444, 0, 1),
        (b"/proc/7/mountinfo", 8, 0o444, 0, 1),
        (b"/proc/7/limits", 8, 0o444, 0, 1),
        (b"/proc/7/environ", 8, 0o400, 0, 1),
        (b"/proc/7/io", 8, 0o400, 0, 1),
        (b"/proc/7/oom_score", 8, 0o444, 0, 1),
        (b"/proc/7/oom_score_adj", 8, 0o644, 0, 1),
        (b"/proc/7/cgroup", 8, 0o444, 0, 1),
        (b"/proc/7/cwd", 10, 0o777, 0, 1),
        (b"/proc/7/root", 10, 0o777, 0, 1),
        (b"/proc/7/fd", 4, 0o500, 0, 2),
        (b"/proc/7/fd/4", 10, 0o777, 64, 1),
        (b"/proc/7/fdinfo", 4, 0o555, 0, 2),
        (b"/proc/7/fdinfo/4", 8, 0o444, 0, 1),
        (b"/proc/7/ns", 4, 0o511, 0, 2),
        (b"/proc/7/map_files", 4, 0o500, 0, 2),
        (b"/proc/7/task", 4, 0o555, 0, 1),
        (b"/proc/7/task/9", 4, 0o555, 0, 1),
        (b"/proc/cpuinfo", 8, 0o444, 0, 1),
        (b"/proc/meminfo", 8, 0o444, 0, 1),
        (b"/proc/uptime", 8, 0o444, 0, 1),
        (b"/proc/sys/kernel/pid_max", 8, 0o444, 0, 1),
        (b"/sys/fs/cgroup", 4, 0o755, 0, 1),
        (b"/sys/devices/system/cpu", 4, 0o755, 0, 1),
        (b"/sys/devices/system/cpu/online", 8, 0o444, 4096, 1),
        (b"/sys/devices/system/cpu/cpu0", 4, 0o755, 0, 1),
        (b"/sys/devices/system/cpu/cpu0/topology", 4, 0o755, 0, 2),
        (b"/sys/devices/system/cpu/cpu0/topology/core_id", 8, 0o444, 4096, 1),
        (b"/sys/class/net", 4, 0o755, 0, 2),
    ] {
        let spelling = String::from_utf8_lossy(path).into_owned();
        let metadata = procfs
            .metadata(path, 7)
            .unwrap_or_else(|error| panic!("{spelling}: {error:?}"))
            .unwrap_or_else(|| panic!("{spelling} is not projected"));
        assert_eq!((&spelling, metadata.kind), (&spelling, kind));
        assert_eq!((&spelling, metadata.permissions), (&spelling, permissions));
        assert_eq!((&spelling, metadata.size), (&spelling, size));
        assert_eq!((&spelling, metadata.links), (&spelling, links));
        assert_eq!((&spelling, metadata.blocks_512), (&spelling, 0));
    }
}

/// An open procfs description reports the same zero size as the path walk, which
/// is what Linux reports through `fstat`.
#[test]
fn open_description_reports_zero_size() {
    let procfs = procfs();
    for path in [
        b"/proc/self/status".as_slice(),
        b"/proc/self/maps",
        b"/proc/self/limits",
        b"/proc/self/environ",
        b"/proc/7/mountinfo",
    ] {
        let file = procfs.open(path, 7, OpenIntent::from_bits(0)).unwrap().unwrap();
        let metadata = file.metadata().unwrap();
        let mut bytes = [0; 4096];
        assert!(file.read(&mut bytes).unwrap() > 0);
        assert_eq!(metadata.size, 0, "{}", String::from_utf8_lossy(path));
        assert_eq!(metadata.blocks_512, 0);
    }
}
