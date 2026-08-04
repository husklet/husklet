// explicit_bzero / bzero / memset zeroing that must not be optimised away. Portable verdicts.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <strings.h>

int main(void) {
    char secret[32]; memset(secret, 0x55, sizeof secret);
    explicit_bzero(secret, sizeof secret);
    int d1 = 1; for (size_t i = 0; i < sizeof secret; i++) if (secret[i]) d1 = 0;
    char b[16]; memset(b, 'x', sizeof b); bzero(b, 8);
    int d2 = b[0] == 0 && b[7] == 0 && b[8] == 'x';
    printf("explicit_bzero d1=%d d2=%d\n", d1, d2);
    return 0;
}
