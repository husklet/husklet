// #371 fork-storm stress: 1000 sequential forks in five mixed patterns, checksummed. Exercises the
// preserved-arena fork path (single-threaded parent) end to end:
//   i%5==0  fork + immediate _exit                    (pure fork/reap churn)
//   i%5==1  fork + WARM work                          (child executes inherited translations)
//   i%5==2  fork + COLD kernel                        (child TRANSLATES NEW CODE after fork -- proves the
//                                                      re-coupled RW/RX aliases: fresh emission must be
//                                                      visible to the execute alias or this crashes)
//   i%5==3  fork + execve(self)                       (in-process exec teardown after a preserved fork)
//   i%5==4  fork + NESTED fork (grandchild runs cold) (generation-2 re-remap of the re-remapped arena)
// The parent folds every exit code into one deterministic checksum -> golden on both Linux engines.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <sys/wait.h>

static uint32_t xs(uint32_t s, int n) {
    while (n--) { s ^= s << 13; s ^= s >> 17; s ^= s << 5; }
    return s ? s : 1;
}
// 32 DISTINCT cold kernels (separate functions -> separate guest code addresses). The parent never calls
// them, so the FIRST call happens in a fork child and forces a post-fork translation into the arena.
#define K(i) __attribute__((noinline)) static uint32_t cold##i(uint32_t s) { return xs(s + i, 1000 + 17 * i); }
K(0) K(1) K(2) K(3) K(4) K(5) K(6) K(7) K(8) K(9) K(10) K(11) K(12) K(13) K(14) K(15)
K(16) K(17) K(18) K(19) K(20) K(21) K(22) K(23) K(24) K(25) K(26) K(27) K(28) K(29) K(30) K(31)
static uint32_t (*coldtab[32])(uint32_t) = {
    cold0,  cold1,  cold2,  cold3,  cold4,  cold5,  cold6,  cold7,  cold8,  cold9,  cold10, cold11,
    cold12, cold13, cold14, cold15, cold16, cold17, cold18, cold19, cold20, cold21, cold22, cold23,
    cold24, cold25, cold26, cold27, cold28, cold29, cold30, cold31,
};

static int reap(pid_t p) {
    int st = 0;
    if (waitpid(p, &st, 0) != p || !WIFEXITED(st)) return -1;
    return WEXITSTATUS(st);
}

int main(int argc, char **argv) {
    if (argc == 3 && !strcmp(argv[1], "-c")) return atoi(argv[2]) & 63; // the exec'd self-image
    long sum = 0, reaped = 0;
    for (int i = 0; i < 1000; i++) {
        pid_t p = fork();
        if (p < 0) { printf("forkstorm fork_fail@%d\n", i); return 1; }
        if (p == 0) {
            switch (i % 5) {
            case 0: _exit(i & 63);
            case 1: _exit((int)(xs((uint32_t)i + 1, 5000) & 63));
            case 2: _exit((int)(coldtab[i % 32]((uint32_t)i) & 63));
            case 3: {
                char nb[16];
                snprintf(nb, sizeof nb, "%d", i);
                execl(argv[0], argv[0], "-c", nb, (char *)NULL);
                _exit(61); // exec failed -- perturbs the checksum
            }
            default: {
                pid_t g = fork();
                if (g == 0) _exit((int)(coldtab[(i + 7) % 32](xs((uint32_t)i + 3, 100)) & 31));
                int gc = reap(g);
                _exit(gc < 0 ? 62 : (gc + (i & 31)) & 63);
            }
            }
        }
        int c = reap(p);
        if (c < 0) { printf("forkstorm reap_fail@%d\n", i); return 1; }
        sum += c;
        reaped++;
    }
    printf("forkstorm reaped=%ld sum=%ld\n", reaped, sum);
    return 0;
}
