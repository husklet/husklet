use hl_isa::GuestArchitecture;

use super::ports::{Disposition, Family, Operation, Route};
use crate::NumberTranslation;

#[derive(Clone, Copy)]
pub struct Definition {
    pub operation: Operation,
    pub x86_number: u16,
}

#[derive(Clone, Copy)]
pub struct LegacyDefinition {
    pub raw_number: u16,
    pub operation: Operation,
}

pub const RETAINED_NUMBER_ORACLE: &str = "../engine/src/linux_abi/number.c";
pub const RETAINED_DISPATCH_ORACLE: &str = "../engine/src/linux_abi/syscall/dispatch.c";

macro_rules! definitions {
    ($(($canonical:literal, $x86:literal, $name:literal, $family:ident)),+ $(,)?) => {
        pub const CANONICAL_SYSCALLS: &[Definition] = &[
            $(Definition {
                operation: Operation {
                    canonical_number: $canonical,
                    name: $name,
                    family: Family::$family,
                },
                x86_number: $x86,
            }),+
        ];
        pub const X86_TRANSLATIONS: &[NumberTranslation] = &[
            $(NumberTranslation {
                raw_number: $x86,
                canonical_number: $canonical,
            }),+
        ];
    };
}

// Manually transcribed from the category-level service dispatcher and joined
// against number.c. This table contains operations with a typed Rust ABI plan,
// not every syscall that the retained C engine happens to service.
definitions!(
    (0, 206, "io_setup", Aio),
    (1, 207, "io_destroy", Aio),
    (2, 209, "io_submit", Aio),
    (3, 210, "io_cancel", Aio),
    (4, 208, "io_getevents", Aio),
    (5, 188, "setxattr", Filesystem),
    (6, 189, "lsetxattr", Filesystem),
    (7, 190, "fsetxattr", Filesystem),
    (8, 191, "getxattr", Filesystem),
    (9, 192, "lgetxattr", Filesystem),
    (10, 193, "fgetxattr", Filesystem),
    (11, 194, "listxattr", Filesystem),
    (12, 195, "llistxattr", Filesystem),
    (13, 196, "flistxattr", Filesystem),
    (14, 197, "removexattr", Filesystem),
    (15, 198, "lremovexattr", Filesystem),
    (16, 199, "fremovexattr", Filesystem),
    (17, 79, "getcwd", Filesystem),
    (19, 290, "eventfd2", Event),
    (20, 291, "epoll_create1", Event),
    (21, 233, "epoll_ctl", Event),
    (22, 281, "epoll_pwait", Event),
    (23, 32, "dup", DescriptorIo),
    (24, 292, "dup3", DescriptorIo),
    (25, 72, "fcntl", DescriptorIo),
    (26, 294, "inotify_init1", Event),
    (27, 254, "inotify_add_watch", Event),
    (28, 255, "inotify_rm_watch", Event),
    (29, 16, "ioctl", DescriptorIo),
    (32, 73, "flock", Filesystem),
    (33, 259, "mknodat", Filesystem),
    (34, 258, "mkdirat", Filesystem),
    (35, 263, "unlinkat", Filesystem),
    (36, 266, "symlinkat", Filesystem),
    (37, 265, "linkat", Filesystem),
    (38, 264, "renameat", Filesystem),
    (43, 137, "statfs", Filesystem),
    (44, 138, "fstatfs", Filesystem),
    (45, 76, "truncate", Filesystem),
    (46, 77, "ftruncate", Filesystem),
    (47, 285, "fallocate", Filesystem),
    (48, 269, "faccessat", Filesystem),
    (49, 80, "chdir", Filesystem),
    (50, 81, "fchdir", Filesystem),
    (51, 161, "chroot", Filesystem),
    (52, 91, "fchmod", Filesystem),
    (53, 268, "fchmodat", Filesystem),
    (54, 260, "fchownat", Filesystem),
    (55, 93, "fchown", Filesystem),
    (56, 257, "openat", Filesystem),
    (57, 3, "close", DescriptorIo),
    (59, 293, "pipe2", DescriptorIo),
    (61, 217, "getdents64", Filesystem),
    (62, 8, "lseek", DescriptorIo),
    (63, 0, "read", DescriptorIo),
    (64, 1, "write", DescriptorIo),
    (65, 19, "readv", DescriptorIo),
    (66, 20, "writev", DescriptorIo),
    (67, 17, "pread64", DescriptorIo),
    (68, 18, "pwrite64", DescriptorIo),
    (69, 295, "preadv", DescriptorIo),
    (70, 296, "pwritev", DescriptorIo),
    (71, 40, "sendfile", DescriptorIo),
    (72, 270, "pselect6", Event),
    (73, 271, "ppoll", Event),
    (74, 289, "signalfd4", Event),
    (75, 278, "vmsplice", DescriptorIo),
    (76, 275, "splice", DescriptorIo),
    (77, 276, "tee", DescriptorIo),
    (78, 267, "readlinkat", Filesystem),
    (79, 262, "newfstatat", Filesystem),
    (80, 5, "fstat", Filesystem),
    (82, 74, "fsync", DescriptorIo),
    (83, 75, "fdatasync", DescriptorIo),
    (84, 277, "sync_file_range", DescriptorIo),
    (285, 326, "copy_file_range", DescriptorIo),
    (85, 283, "timerfd_create", Event),
    (86, 286, "timerfd_settime", Event),
    (87, 287, "timerfd_gettime", Event),
    (88, 280, "utimensat", Filesystem),
    (90, 125, "capget", TaskSignalTime),
    (91, 126, "capset", TaskSignalTime),
    (92, 135, "personality", TaskSignalTime),
    (93, 60, "exit", TaskSignalTime),
    (94, 231, "exit_group", TaskSignalTime),
    (96, 218, "set_tid_address", TaskSignalTime),
    (97, 272, "unshare", TaskSignalTime),
    (95, 247, "waitid", TaskSignalTime),
    (98, 202, "futex", TaskSignalTime),
    (99, 273, "set_robust_list", TaskSignalTime),
    (100, 274, "get_robust_list", TaskSignalTime),
    (101, 35, "nanosleep", TaskSignalTime),
    (102, 36, "getitimer", TaskSignalTime),
    (103, 38, "setitimer", TaskSignalTime),
    (107, 222, "timer_create", TaskSignalTime),
    (108, 224, "timer_gettime", TaskSignalTime),
    (109, 225, "timer_getoverrun", TaskSignalTime),
    (110, 223, "timer_settime", TaskSignalTime),
    (111, 226, "timer_delete", TaskSignalTime),
    (112, 227, "clock_settime", TaskSignalTime),
    (113, 228, "clock_gettime", TaskSignalTime),
    (114, 229, "clock_getres", TaskSignalTime),
    (115, 230, "clock_nanosleep", TaskSignalTime),
    (117, 101, "ptrace", TaskSignalTime),
    (118, 142, "sched_setparam", TaskSignalTime),
    (119, 144, "sched_setscheduler", TaskSignalTime),
    (120, 145, "sched_getscheduler", TaskSignalTime),
    (121, 143, "sched_getparam", TaskSignalTime),
    (122, 203, "sched_setaffinity", TaskSignalTime),
    (123, 204, "sched_getaffinity", TaskSignalTime),
    (124, 24, "sched_yield", TaskSignalTime),
    (125, 146, "sched_get_priority_max", TaskSignalTime),
    (126, 147, "sched_get_priority_min", TaskSignalTime),
    (127, 148, "sched_rr_get_interval", TaskSignalTime),
    (129, 62, "kill", TaskSignalTime),
    (130, 200, "tkill", TaskSignalTime),
    (131, 234, "tgkill", TaskSignalTime),
    (132, 131, "sigaltstack", TaskSignalTime),
    (133, 130, "rt_sigsuspend", TaskSignalTime),
    (134, 13, "rt_sigaction", TaskSignalTime),
    (135, 14, "rt_sigprocmask", TaskSignalTime),
    (136, 127, "rt_sigpending", TaskSignalTime),
    (137, 128, "rt_sigtimedwait", TaskSignalTime),
    (138, 129, "rt_sigqueueinfo", TaskSignalTime),
    (139, 15, "rt_sigreturn", TaskSignalTime),
    (140, 141, "setpriority", TaskSignalTime),
    (141, 140, "getpriority", TaskSignalTime),
    (143, 114, "setregid", TaskSignalTime),
    (144, 106, "setgid", TaskSignalTime),
    (145, 113, "setreuid", TaskSignalTime),
    (146, 105, "setuid", TaskSignalTime),
    (147, 117, "setresuid", TaskSignalTime),
    (148, 118, "getresuid", TaskSignalTime),
    (149, 119, "setresgid", TaskSignalTime),
    (150, 120, "getresgid", TaskSignalTime),
    (151, 122, "setfsuid", TaskSignalTime),
    (152, 123, "setfsgid", TaskSignalTime),
    (153, 100, "times", TaskSignalTime),
    (154, 109, "setpgid", TaskSignalTime),
    (155, 121, "getpgid", TaskSignalTime),
    (156, 124, "getsid", TaskSignalTime),
    (157, 112, "setsid", TaskSignalTime),
    (158, 115, "getgroups", TaskSignalTime),
    (159, 116, "setgroups", TaskSignalTime),
    (160, 63, "uname", TaskSignalTime),
    (161, 170, "sethostname", TaskSignalTime),
    (162, 171, "setdomainname", TaskSignalTime),
    (163, 97, "getrlimit", TaskSignalTime),
    (164, 160, "setrlimit", TaskSignalTime),
    (165, 98, "getrusage", TaskSignalTime),
    (166, 95, "umask", TaskSignalTime),
    (167, 157, "prctl", TaskSignalTime),
    (168, 309, "getcpu", TaskSignalTime),
    (169, 96, "gettimeofday", TaskSignalTime),
    (171, 159, "adjtimex", TaskSignalTime),
    (172, 39, "getpid", TaskSignalTime),
    (173, 110, "getppid", TaskSignalTime),
    (174, 102, "getuid", TaskSignalTime),
    (175, 107, "geteuid", TaskSignalTime),
    (176, 104, "getgid", TaskSignalTime),
    (177, 108, "getegid", TaskSignalTime),
    (178, 186, "gettid", TaskSignalTime),
    (179, 99, "sysinfo", TaskSignalTime),
    (180, 240, "mq_open", Ipc),
    (181, 241, "mq_unlink", Ipc),
    (182, 242, "mq_timedsend", Ipc),
    (183, 243, "mq_timedreceive", Ipc),
    (184, 244, "mq_notify", Ipc),
    (185, 245, "mq_getsetattr", Ipc),
    (186, 68, "msgget", Ipc),
    (187, 71, "msgctl", Ipc),
    (188, 70, "msgrcv", Ipc),
    (189, 69, "msgsnd", Ipc),
    (190, 64, "semget", Ipc),
    (191, 66, "semctl", Ipc),
    (192, 220, "semtimedop", Ipc),
    (193, 65, "semop", Ipc),
    (194, 29, "shmget", Ipc),
    (195, 31, "shmctl", Ipc),
    (196, 30, "shmat", Ipc),
    (197, 67, "shmdt", Ipc),
    (198, 41, "socket", Network),
    (199, 53, "socketpair", Network),
    (200, 49, "bind", Network),
    (201, 50, "listen", Network),
    (202, 43, "accept", Network),
    (203, 42, "connect", Network),
    (204, 51, "getsockname", Network),
    (205, 52, "getpeername", Network),
    (206, 44, "sendto", Network),
    (207, 45, "recvfrom", Network),
    (208, 54, "setsockopt", Network),
    (209, 55, "getsockopt", Network),
    (210, 48, "shutdown", Network),
    (211, 46, "sendmsg", Network),
    (212, 47, "recvmsg", Network),
    (213, 187, "readahead", DescriptorIo),
    (214, 12, "brk", Memory),
    (215, 11, "munmap", Memory),
    (216, 25, "mremap", Memory),
    (220, 56, "clone", TaskSignalTime),
    (221, 59, "execve", TaskSignalTime),
    (222, 9, "mmap", Memory),
    (223, 221, "fadvise64", Filesystem),
    (226, 10, "mprotect", Memory),
    (227, 26, "msync", Memory),
    (228, 149, "mlock", Memory),
    (229, 150, "munlock", Memory),
    (230, 151, "mlockall", Memory),
    (231, 152, "munlockall", Memory),
    (232, 27, "mincore", Memory),
    (233, 28, "madvise", Memory),
    (236, 239, "get_mempolicy", Memory),
    (239, 279, "move_pages", Memory),
    (240, 297, "rt_tgsigqueueinfo", TaskSignalTime),
    (242, 288, "accept4", Network),
    (243, 299, "recvmmsg", Network),
    (260, 61, "wait4", TaskSignalTime),
    (261, 302, "prlimit64", TaskSignalTime),
    (262, 300, "fanotify_init", TaskSignalTime),
    (263, 301, "fanotify_mark", TaskSignalTime),
    (264, 303, "name_to_handle_at", Filesystem),
    (265, 304, "open_by_handle_at", Filesystem),
    (266, 305, "clock_adjtime", TaskSignalTime),
    (267, 306, "syncfs", DescriptorIo),
    (268, 308, "setns", TaskSignalTime),
    (274, 314, "sched_setattr", TaskSignalTime),
    (275, 315, "sched_getattr", TaskSignalTime),
    (269, 307, "sendmmsg", Network),
    (270, 310, "process_vm_readv", Memory),
    (271, 311, "process_vm_writev", Memory),
    (276, 316, "renameat2", Filesystem),
    (277, 317, "seccomp", Seccomp),
    (278, 318, "getrandom", DescriptorIo),
    (279, 319, "memfd_create", Memory),
    (280, 321, "bpf", TaskSignalTime),
    (281, 322, "execveat", TaskSignalTime),
    (282, 323, "userfaultfd", TaskSignalTime),
    (283, 324, "membarrier", Memory),
    (284, 325, "mlock2", Memory),
    (286, 327, "preadv2", DescriptorIo),
    (287, 328, "pwritev2", DescriptorIo),
    (291, 332, "statx", Filesystem),
    (424, 424, "pidfd_send_signal", TaskSignalTime),
    (425, 425, "io_uring_setup", TaskSignalTime),
    (426, 426, "io_uring_enter", TaskSignalTime),
    (427, 427, "io_uring_register", TaskSignalTime),
    (434, 434, "pidfd_open", TaskSignalTime),
    (435, 435, "clone3", TaskSignalTime),
    (436, 436, "close_range", DescriptorIo),
    (437, 437, "openat2", Filesystem),
    (438, 438, "pidfd_getfd", TaskSignalTime),
    (439, 439, "faccessat2", Filesystem),
    (441, 441, "epoll_pwait2", Event),
    (449, 449, "futex_waitv", TaskSignalTime),
    (452, 452, "fchmodat2", Filesystem),
    (32768, 34, "pause", TaskSignalTime)
);

/// x86-64 operations whose Linux ABI has no AArch64 syscall-number peer.
pub const X86_LEGACY_SYSCALLS: &[LegacyDefinition] = &[
    LegacyDefinition {
        raw_number: 132,
        operation: Operation {
            canonical_number: 32790,
            name: "utime",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 235,
        operation: Operation {
            canonical_number: 32791,
            name: "utimes",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 261,
        operation: Operation {
            canonical_number: 32792,
            name: "futimesat",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 213,
        operation: Operation {
            canonical_number: 32789,
            name: "epoll_create",
            family: Family::Event,
        },
    },
    LegacyDefinition {
        raw_number: 37,
        operation: Operation {
            canonical_number: 32785,
            name: "alarm",
            family: Family::TaskSignalTime,
        },
    },
    LegacyDefinition {
        raw_number: 7,
        operation: Operation {
            canonical_number: 32777,
            name: "poll",
            family: Family::Event,
        },
    },
    LegacyDefinition {
        raw_number: 21,
        operation: Operation {
            canonical_number: 32780,
            name: "access",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 23,
        operation: Operation {
            canonical_number: 32778,
            name: "select",
            family: Family::Event,
        },
    },
    LegacyDefinition {
        raw_number: 33,
        operation: Operation {
            canonical_number: 32769,
            name: "dup2",
            family: Family::DescriptorIo,
        },
    },
    LegacyDefinition {
        raw_number: 57,
        operation: Operation {
            canonical_number: 32776,
            name: "fork",
            family: Family::TaskSignalTime,
        },
    },
    LegacyDefinition {
        raw_number: 58,
        operation: Operation {
            canonical_number: 32783,
            name: "vfork",
            family: Family::TaskSignalTime,
        },
    },
    LegacyDefinition {
        raw_number: 82,
        operation: Operation {
            canonical_number: 32770,
            name: "rename",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 83,
        operation: Operation {
            canonical_number: 32771,
            name: "mkdir",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 84,
        operation: Operation {
            canonical_number: 32772,
            name: "rmdir",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 85,
        operation: Operation {
            canonical_number: 32782,
            name: "creat",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 86,
        operation: Operation {
            canonical_number: 32773,
            name: "link",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 87,
        operation: Operation {
            canonical_number: 32774,
            name: "unlink",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 88,
        operation: Operation {
            canonical_number: 32775,
            name: "symlink",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 89,
        operation: Operation {
            canonical_number: 32781,
            name: "readlink",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 90,
        operation: Operation {
            canonical_number: 32780,
            name: "chmod",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 92,
        operation: Operation {
            canonical_number: 32786,
            name: "chown",
            family: Family::Filesystem,
        },
    },
    LegacyDefinition {
        raw_number: 111,
        operation: Operation {
            canonical_number: 32787,
            name: "getpgrp",
            family: Family::TaskSignalTime,
        },
    },
    LegacyDefinition {
        raw_number: 201,
        operation: Operation {
            canonical_number: 32788,
            name: "time",
            family: Family::TaskSignalTime,
        },
    },
    LegacyDefinition {
        raw_number: 232,
        operation: Operation {
            canonical_number: 32779,
            name: "epoll_wait",
            family: Family::Event,
        },
    },
];

pub struct Table;

impl Table {
    pub fn route(architecture: GuestArchitecture, raw: u64) -> Route {
        let Some(raw_number) = u16::try_from(raw).ok() else {
            return Self::reserved(u16::MAX);
        };
        if let Some(route) = Self::legacy(architecture, raw_number) {
            return route;
        }
        let definition = match architecture {
            GuestArchitecture::Aarch64 => CANONICAL_SYSCALLS
                .iter()
                .find(|entry| entry.operation.canonical_number == raw_number),
            GuestArchitecture::X86_64 => CANONICAL_SYSCALLS.iter().find(|entry| entry.x86_number == raw_number),
        };
        if let Some(definition) = definition {
            let canonical_number = definition.operation.canonical_number;
            return Route {
                translation: NumberTranslation {
                    raw_number,
                    canonical_number,
                },
                disposition: Disposition::Operation(definition.operation),
            };
        }
        if raw_number <= 471 {
            Route {
                translation: NumberTranslation {
                    raw_number,
                    canonical_number: raw_number,
                },
                disposition: Disposition::Unsupported {
                    canonical_number: raw_number,
                    name: "not-yet-typed",
                },
            }
        } else {
            Self::reserved(raw_number)
        }
    }

    fn legacy(architecture: GuestArchitecture, raw_number: u16) -> Option<Route> {
        if architecture != GuestArchitecture::X86_64 {
            return None;
        }
        for definition in X86_LEGACY_SYSCALLS {
            if definition.raw_number == raw_number {
                return Some(Route {
                    translation: NumberTranslation {
                        raw_number,
                        canonical_number: definition.operation.canonical_number,
                    },
                    disposition: Disposition::Operation(definition.operation),
                });
            }
        }
        None
    }

    const fn reserved(raw_number: u16) -> Route {
        Route {
            translation: NumberTranslation {
                raw_number,
                canonical_number: raw_number,
            },
            disposition: Disposition::Reserved {
                canonical_number: raw_number,
            },
        }
    }
}
