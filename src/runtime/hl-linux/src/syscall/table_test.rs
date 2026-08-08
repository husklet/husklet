use std::collections::{BTreeSet, HashMap};

use hl_isa::GuestArchitecture;

use crate::{
    AioSyscalls, CANONICAL_SYSCALLS, Decision, DescriptorIoSyscalls, EventSyscalls, FilesystemSyscalls, IpcSyscalls,
    LinuxResult, MemorySyscalls, NetworkSyscalls, SeccompDispatch, SeccompSyscalls, SyscallDispatcher,
    SyscallDisposition, SyscallFamily, SyscallFrame, SyscallOperation, SyscallPorts, TaskSignalTimeSyscalls,
    X86_LEGACY_SYSCALLS, X86_TRANSLATIONS,
};

#[test]
fn tables_no_operation() {
    let canonical = CANONICAL_SYSCALLS
        .iter()
        .map(|entry| entry.operation.canonical_number)
        .collect::<BTreeSet<_>>();
    let x86 = X86_TRANSLATIONS
        .iter()
        .map(|entry| entry.raw_number)
        .collect::<BTreeSet<_>>();
    assert_eq!(canonical.len(), CANONICAL_SYSCALLS.len());
    assert_eq!(x86.len(), X86_TRANSLATIONS.len());
    assert_eq!(X86_TRANSLATIONS.len(), CANONICAL_SYSCALLS.len());
    assert!(
        X86_LEGACY_SYSCALLS
            .iter()
            .all(|legacy| { !x86.contains(&legacy.raw_number) })
    );
    for definition in CANONICAL_SYSCALLS {
        assert!(X86_TRANSLATIONS.iter().any(|translation| {
            translation.raw_number == definition.x86_number
                && translation.canonical_number == definition.operation.canonical_number
        }));
    }
}

/// `access` and `chmod` once shared 32780, which anything keying on the canonical number would conflate.
#[test]
fn legacy_numbers_are_unique() {
    let raw = X86_LEGACY_SYSCALLS
        .iter()
        .map(|entry| entry.raw_number)
        .collect::<BTreeSet<_>>();
    let canonical = X86_LEGACY_SYSCALLS
        .iter()
        .map(|entry| entry.operation.canonical_number)
        .collect::<BTreeSet<_>>();
    assert_eq!(raw.len(), X86_LEGACY_SYSCALLS.len());
    assert_eq!(canonical.len(), X86_LEGACY_SYSCALLS.len());
    let shared = CANONICAL_SYSCALLS
        .iter()
        .map(|entry| entry.operation.canonical_number)
        .collect::<BTreeSet<_>>();
    assert!(canonical.iter().all(|number| !shared.contains(number)));
}

/// Every rewrite case in the oracle's `translator/guest/x86_64/legacy.c` must route to a typed operation.
#[test]
fn oracle_legacy_cases_all_route() {
    const ORACLE_CASES: [(u16, &str); 36] = [
        (2, "open"),
        (4, "stat"),
        (6, "lstat"),
        (7, "poll"),
        (21, "access"),
        (22, "pipe"),
        (23, "select"),
        (33, "dup2"),
        (34, "pause"),
        (37, "alarm"),
        (56, "clone"),
        (57, "fork"),
        (58, "vfork"),
        (82, "rename"),
        (83, "mkdir"),
        (84, "rmdir"),
        (85, "creat"),
        (86, "link"),
        (87, "unlink"),
        (88, "symlink"),
        (89, "readlink"),
        (90, "chmod"),
        (92, "chown"),
        (94, "lchown"),
        (111, "getpgrp"),
        (132, "utime"),
        (133, "mknod"),
        // 158 arch_prctl is intercepted ahead of the table by the runtime router, so it stays untyped here.
        (158, "arch_prctl"),
        (201, "time"),
        (213, "epoll_create"),
        (232, "epoll_wait"),
        (235, "utimes"),
        (253, "inotify_init"),
        (261, "futimesat"),
        (282, "signalfd"),
        (284, "eventfd"),
    ];
    for (number, name) in ORACLE_CASES {
        if name == "arch_prctl" {
            continue;
        }
        let route = SyscallDispatcher::route(GuestArchitecture::X86_64, u64::from(number));
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("x86 {number} ({name}) must be typed");
        };
        assert_eq!(operation.name, name, "x86 {number}");
    }
}

#[test]
fn x86_chown_routes_to_filesystem() {
    let route = SyscallDispatcher::route(GuestArchitecture::X86_64, 92);
    let SyscallDisposition::Operation(operation) = route.disposition else {
        panic!("chown must be typed");
    };
    assert_eq!(operation.name, "chown");
    assert_eq!(operation.family, SyscallFamily::Filesystem);
}

#[test]
fn x86_vfork_routes_to_task_runtime() {
    let route = SyscallDispatcher::route(GuestArchitecture::X86_64, 58);
    let SyscallDisposition::Operation(operation) = route.disposition else {
        panic!("vfork must be typed");
    };
    assert_eq!(operation.name, "vfork");
    assert_eq!(operation.family, SyscallFamily::TaskSignalTime);
}

#[test]
fn x86_getpgrp_routes_to_task_runtime() {
    let route = SyscallDispatcher::route(GuestArchitecture::X86_64, 111);
    let SyscallDisposition::Operation(operation) = route.disposition else {
        panic!("getpgrp must be typed");
    };
    assert_eq!(operation.name, "getpgrp");
    assert_eq!(operation.family, SyscallFamily::TaskSignalTime);
}

#[test]
fn seccomp_numbers_match() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 277), (GuestArchitecture::X86_64, 317)] {
        let route = SyscallDispatcher::route(architecture, number);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("seccomp must route");
        };
        assert_eq!(operation.name, "seccomp");
        assert_eq!(operation.canonical_number, 277);
        assert_eq!(operation.family, SyscallFamily::Seccomp);
    }
}

#[test]
fn membarrier_numbers_match() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 283), (GuestArchitecture::X86_64, 324)] {
        let route = SyscallDispatcher::route(architecture, number);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("membarrier must route");
        };
        assert_eq!(operation.name, "membarrier");
        assert_eq!(operation.family, SyscallFamily::Memory);
    }
}

#[test]
fn syncfs_numbers_match() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 267), (GuestArchitecture::X86_64, 306)] {
        let route = SyscallDispatcher::route(architecture, number);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("syncfs must route");
        };
        assert_eq!(operation.name, "syncfs");
        assert_eq!(operation.family, SyscallFamily::DescriptorIo);
    }
}

#[test]
fn adjtimex_numbers_match() {
    for (architecture, adjtimex, clock_adjtime) in [
        (GuestArchitecture::Aarch64, 171, 266),
        (GuestArchitecture::X86_64, 159, 305),
    ] {
        for (number, name) in [(adjtimex, "adjtimex"), (clock_adjtime, "clock_adjtime")] {
            let route = SyscallDispatcher::route(architecture, number);
            let SyscallDisposition::Operation(operation) = route.disposition else {
                panic!("{name} must route");
            };
            assert_eq!(operation.name, name);
            assert_eq!(operation.family, SyscallFamily::TaskSignalTime);
        }
    }
}

#[test]
fn tid_address_numbers() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 96), (GuestArchitecture::X86_64, 218)] {
        let route = SyscallDispatcher::route(architecture, number);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("set_tid_address must route");
        };
        assert_eq!(operation.name, "set_tid_address");
        assert_eq!(operation.canonical_number, 96);
        assert_eq!(operation.family, SyscallFamily::TaskSignalTime);
    }
}

#[test]
fn vectored_v2_routes() {
    for (architecture, preadv2, pwritev2) in [
        (GuestArchitecture::Aarch64, 286, 287),
        (GuestArchitecture::X86_64, 327, 328),
    ] {
        for (number, name, canonical) in [(preadv2, "preadv2", 286), (pwritev2, "pwritev2", 287)] {
            let route = SyscallDispatcher::route(architecture, number);
            let SyscallDisposition::Operation(operation) = route.disposition else {
                panic!("vectored v2 operation must route");
            };
            assert_eq!(operation.name, name);
            assert_eq!(operation.canonical_number, canonical);
            assert_eq!(operation.family, SyscallFamily::DescriptorIo);
        }
    }
}

#[test]
fn readahead_routes() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 213), (GuestArchitecture::X86_64, 187)] {
        let route = SyscallDispatcher::route(architecture, number);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("readahead must route");
        };
        assert_eq!(operation.name, "readahead");
        assert_eq!(operation.canonical_number, 213);
        assert_eq!(operation.family, SyscallFamily::DescriptorIo);
    }
}

#[test]
fn x86_readlink_route() {
    let route = SyscallDispatcher::route(GuestArchitecture::X86_64, 89);
    let SyscallDisposition::Operation(operation) = route.disposition else {
        panic!("readlink must route");
    };
    assert_eq!(operation.name, "readlink");
    assert_eq!(operation.family, SyscallFamily::Filesystem);
}

#[test]
fn namespace_numbers_match() {
    for (architecture, unshare, setns) in [
        (GuestArchitecture::Aarch64, 97, 268),
        (GuestArchitecture::X86_64, 272, 308),
    ] {
        for (number, name, canonical) in [(unshare, "unshare", 97), (setns, "setns", 268)] {
            let route = SyscallDispatcher::route(architecture, number);
            let SyscallDisposition::Operation(operation) = route.disposition else {
                panic!("namespace syscall must route");
            };
            assert_eq!(operation.name, name);
            assert_eq!(operation.canonical_number, canonical);
            assert_eq!(operation.family, SyscallFamily::TaskSignalTime);
        }
    }
}

#[test]
fn ioctl_numbers_match() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 29), (GuestArchitecture::X86_64, 16)] {
        let route = SyscallDispatcher::route(architecture, number);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("ioctl must route");
        };
        assert_eq!(operation.name, "ioctl");
        assert_eq!(operation.canonical_number, 29);
        assert_eq!(operation.family, SyscallFamily::DescriptorIo);
    }
}

#[test]
fn uname_numbers_match() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 160), (GuestArchitecture::X86_64, 63)] {
        let route = SyscallDispatcher::route(architecture, number);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("uname must route");
        };
        assert_eq!(operation.name, "uname");
        assert_eq!(operation.canonical_number, 160);
        assert_eq!(operation.family, SyscallFamily::TaskSignalTime);
    }
}

#[test]
fn sysinfo_numbers_match() {
    for (architecture, number) in [(GuestArchitecture::Aarch64, 179), (GuestArchitecture::X86_64, 99)] {
        let route = SyscallDispatcher::route(architecture, number);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("sysinfo must route");
        };
        assert_eq!(operation.name, "sysinfo");
        assert_eq!(operation.canonical_number, 179);
        assert_eq!(operation.family, SyscallFamily::TaskSignalTime);
    }
}

#[test]
fn scheduler_numbers_match() {
    for (architecture, yield_number, cpu_number) in [
        (GuestArchitecture::Aarch64, 124, 168),
        (GuestArchitecture::X86_64, 24, 309),
    ] {
        for (number, name) in [(yield_number, "sched_yield"), (cpu_number, "getcpu")] {
            let route = SyscallDispatcher::route(architecture, number);
            let SyscallDisposition::Operation(operation) = route.disposition else {
                panic!("scheduler operation must route");
            };
            assert_eq!(operation.name, name);
            assert_eq!(operation.family, SyscallFamily::TaskSignalTime);
        }
    }
}

#[test]
fn x86_legacy_routes() {
    let route = SyscallDispatcher::route(GuestArchitecture::X86_64, 33);
    let SyscallDisposition::Operation(operation) = route.disposition else {
        panic!("dup2 must route");
    };
    assert_eq!(operation.name, "dup2");
    assert_eq!(operation.family, SyscallFamily::DescriptorIo);
    let arm = SyscallDispatcher::route(GuestArchitecture::Aarch64, 32769);
    assert!(matches!(arm.disposition, SyscallDisposition::Reserved { .. }));

    let wait = SyscallDispatcher::route(GuestArchitecture::X86_64, 232);
    let SyscallDisposition::Operation(operation) = wait.disposition else {
        panic!("epoll_wait must route");
    };
    assert_eq!(operation.name, "epoll_wait");
    assert_eq!(operation.family, SyscallFamily::Event);

    let create = SyscallDispatcher::route(GuestArchitecture::X86_64, 213);
    let SyscallDisposition::Operation(operation) = create.disposition else {
        panic!("epoll_create must route");
    };
    assert_eq!(operation.name, "epoll_create");
    assert_eq!(operation.family, SyscallFamily::Event);
    let arm = SyscallDispatcher::route(GuestArchitecture::Aarch64, 213);
    assert!(!matches!(
        arm.disposition,
        SyscallDisposition::Operation(operation) if operation.name == "epoll_create"
    ));

    for (number, name) in [(132, "utime"), (235, "utimes"), (261, "futimesat")] {
        let route = SyscallDispatcher::route(GuestArchitecture::X86_64, number);
        assert!(matches!(
            route.disposition,
            SyscallDisposition::Operation(operation)
                if operation.name == name && operation.family == SyscallFamily::Filesystem
        ));
    }
}

#[test]
fn isa_tables_families() {
    for definition in CANONICAL_SYSCALLS {
        let arm = SyscallDispatcher::route(GuestArchitecture::Aarch64, definition.operation.canonical_number as u64);
        let x86 = SyscallDispatcher::route(GuestArchitecture::X86_64, definition.x86_number as u64);
        let (SyscallDisposition::Operation(arm), SyscallDisposition::Operation(x86)) =
            (arm.disposition, x86.disposition)
        else {
            panic!("typed operation missing");
        };
        assert_eq!((arm.name, arm.family), (x86.name, x86.family));
        assert_eq!(arm.canonical_number, x86.canonical_number);
    }
}

#[test]
fn fadvise_numbers_route() {
    for (architecture, raw) in [(GuestArchitecture::Aarch64, 223), (GuestArchitecture::X86_64, 221)] {
        let route = SyscallDispatcher::route(architecture, raw);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("fadvise64 must route");
        };
        assert_eq!(operation.canonical_number, 223);
        assert_eq!(operation.name, "fadvise64");
        assert_eq!(operation.family, SyscallFamily::Filesystem);
    }
}

#[test]
fn unsupported_reserved_explicit() {
    let unsupported = SyscallDispatcher::route(GuestArchitecture::Aarch64, 18);
    assert!(matches!(
        unsupported.disposition,
        SyscallDisposition::Unsupported {
            canonical_number: 18,
            ..
        }
    ));
    let reserved = SyscallDispatcher::route(GuestArchitecture::X86_64, 700);
    assert_eq!(
        reserved.disposition,
        SyscallDisposition::Reserved { canonical_number: 700 },
    );
}

struct Port {
    calls: HashMap<SyscallFamily, (&'static str, [u64; 6])>,
}

impl Port {
    fn new() -> Self {
        Self { calls: HashMap::new() }
    }
    fn record(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        self.calls.insert(operation.family, (operation.name, arguments));
        LinuxResult::Value(operation.canonical_number as u64)
    }
}

macro_rules! port {
    ($trait:ident) => {
        impl $trait for Port {
            fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
                self.record(operation, arguments)
            }
        }
    };
}

port!(FilesystemSyscalls);
port!(AioSyscalls);
port!(DescriptorIoSyscalls);
port!(EventSyscalls);
port!(MemorySyscalls);
port!(NetworkSyscalls);
port!(TaskSignalTimeSyscalls);
port!(IpcSyscalls);
port!(SeccompSyscalls);

#[test]
fn dispatcher_routes_family() {
    let mut filesystem = Port::new();
    let mut aio = Port::new();
    let mut descriptor = Port::new();
    let mut event = Port::new();
    let mut memory = Port::new();
    let mut network = Port::new();
    let mut task = Port::new();
    let mut ipc = Port::new();
    let mut seccomp = Port::new();
    let mut ports = SyscallPorts {
        aio: &mut aio,
        filesystem: &mut filesystem,
        descriptor_io: &mut descriptor,
        event: &mut event,
        memory: &mut memory,
        network: &mut network,
        task_signal_time: &mut task,
        ipc: &mut ipc,
        seccomp: &mut seccomp,
    };
    let result = SyscallDispatcher::dispatch(
        SyscallFrame {
            architecture: GuestArchitecture::X86_64,
            raw_number: 46,
            arguments: [1, 2, 3, 4, 5, 6],
        },
        &mut ports,
    );
    assert_eq!(result, LinuxResult::Value(211));
    let blocked = SyscallDispatcher::dispatch_seccomp(
        SyscallFrame {
            architecture: GuestArchitecture::X86_64,
            raw_number: 46,
            arguments: [0; 6],
        },
        Decision::ReturnErrno(13),
        &mut ports,
    );
    assert_eq!(
        blocked,
        SeccompDispatch::Result(LinuxResult::Error(crate::Errno::EACCES)),
    );
    assert_eq!(
        network.calls.get(&SyscallFamily::Network),
        Some(&("sendmsg", [1, 2, 3, 4, 5, 6])),
    );
    assert_eq!(network.calls.len(), 1);
    assert!(filesystem.calls.is_empty());
    assert!(task.calls.is_empty());
}

#[test]
#[ignore = "microbenchmark; run explicitly on an idle pinned host"]
fn route_lookup_benchmark() {
    let started = std::time::Instant::now();
    let mut checksum = 0_u64;
    for iteration in 0..20_000_000_u64 {
        let architecture = if iteration & 1 == 0 {
            GuestArchitecture::Aarch64
        } else {
            GuestArchitecture::X86_64
        };
        let number = match iteration % 5 {
            0 => 172,
            1 => 39,
            2 => 178,
            3 => 186,
            _ => 999,
        };
        let route = std::hint::black_box(SyscallDispatcher::route(architecture, number));
        checksum ^= u64::from(route.translation.canonical_number);
    }
    eprintln!(
        "route_lookup elapsed_ns={} checksum={checksum}",
        started.elapsed().as_nanos()
    );
}
