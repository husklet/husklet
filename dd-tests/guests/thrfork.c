// #371: fork from a THREADED parent -- exercises the conservative REBUILD branch of jit_after_fork
// (g_threaded set -> the child cannot trust the inherited arena and builds a fresh dual map). The
// specific regression this guards: the old RX range is a HOLE in the child (VM_INHERIT_NONE), the fresh
// arena may be allocated inside that gap, and a stray munmap of the old RX range would unmap the brand
// new cache -> every child SIGSEGVs on its first translation and all 50 exit codes collapse to 0.
#include <stdio.h>
#include <pthread.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/wait.h>
static volatile int stop;
static void *spin(void *a) { // peer thread translating/executing while main forks
    volatile uint64_t x = 1;
    while (!stop) { x = x * 6364136223846793005ull + 1442695040888963407ull; }
    return (void *)(uintptr_t)x;
}
int main(void) {
    pthread_t th;
    pthread_create(&th, NULL, spin, NULL);
    long sum = 0;
    for (int i = 0; i < 50; i++) {
        pid_t p = fork();
        if (p == 0) _exit(i & 63);
        int st; waitpid(p, &st, 0);
        sum += WEXITSTATUS(st);
    }
    stop = 1;
    pthread_join(th, NULL);
    printf("thrfork sum=%ld\n", sum);
    return 0;
}
