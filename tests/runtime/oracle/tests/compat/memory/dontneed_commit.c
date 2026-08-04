#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

/* A committed subrange of a PROT_NONE reservation must keep its committed
 * protection across MADV_DONTNEED: reserve NONE -> mprotect RW -> store ->
 * DONTNEED -> loads read fresh zeros and stores still succeed (jemalloc purge
 * inside rustc, mozjs/V8 GC chunks).  A DONTNEED emulation that re-establishes
 * the range with the stale mmap-time PROT_NONE turns the committed pages back
 * into an inaccessible reservation and the next access faults. */
int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 1;
    size_t commit = (size_t)page * 16;
    unsigned char *reserved =
        mmap(NULL, (size_t)page * 64, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reserved == MAP_FAILED) return 2;
    if (mprotect(reserved, commit, PROT_READ | PROT_WRITE) != 0) return 3;
    memset(reserved, 'A', commit);
    if (madvise(reserved, commit, MADV_DONTNEED) != 0) return 4;
    if (memchr(reserved, 'A', commit) != NULL) {
        printf("dontneed_commit=stale\n");
        return 5;
    }
    reserved[7] = 0x5a; /* the committed pages must still accept stores */
    printf("dontneed_commit=%02x zero=%02x\n", reserved[7], reserved[8]);
    return munmap(reserved, (size_t)page * 64) == 0 ? 0 : 6;
}
