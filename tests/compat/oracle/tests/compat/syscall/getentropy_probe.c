// getentropy(3): fills exactly n (<=256) bytes or fails entirely; two calls differ; n>256 -> EIO. glibc
// implements getentropy over getrandom(2), so this locks the same host-RNG plumbing at the libc contract.
// Only stable booleans printed -> native == engine byte-for-byte.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    unsigned char a[256] = {0}, b[256] = {0};

    int ok256 = getentropy(a, 256) == 0;
    int ok_b = getentropy(b, 256) == 0;
    int differ = memcmp(a, b, 256) != 0;

    // Small exact fill.
    unsigned char s[32] = {0};
    int ok32 = getentropy(s, 32) == 0;

    // Not all bytes equal across the 256-byte block (gross-failure detection).
    int all_equal = 1;
    for (int i = 1; i < 256; i++)
        if (a[i] != a[0]) { all_equal = 0; break; }

    // n > 256 is rejected entirely with EIO (glibc userspace check).
    errno = 0;
    unsigned char big[257] = {0};
    int eio = (getentropy(big, 257) == -1) && (errno == EIO);

    printf("ok256=%d ok_b=%d ok32=%d differ=%d all_equal=%d eio=%d\n", ok256, ok_b, ok32, differ, all_equal,
           eio);
    return 0;
}
