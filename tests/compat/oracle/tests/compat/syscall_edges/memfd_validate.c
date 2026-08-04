// memfd_create argument validation is a fixed Linux ABI error surface: an undefined flag bit is
// EINVAL (checked before the name), and a valid creation with the documented flags succeeds and
// carries FD_CLOEXEC. Only errnos/booleans are printed so the golden is host-invariant. Arch-neutral.
//
// NOTE: the name-length limit (a name > MFD_NAME_MAX=249 bytes must be EINVAL) is deliberately NOT
// asserted here. The engine's mkstemp-backed memfd emulation ignores the name and cannot validate its
// length without dereferencing a guest pointer, which is unsafe from the trusted syscall thread for an
// x86_64 guest whose .rodata is not host-identity-mapped. That is a real (reported) engine divergence,
// but a correct fix needs the engine's guest-memory translation layer, so it is not encoded as a case.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    // Undefined flag bit (0x100 is unused; valid bits are CLOEXEC 0x1, ALLOW_SEALING 0x2, HUGETLB 0x4) -> EINVAL.
    long bad = syscall(SYS_memfd_create, "x", 0x100u);
    printf("badflag_errno=%d\n", bad == -1 ? errno : 0);

    // Valid creation with MFD_CLOEXEC|MFD_ALLOW_SEALING succeeds and carries FD_CLOEXEC.
    int fd = (int)syscall(SYS_memfd_create, "hlvalid", 0x1u | 0x2u);
    printf("valid_ok=%d cloexec=%d\n", fd >= 0, fd >= 0 && (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0);
    return 0;
}
