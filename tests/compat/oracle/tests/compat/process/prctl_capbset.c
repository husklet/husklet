// prctl(PR_CAPBSET_READ) range validation is a fixed-ABI contract: an out-of-range capability
// number is EINVAL, and an in-range query never errors (it returns 0 or 1 depending on the
// bounding set, so the verdict only asserts "did not error"). PR_GET_NO_NEW_PRIVS defaults to 0
// and PR_SET_NO_NEW_PRIVS is a monotone latch that reads back 1. Errno/boolean classes only,
// which the engine must emulate to Linux regardless of host backend.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/prctl.h>

int main(void) {
    // CAP_CHOWN (0) is always a valid index: the read must not error (-1).
    int in_range = prctl(PR_CAPBSET_READ, 0, 0, 0, 0);
    int in_range_ok = in_range == 0 || in_range == 1;

    // An out-of-range capability number is EINVAL.
    errno = 0;
    int oor = prctl(PR_CAPBSET_READ, 200, 0, 0, 0);
    int oor_einval = oor == -1 && errno == EINVAL;

    // PR_SET_NO_NEW_PRIVS latch.
    int nnp_before = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
    int nnp_set = prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0;
    int nnp_after = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);

    printf("prctl-capbset in_range_ok=%d oor_einval=%d nnp_before=%d nnp_set=%d nnp_after=%d\n",
           in_range_ok, oor_einval, nnp_before, nnp_set, nnp_after);
    return 0;
}
