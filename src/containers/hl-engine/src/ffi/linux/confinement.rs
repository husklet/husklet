#![allow(unsafe_code)]

use ::libc;

pub(crate) struct Seccomp;

impl Seccomp {
    pub(crate) fn apply() -> Result<(), ()> {
        let mut filter = Vec::new();
        filter.push(Self::statement(0x20, 4));
        filter.push(Self::jump(0x15, Self::audit_arch(), 1, 0));
        filter.push(Self::statement(0x06, 0x8000_0000));
        filter.push(Self::statement(0x20, 0));
        // clone3 hides flags behind a pointer that classic BPF cannot inspect.
        // Report it unavailable so libc falls back to filterable legacy clone.
        filter.push(Self::jump(0x15, libc::SYS_clone3 as u32, 0, 1));
        filter.push(Self::statement(0x06, 0x0005_0000 | libc::ENOSYS as u32));
        // Legacy clone exposes flags as scalar argument zero on both supported
        // architectures. Admit only libc's exact pthread shape.
        filter.push(Self::jump(0x15, libc::SYS_clone as u32, 0, 5));
        filter.push(Self::statement(0x20, 16));
        filter.push(Self::jump(0x15, Self::thread_flags(), 0, 3));
        filter.push(Self::statement(0x20, 20));
        filter.push(Self::jump(0x15, 0, 0, 1));
        filter.push(Self::statement(0x06, 0x7fff_0000));
        filter.push(Self::statement(0x20, 0));
        Self::allow_shape(&mut filter, libc::SYS_set_robust_list, &[(24, 24), (28, 0)]);
        Self::allow_shape(
            &mut filter,
            libc::SYS_rseq,
            &[
                (24, 33),
                (28, 0),
                (32, 0),
                (36, 0),
                (40, Self::rseq_signature()),
                (44, 0),
            ],
        );
        Self::allow_shape(&mut filter, libc::SYS_prctl, &[(16, libc::PR_SET_NAME as u32), (20, 0)]);
        Self::allow_shape(
            &mut filter,
            libc::SYS_eventfd2,
            &[
                (16, 0),
                (20, 0),
                (24, (libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) as u32),
                (28, 0),
            ],
        );
        Self::allow_shape(&mut filter, libc::SYS_memfd_create, &[(24, 3), (28, 0)]);
        Self::allow_fcntl(&mut filter);
        for syscall in Self::allowed() {
            filter.push(Self::jump(0x15, *syscall as u32, 0, 1));
            filter.push(Self::statement(0x06, 0x7fff_0000));
        }
        filter.push(Self::statement(0x06, 0x0005_0000 | libc::EPERM as u32));
        let program = libc::sock_fprog {
            len: u16::try_from(filter.len()).map_err(|_| ())?,
            filter: filter.as_mut_ptr(),
        };
        // SAFETY: prctl receives scalar policy values and a live immutable BPF
        // program for the duration of the call; the kernel copies the program.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(());
        }
        // SAFETY: no_new_privs is active and program points to validated local
        // storage copied synchronously by the kernel.
        if unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &raw const program) } != 0 {
            return Err(());
        }
        Ok(())
    }

    const fn statement(code: u16, value: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k: value,
        }
    }

    const fn jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: yes,
            jf: no,
            k: value,
        }
    }

    #[cfg(target_arch = "x86_64")]
    const fn audit_arch() -> u32 {
        0xc000_003e
    }
    #[cfg(target_arch = "aarch64")]
    const fn audit_arch() -> u32 {
        0xc000_00b7
    }

    fn allowed() -> &'static [i64] {
        &[
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_close,
            libc::SYS_sendto,
            libc::SYS_recvfrom,
            // Local cancellation shuts down only the pre-captured lifecycle
            // socket; the worker owns no ambient network descriptors.
            libc::SYS_shutdown,
            // The authority lifecycle watcher polls one inherited health
            // socket for HUP/ERR; it never inspects the authenticated RPC fd.
            libc::SYS_ppoll,
            libc::SYS_mmap,
            libc::SYS_mprotect,
            libc::SYS_munmap,
            libc::SYS_brk,
            libc::SYS_ftruncate,
            libc::SYS_pwrite64,
            libc::SYS_fstat,
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigprocmask,
            libc::SYS_rt_sigreturn,
            libc::SYS_futex,
            libc::SYS_clock_gettime,
            libc::SYS_sched_yield,
            // Host Rust runtime initialization requires kernel entropy. This
            // grants bytes only; it cannot open files, sockets, or processes.
            libc::SYS_getrandom,
            libc::SYS_exit,
            libc::SYS_exit_group,
        ]
    }

    const fn thread_flags() -> u32 {
        (libc::CLONE_VM
            | libc::CLONE_FS
            | libc::CLONE_FILES
            | libc::CLONE_SIGHAND
            | libc::CLONE_THREAD
            | libc::CLONE_SYSVSEM
            | libc::CLONE_SETTLS
            | libc::CLONE_PARENT_SETTID
            | libc::CLONE_CHILD_CLEARTID) as u32
    }

    fn allow_shape(filter: &mut Vec<libc::sock_filter>, syscall: i64, checks: &[(u32, u32)]) {
        let skip = u8::try_from(checks.len() * 3 + 1).expect("bounded seccomp rule");
        filter.push(Self::jump(0x15, syscall as u32, 0, skip));
        for (offset, value) in checks {
            filter.push(Self::statement(0x20, *offset));
            filter.push(Self::jump(0x15, *value, 1, 0));
            filter.push(Self::statement(0x06, 0x0005_0000 | libc::EPERM as u32));
        }
        filter.push(Self::statement(0x06, 0x7fff_0000));
        filter.push(Self::statement(0x20, 0));
    }

    fn allow_fcntl(filter: &mut Vec<libc::sock_filter>) {
        filter.push(Self::jump(0x15, libc::SYS_fcntl as u32, 0, 11));
        filter.push(Self::statement(0x20, 24));
        filter.push(Self::jump(0x15, 1030, 0, 3));
        filter.push(Self::statement(0x20, 32));
        filter.push(Self::jump(0x15, 3, 0, 6));
        filter.push(Self::statement(0x06, 0x7fff_0000));
        filter.push(Self::statement(0x20, 24));
        filter.push(Self::jump(0x15, 1033, 0, 3));
        filter.push(Self::statement(0x20, 32));
        filter.push(Self::jump(0x15, 15, 0, 1));
        filter.push(Self::statement(0x06, 0x7fff_0000));
        filter.push(Self::statement(0x06, 0x0005_0000 | libc::EPERM as u32));
        filter.push(Self::statement(0x20, 0));
    }

    #[cfg(target_arch = "aarch64")]
    const fn rseq_signature() -> u32 {
        0xd428_bc00
    }
    #[cfg(target_arch = "x86_64")]
    const fn rseq_signature() -> u32 {
        0x5305_3053
    }

    pub(crate) fn variants_denied() -> bool {
        // SAFETY: these scalar-only probes pass no dereferenced pointers. The
        // installed filter must reject them before the kernel performs work.
        let prctl = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
        let prctl_denied = prctl < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        // SAFETY: eventfd2 takes scalar arguments and retains no Rust storage.
        let initial = unsafe { libc::syscall(libc::SYS_eventfd2, 1_u32, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        let initial_denied = initial < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        // SAFETY: same scalar-only probe with a non-approved flag shape.
        let flags = unsafe { libc::syscall(libc::SYS_eventfd2, 0_u32, 0_u32) };
        let flags_denied = flags < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        // SAFETY: the filter rejects this non-sealable flag shape before the
        // kernel observes the static name pointer.
        let memfd = unsafe { libc::syscall(libc::SYS_memfd_create, c"denied".as_ptr(), 1_u32) };
        let memfd_denied = memfd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        // SAFETY: this scalar-only unsupported command is rejected before the
        // invalid descriptor can be inspected.
        let fcntl = unsafe { libc::syscall(libc::SYS_fcntl, -1_i32, libc::F_GETFD, 0_u32) };
        let fcntl_denied = fcntl < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        prctl_denied && initial_denied && flags_denied && memfd_denied && fcntl_denied
    }
}
