// fork() COW on private-anon plus MADV_DONTNEED in the child. The child sees the inherited bytes, then
// DONTNEED re-zeroes ONLY the child's copy; the parent's pages are untouched. Locks the copy-on-write +
// per-process re-zero semantics that the engine's private-anon tracking must preserve across fork.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    unsigned char *m = mmap(NULL, ps * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    memset(m, 0x77, ps * 2);

    pid_t pid = fork();
    if (pid == 0) {
        int inherited = m[0] == 0x77 && m[ps] == 0x77;
        int rc = madvise(m, ps * 2, MADV_DONTNEED);
        int rezeroed = m[0] == 0 && m[ps] == 0;
        m[0] = 0x9a;                        // write after re-zero
        int rewrite = m[0] == 0x9a;
        _exit((inherited && rc == 0 && rezeroed && rewrite) ? 0 : 1);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    int child_ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int parent_keeps = m[0] == 0x77 && m[ps] == 0x77;   // parent unaffected by child DONTNEED

    munmap(m, ps * 2);
    printf("child_ok=%d parent_keeps=%d\n", child_ok, parent_keeps);
    return 0;
}
