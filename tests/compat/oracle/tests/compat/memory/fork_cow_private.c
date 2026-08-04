// Classic private-anon copy-on-write across fork: child mutations are invisible to the parent and vice
// versa after the fork point, while pre-fork bytes are shared. Confirms the engine does not accidentally
// share writable private pages between parent and child.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    unsigned char *m = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    m[0] = 0x10; m[1] = 0x20;

    pid_t pid = fork();
    if (pid == 0) {
        int saw = m[0] == 0x10 && m[1] == 0x20;   // inherited pre-fork values
        m[0] = 0xee;                              // COW: parent must not observe this
        int child_local = m[0] == 0xee;
        _exit((saw && child_local) ? 0 : 1);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    int child_ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int parent_unchanged = m[0] == 0x10;

    munmap(m, ps);
    printf("child_ok=%d parent_unchanged=%d\n", child_ok, parent_unchanged);
    return 0;
}
