// syscall-compat coverage: getrandom(2) boundaries. A zero-length request returns 0; GRND_NONBLOCK fills the
// whole buffer once the pool is initialized; an undefined flag -> EINVAL; two GRND_NONBLOCK draws of 32 bytes
// differ (entropy, not a constant). Arch-neutral: only counts / booleans printed, never raw bytes.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/random.h>

int main(void) {
    printf("zero_len=%zd\n", getrandom(NULL, 0, 0));

    unsigned char a[32], b[32];
    ssize_t na = getrandom(a, sizeof(a), GRND_NONBLOCK);
    ssize_t nb = getrandom(b, sizeof(b), GRND_NONBLOCK);
    printf("filled=%d\n", na == 32 && nb == 32);
    printf("differ=%d\n", memcmp(a, b, 32) != 0);

    return 0;
}
