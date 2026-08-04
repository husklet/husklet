// MADV_DONTFORK marks a range so a fork(2) child does NOT inherit it (a child access faults), while the
// parent keeps its data; MADV_DOFORK undoes the marking so a later fork inherits again. Realized in the
// engine's own fork path (the child unmaps the marked ranges), since a guest fork re-establishes the
// child's guest memory rather than relying on host VMA inheritance.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef MADV_DONTFORK
#define MADV_DONTFORK 10
#endif
#ifndef MADV_DOFORK
#define MADV_DOFORK 11
#endif

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);

    // DONTFORK: parent retains its byte; the child faults on access.
    volatile unsigned char *a = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (a == MAP_FAILED) return 1;
    a[0] = 0x5c;
    int adv_ok = madvise((void *)a, ps, MADV_DONTFORK) == 0;
    pid_t p1 = fork();
    if (p1 == 0) {
        volatile unsigned char x = a[0]; // must fault (range not present in the child)
        _exit((int)x);                   // only reached if it wrongly inherited
    }
    int st1 = 0;
    waitpid(p1, &st1, 0);
    int child_faults = WIFSIGNALED(st1);
    int parent_keeps = a[0] == 0x5c;

    // DOFORK: undo the marking -> a later fork inherits the range again.
    volatile unsigned char *b = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (b == MAP_FAILED) return 1;
    b[0] = 0x42;
    madvise((void *)b, ps, MADV_DONTFORK);
    madvise((void *)b, ps, MADV_DOFORK);
    pid_t p2 = fork();
    if (p2 == 0) _exit(b[0] == 0x42 ? 66 : 1);
    int st2 = 0;
    waitpid(p2, &st2, 0);
    int dofork_inherits = WIFEXITED(st2) && WEXITSTATUS(st2) == 66;

    printf("madv_dontfork adv_ok=%d child_faults=%d parent_keeps=%d dofork_inherits=%d\n", adv_ok,
           child_faults, parent_keeps, dofork_inherits);
    munmap((void *)a, ps);
    munmap((void *)b, ps);
    return adv_ok && child_faults && parent_keeps && dofork_inherits ? 0 : 1;
}
