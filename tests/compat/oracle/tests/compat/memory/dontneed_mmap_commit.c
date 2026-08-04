#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

/* Sibling of dontneed_commit.c for the OTHER commit idiom: reserve NONE ->
 * mmap(MAP_FIXED, RW) the subrange -> store -> DONTNEED -> loads read fresh
 * zeros and stores still succeed.  This is Go's heap pattern (sysReserve
 * PROT_NONE, sysMap MAP_FIXED RW, scavenger MADV_DONTNEED).  A private-anon
 * registry that only APPENDS the re-commit record leaves the stale PROT_NONE
 * reservation record shadowing it (first-match), so the DONTNEED emulation
 * re-establishes live heap as an inaccessible reservation and the guest's
 * next access faults (the go-build memclr SIGSEGV class). */
int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 1;
    size_t commit = (size_t)page * 16;
    unsigned char *reserved =
        mmap(NULL, (size_t)page * 64, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reserved == MAP_FAILED) return 2;
    if (mmap(reserved, commit, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1,
             0) != (void *)reserved)
        return 3;
    memset(reserved, 'A', commit);
    if (madvise(reserved, commit, MADV_DONTNEED) != 0) return 4;
    if (memchr(reserved, 'A', commit) != NULL) {
        printf("dontneed_mmap_commit=stale\n");
        return 5;
    }
    reserved[7] = 0x5a; /* the re-committed pages must still accept stores */
    printf("dontneed_mmap_commit=%02x zero=%02x\n", reserved[7], reserved[8]);
    return munmap(reserved, (size_t)page * 64) == 0 ? 0 : 6;
}
