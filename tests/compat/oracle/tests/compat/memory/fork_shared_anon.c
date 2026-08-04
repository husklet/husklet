// MAP_SHARED|MAP_ANONYMOUS is inherited as a genuinely shared segment across fork: a child store is
// observed by the parent after the child exits. Distinct from the private-anon COW path.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    unsigned char *m = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    m[0] = 0x01;

    pid_t pid = fork();
    if (pid == 0) {
        int saw = m[0] == 0x01;
        m[0] = 0xbe;              // visible to parent because the page is shared
        m[ps - 1] = 0xef;
        _exit(saw ? 0 : 1);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    int child_ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int parent_sees = m[0] == 0xbe && m[ps - 1] == 0xef;

    munmap(m, ps);
    printf("child_ok=%d parent_sees=%d\n", child_ok, parent_sees);
    return 0;
}
