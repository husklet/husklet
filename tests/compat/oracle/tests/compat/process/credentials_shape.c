// Credential-query CONTRACT (shapes, not host-specific id values) fixed by the Linux ABI:
//   - getresuid/getresgid succeed and report real==effective==saved for a normally-launched process.
//   - getgroups(-1, ...) is EINVAL; getgroups(0, NULL) returns the count without faulting.
//   - setuid/setgid to the process's OWN current id always succeeds (no privilege needed).
//   - setreuid(-1,-1)/setregid(-1,-1) are no-ops that succeed.
// No absolute uid/gid numbers are printed, so the golden is host-invariant. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/types.h>
#include <unistd.h>

int main(void) {
    uid_t r, e, s;
    gid_t gr, ge, gs;
    getresuid(&r, &e, &s);
    getresgid(&gr, &ge, &gs);
    printf("uid_all_equal=%d gid_all_equal=%d\n", (r == e && e == s), (gr == ge && ge == gs));

    // getgroups(-1) -> EINVAL.
    errno = 0;
    printf("getgroups_neg_errno=%d\n", getgroups(-1, NULL) == -1 ? errno : 0);
    // getgroups(0, NULL) returns the count (>= 0) without EFAULT.
    errno = 0;
    printf("getgroups_count_ok=%d\n", getgroups(0, NULL) >= 0);

    // Setting our own current ids is always allowed.
    printf("setuid_self_ok=%d setgid_self_ok=%d\n", setuid(getuid()) == 0, setgid(getgid()) == 0);
    // -1 sentinels are no-ops that succeed.
    printf("setreuid_noop_ok=%d setregid_noop_ok=%d\n", setreuid(-1, -1) == 0, setregid(-1, -1) == 0);
    return 0;
}
