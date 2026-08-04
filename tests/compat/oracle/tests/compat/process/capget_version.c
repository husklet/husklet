// capget/capset version negotiation is a fixed-ABI contract: a call with version 0 (or any
// unsupported value) fails EINVAL and the kernel rewrites the header's version field to the
// preferred one it does support (_LINUX_CAPABILITY_VERSION_3, 0x20080522). A well-formed v3
// capget succeeds, and a capset with a bad version is EINVAL. Errno/constant classes only, which
// the engine must emulate to Linux regardless of host backend (macOS has no capabilities at all).
#define _GNU_SOURCE
#include <errno.h>
#include <linux/capability.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    struct __user_cap_header_struct h;
    struct __user_cap_data_struct d[2];

    // Version 0 -> EINVAL, header rewritten to the preferred version.
    h.version = 0; h.pid = 0;
    errno = 0;
    int v0_einval = syscall(SYS_capget, &h, d) == -1 && errno == EINVAL;
    int pref_v3 = h.version == _LINUX_CAPABILITY_VERSION_3;

    // Bogus version -> EINVAL as well.
    h.version = 0xdeadbeef; h.pid = 0;
    errno = 0;
    int badver_einval = syscall(SYS_capget, &h, d) == -1 && errno == EINVAL;

    // Well-formed v3 query succeeds.
    h.version = _LINUX_CAPABILITY_VERSION_3; h.pid = 0;
    memset(d, 0, sizeof d);
    int v3_ok = syscall(SYS_capget, &h, d) == 0;

    // capset with a bad version -> EINVAL (before any privilege check).
    h.version = 0; h.pid = 0;
    errno = 0;
    int capset_badver = syscall(SYS_capset, &h, d) == -1 && errno == EINVAL;

    printf("capget-version v0=%d pref_v3=%d badver=%d v3_ok=%d capset_badver=%d\n",
           v0_einval, pref_v3, badver_einval, v3_ok, capset_badver);
    return 0;
}
