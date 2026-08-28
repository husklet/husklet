#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <sys/mman.h>

typedef void *(*mmap_fn)(void *, size_t, int, int, int, off_t);

void *mmap(void *address, size_t length, int protection, int flags, int descriptor, off_t offset) {
    static mmap_fn real_mmap;
    static _Atomic int failed;
    if (real_mmap == NULL) real_mmap = (mmap_fn)dlsym(RTLD_NEXT, "mmap");
    if (getenv("HL_TEST_FAIL_SHARED_ANON") != NULL && (flags & MAP_SHARED) != 0 &&
        (flags & MAP_ANONYMOUS) != 0 && atomic_exchange_explicit(&failed, 1, memory_order_relaxed) == 0) {
        errno = ENOMEM;
        return MAP_FAILED;
    }
    if (real_mmap == NULL) {
        errno = ENOMEM;
        return MAP_FAILED;
    }
    return real_mmap(address, length, protection, flags, descriptor, offset);
}
