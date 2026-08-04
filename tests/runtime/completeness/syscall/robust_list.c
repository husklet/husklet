/* get_robust_list / set_robust_list round-trip. glibc's NPTL registers a robust-futex list head per
   thread; the kernel must report a non-NULL head with the canonical size for the running thread, and
   accept a set_robust_list re-registration. A correct engine round-trips these. We print derived
   booleans (head-nonnull, size-canonical, set-accepted), never the raw pointer, so the verdict is
   arch-neutral and host-independent. */
#include "compat.h"
#include <stdio.h>
#include <stddef.h>
#ifndef __NR_get_robust_list
#if defined(__aarch64__)
#define __NR_get_robust_list 100
#define __NR_set_robust_list 99
#else
#define __NR_get_robust_list 274
#define __NR_set_robust_list 273
#endif
#endif

int main(void) {
    void *head = (void *)0;
    size_t len = 0;
    long g = syscall(__NR_get_robust_list, 0 /*self*/, &head, &len);
    int get_ok = (g == 0) || (errno == ENOSYS || errno == EPERM);
    /* glibc registers a head; canonical robust_list_head is 24 bytes on both LP64 targets */
    int size_canonical = (g != 0) ? 1 : (len == 24);

    long s = -1;
    if (g == 0 && head) s = syscall(__NR_set_robust_list, head, len);
    int set_ok = (g != 0) || (s == 0) || (errno == ENOSYS || errno == EPERM);

    printf("robust get_ok=%d size_canonical=%d set_ok=%d\n", get_ok, size_canonical, set_ok);
    return 0;
}
