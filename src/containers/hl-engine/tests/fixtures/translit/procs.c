// translit/procs -- %gs republication across every way a guest acquires a new struct cpu.
//
// struct cpu is reached through the %gs base, set per thread with arch_prctl(ARCH_SET_GS). run_block
// republishes whenever it sees a cpu it has not published for, and a cloned thread, a fork child and an
// exec'd image all have to land there. Threads, fork, vfork+execve, raw clone, and a final execve out of
// a transliterated frame.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <pthread.h>
#include <sched.h>
#include <sys/wait.h>
#include <sys/mman.h>

static uint64_t mix(uint64_t x) {
    x ^= x >> 33;
    x *= 0xff51afd7ed558ccdull;
    x ^= x >> 33;
    return x;
}

static _Atomic unsigned long shared;

static void *worker(void *arg) {
    uint64_t seed = (uint64_t)(uintptr_t)arg, h = seed;
    for (uint64_t i = 0; i < 200000; i++) {
        h = mix(h + i);
        shared += (h & 1);
    }
    return (void *)(uintptr_t)h;
}

static int clone_child(void *arg) {
    (void)arg;
    _exit(37);
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0); // unbuffered: the ordering of a forked child\'s output is part of the comparison
    // Re-exec target: the fixture itself, so execve does not depend on any path in the guest
    // filesystem being reachable from the launch configuration under test.
    if (argc > 1) {
        printf("exec ok\n");
        return 0;
    }
    // threads: each guest thread needs its own %gs published
    pthread_t t[8];
    uint64_t acc = 0;
    for (int i = 0; i < 8; i++)
        pthread_create(&t[i], NULL, worker, (void *)(uintptr_t)(i + 1));
    for (int i = 0; i < 8; i++) {
        void *r;
        pthread_join(t[i], &r);
        acc = acc * 131 + (uint64_t)(uintptr_t)r;
    }
    printf("threads acc=%016llx shared_parity=%lu\n", (unsigned long long)acc, (unsigned long)(shared & 1));
    // fork: the child must re-publish %gs for its own cpu
    int total = 0;
    for (int i = 0; i < 8; i++) {
        pid_t p = fork();
        if (p == 0) {
            uint64_t h = mix(i);
            for (int k = 0; k < 50000; k++)
                h = mix(h + k);
            _exit((int)(h & 0x3f));
        }
        int st;
        waitpid(p, &st, 0);
        total += WEXITSTATUS(st);
    }
    printf("fork total=%d\n", total);
    // vfork + execve, into this same static-PIE image
    for (int i = 0; i < 4; i++) {
        pid_t p = vfork();
        if (p == 0) {
            execl(argv[0], argv[0], "child", (char *)NULL);
            _exit(99);
        }
        int st;
        waitpid(p, &st, 0);
        total += WEXITSTATUS(st);
    }
    printf("vfork total=%d\n", total);
    // raw clone
    char *stack = mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    int cs = 0;
    for (int i = 0; i < 4; i++) {
        pid_t p = clone(clone_child, stack + (1 << 20), SIGCHLD, NULL);
        if (p < 0) {
            cs = -1;
            break;
        }
        int st;
        waitpid(p, &st, 0);
        cs += WEXITSTATUS(st);
    }
    printf("clone total=%d\n", cs);
    // execve from a transliterated frame, tail of the process
    fflush(stdout);
    execl(argv[0], argv[0], "child", (char *)NULL);
    return 42;
}
