// clone(CLONE_VM | CLONE_VFORK | SIGCHLD): the child runs on its own stack but shares the parent's
// address space, and CLONE_VFORK suspends the parent until the child exits. A value the child stores
// into a shared heap word is therefore visible to the parent the instant it resumes -- deterministic.
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile int *shared;

static int child_fn(void *arg) {
    int base = *(int *)arg;
    *shared = base + 100;      // store visible to the parent (shared VM)
    _exit(0);
}

int main(void) {
    shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (shared == MAP_FAILED) { printf("mmap fail\n"); return 1; }
    *shared = 0;

    size_t ss = 64 * 1024;
    void *stack = mmap(NULL, ss, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    if (stack == MAP_FAILED) { printf("mmap2 fail\n"); return 1; }

    int seed = 23;
    pid_t pid = clone(child_fn, (char *)stack + ss, CLONE_VM | CLONE_VFORK | SIGCHLD, &seed);
    if (pid < 0) { printf("clone fail\n"); return 1; }

    // Parent has resumed only after child exit (VFORK): the store must already be visible.
    int seen_after_resume = (*shared == 123);
    int st = 0;
    int reaped = waitpid(pid, &st, 0) == pid && WIFEXITED(st) && WEXITSTATUS(st) == 0;

    printf("clone_vm_vfork seen=%d reaped=%d\n", seen_after_resume, reaped);
    munmap(stack, ss);
    munmap((void *)shared, 4096);
    return 0;
}
