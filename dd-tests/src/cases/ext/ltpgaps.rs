//! Task #398 — smaller LTP gap cluster, distilled to deterministic oracle-diffed probes. Each guest
//! prints fixed strings only (booleans/errno-names, never raw fds/pids/addresses/cpu-counts) so the
//! `.oracle()` check holds dd byte-identical to native (aarch64) / qemu (x86_64) on BOTH Linux engines.
//! Covers dup/dup2/dup3/fcntl flag semantics, link/lstat, socket error paths (incl. the connect EFAULT
//! crash), and prctl/nanosleep/sched_getaffinity/read error paths.

use crate::{group, src, Group};

pub fn groups() -> Vec<Group> {
    vec![group("ltpgaps", vec![
        // dup03/dup201/fcntl05/fcntl13: dup2 oldfd==newfd, dup2-over-open, dup clears cloexec,
        // F_DUPFD(_CLOEXEC) floor, F_GETFD/F_SETFD, F_GETFL/F_SETFL status flags.
        src("dupfcntl", "ltp_dupfcntl.c").oracle(),
        // link02/link05/lstat01/lstat02: link nlink/shared-content/EEXIST/ENOENT, lstat-on-symlink.
        src("linkstat", "ltp_linkstat.c").oracle(),
        // pause01/pause02/mincore04/fork04 setup: LTP tst_checkpoint over a MAP_SHARED /dev/shm page +
        // cross-process futex. FUTEX_WAKE must return the ACTUAL waiters woken (not the requested max),
        // else tst_checkpoint_wake() spins to ETIMEDOUT and BROKs setup (#402/#400 shared root cause).
        src("checkpoint", "ltp_checkpoint.c").oracle(),
        // connect01/bind01/sendto02: EBADF/ENOTSOCK/EFAULT/EINVAL error paths (connect EFAULT = the crash).
        src("neterr", "ltp_neterr.c").oracle(),
        // prctl02/prctl03/nanosleep02/sched_getaffinity01/read02: option/flag + error-path fidelity.
        src("procmisc", "ltp_procmisc.c").oracle(),
    ])]
}
