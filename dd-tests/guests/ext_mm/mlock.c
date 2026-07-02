// mlock/munlock and mlockall/munlockall on a small anon region. Locking may be denied (EPERM/ENOMEM
// under an RLIMIT_MEMLOCK cap) -> we accept "locked or denied", asserting only that the call is
// HONORED (0 or a sane errno) and never crashes, and that the page stays readable/writable.
// Portable POSIX -> golden verdict on every engine.
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static int honored(int rc) {
    return rc == 0 || errno == EPERM || errno == ENOMEM || errno == EAGAIN || errno == ENOSYS;
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    size_t len = ps * 2;
    char *m = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    memset(m, 1, len);
    errno = 0;
    int lock = honored(mlock(m, len));
    m[0] = 42;                                  // locked page must stay usable
    int usable = m[0] == 42;
    errno = 0;
    int unlock = honored(munlock(m, len));
    // mlockall/munlockall are privileged and unimplemented on some hosts (notably macOS): accept a
    // sane errno as "honored" so the case stays portable while still exercising the calls.
    errno = 0;
    int lockall = honored(mlockall(MCL_CURRENT));
    errno = 0;
    int unlockall = honored(munlockall());
    munmap(m, len);
    printf("mlock lock=%d usable=%d unlock=%d lockall=%d unlockall=%d\n",
           lock, usable, unlock, lockall, unlockall);
    return 0;
}
