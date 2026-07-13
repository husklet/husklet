//! — smaller LTP gap cluster, distilled to deterministic oracle-diffed probes. Each guest
//! prints fixed strings only (booleans/errno-names, never raw fds/pids/addresses/cpu-counts) so the
//! `.oracle()` check holds dd byte-identical to native (aarch64) / qemu (x86_64) on BOTH Linux engines.
//! Covers dup/dup2/dup3/fcntl flag semantics, link/lstat, socket error paths (incl. the connect EFAULT
//! crash), and prctl/nanosleep/sched_getaffinity/read error paths.

use dd_tests::{group, src, Group};

pub fn groups() -> Vec<Group> {
    vec![group(
        "ltpgaps",
        vec![
            // dup03/dup201/fcntl05/fcntl13: dup2 oldfd==newfd, dup2-over-open, dup clears cloexec,
            // F_DUPFD(_CLOEXEC) floor, F_GETFD/F_SETFD, F_GETFL/F_SETFL status flags.
            src("dupfcntl", "ltp_dupfcntl.c").oracle(),
            // link02/link05/lstat01/lstat02: link nlink/shared-content/EEXIST/ENOENT, lstat-on-symlink.
            src("linkstat", "ltp_linkstat.c").oracle(),
            // pause01/pause02/mincore04/fork04 setup: LTP tst_checkpoint over a MAP_SHARED /dev/shm page +
            // cross-process futex. FUTEX_WAKE must return the ACTUAL waiters woken (not the requested max),
            // else tst_checkpoint_wake spins to ETIMEDOUT and BROKs setup (shared root cause).
            src("checkpoint", "ltp_checkpoint.c").oracle(),
            // connect01/bind01/sendto02: EBADF/ENOTSOCK/EFAULT/EINVAL error paths. dd re-derives the Linux
            // errno + ORDER in net_precheck() (net.c): the fd is validated BEFORE the sockaddr is read, exactly
            // as the real kernel does (fdget -> EBADF before move_addr_to_kernel). The guard's EBADF case pairs
            // the bad fd with a VALID address (like LTP connect01) so it is portable; a bad-fd+bad-addr combo is
            // order-ambiguous (qemu-user copies the addr first -> EFAULT) and is deliberately not asserted.
            // resolved: dd byte-exact vs native on BOTH arches; connect01/bind01 LTP binaries pass 7/7 each.
            src("neterr", "ltp_neterr.c").oracle(),
            // prctl02/prctl03/nanosleep02/sched_getaffinity01/read02: option/flag + error-path fidelity.
            src("procmisc", "ltp_procmisc.c").oracle(),
            // fstatat01/statx03/symlinkat01/linkat01/renameat201/unlink07: the *at dirfd/flag/EFAULT/EXDEV
            // error-path surface. dd folded a bad/non-dir dirfd into a host path via g_fdpath (missing EBADF/
            // ENOTDIR), ignored invalid flag/mask bits (missing EINVAL), read a PROT_NONE path as "" (ENOENT not
            // EFAULT), and ENOENT'd a pseudo-fs hardlink (not EXDEV). All standard kernel paths -> oracle-diffed.
            src("aterr", "ltp_aterr.c").oracle(),
        ],
    )]
}
