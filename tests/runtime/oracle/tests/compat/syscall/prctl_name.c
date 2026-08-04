// syscall-compat coverage: prctl name and misc. PR_SET_NAME / PR_GET_NAME round-trip a thread name;
// PR_GET_KEEPCAPS defaults to 0; an unknown
// prctl option -> EINVAL. Arch-neutral: booleans/errnos and the (fixed) round-tripped name printed.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>

int main(void) {
    prctl(PR_SET_NAME, "hl-probe", 0, 0, 0);
    char name[16] = {0};
    prctl(PR_GET_NAME, name, 0, 0, 0);
    printf("name_match=%d name=%s\n", strcmp(name, "hl-probe") == 0, name);

    printf("keepcaps=%ld\n", prctl(PR_GET_KEEPCAPS, 0, 0, 0, 0));

    printf("badopt_errno=%d\n", prctl(999999, 0, 0, 0, 0) == -1 ? errno : 0);
    return 0;
}
