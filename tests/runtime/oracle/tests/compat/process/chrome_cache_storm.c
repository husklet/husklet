#define _GNU_SOURCE

#include <linux/sched.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

/*
 * Chrome-shaped process churn: a historically threaded parent repeatedly
 * creates no-exec fork and clone3 children, interleaved with fork+exec. The
 * no-exec children run a parent-prewarmed code corpus, so translation logs
 * expose whether the inherited working set was reused or rebuilt.
 */
#define LEAF(n)                                                                                                        \
    __attribute__((noinline)) static uint64_t leaf##n(uint64_t value) {                                                \
        return (value ^ (UINT64_C(0x9e3779b97f4a7c15) + n)) * (UINT64_C(0x100000001b3) + 2 * n);                       \
    }
LEAF(0)
LEAF(1) LEAF(2) LEAF(3) LEAF(4) LEAF(5) LEAF(6) LEAF(7) LEAF(8) LEAF(9) LEAF(10) LEAF(11) LEAF(12) LEAF(13) LEAF(14)
    LEAF(15) LEAF(16) LEAF(17) LEAF(18) LEAF(19) LEAF(20) LEAF(21) LEAF(22) LEAF(23) LEAF(24) LEAF(25) LEAF(26) LEAF(27)
        LEAF(28) LEAF(29) LEAF(30) LEAF(31) LEAF(32) LEAF(33) LEAF(34) LEAF(35) LEAF(36) LEAF(37) LEAF(38) LEAF(39)
            LEAF(40) LEAF(41) LEAF(42) LEAF(43) LEAF(44) LEAF(45) LEAF(46) LEAF(47) LEAF(48) LEAF(49) LEAF(50) LEAF(51)
                LEAF(52) LEAF(53) LEAF(54) LEAF(55) LEAF(56) LEAF(57) LEAF(58) LEAF(59) LEAF(60) LEAF(61) LEAF(62)
                    LEAF(63)

                        typedef uint64_t (*leaf_fn)(uint64_t);
static leaf_fn const leaves[] = {
    leaf0,  leaf1,  leaf2,  leaf3,  leaf4,  leaf5,  leaf6,  leaf7,  leaf8,  leaf9,  leaf10, leaf11, leaf12,
    leaf13, leaf14, leaf15, leaf16, leaf17, leaf18, leaf19, leaf20, leaf21, leaf22, leaf23, leaf24, leaf25,
    leaf26, leaf27, leaf28, leaf29, leaf30, leaf31, leaf32, leaf33, leaf34, leaf35, leaf36, leaf37, leaf38,
    leaf39, leaf40, leaf41, leaf42, leaf43, leaf44, leaf45, leaf46, leaf47, leaf48, leaf49, leaf50, leaf51,
    leaf52, leaf53, leaf54, leaf55, leaf56, leaf57, leaf58, leaf59, leaf60, leaf61, leaf62, leaf63,
};

static _Atomic int stop_workers;
static _Atomic unsigned workers_ready;
static int blocked_pipe[2];

static uint64_t corpus(uint64_t value) {
    for (size_t round = 0; round < 4; ++round)
        for (size_t index = 0; index < sizeof leaves / sizeof leaves[0]; ++index)
            value = leaves[index](value + round);
    return value;
}

static void *spinner(void *argument) {
    uint64_t value = (uintptr_t)argument + 1;
    atomic_fetch_add_explicit(&workers_ready, 1, memory_order_release);
    while (!atomic_load_explicit(&stop_workers, memory_order_acquire))
        value = corpus(value);
    return (void *)(uintptr_t)(value | 1);
}

static void *blocked_reader(void *unused) {
    char byte;
    (void)unused;
    atomic_fetch_add_explicit(&workers_ready, 1, memory_order_release);
    return (void *)(uintptr_t)(read(blocked_pipe[0], &byte, 1) == 1 && byte == 'x' ? 1 : 0);
}

static int reap(pid_t child, int expected) {
    int status = 0;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == expected;
}

static int child_value(int round) {
    return (int)(corpus((uint64_t)round + 17) & 63);
}

static void child_done(const char *kind, int value) {
    dprintf(STDERR_FILENO, "[cache-reuse] kind=%s\n", kind);
    _exit(value);
}

int main(int argc, char **argv) {
    enum { ROUNDS = 12 };

    pthread_t workers[3];
    int made[3] = {0}, reaped = 0, checksum_ok = 1, nested_ok = 0, blocked_ok = 0;

    if (argc == 3 && strcmp(argv[1], "--exec-child") == 0) {
        int round = atoi(argv[2]);
        child_done("exec", (round * 7 + 3) & 63);
    }

    (void)corpus(1); /* Parent working set that every no-exec child should reuse. */
    if (pipe(blocked_pipe) != 0 || pthread_create(&workers[0], NULL, spinner, (void *)1) != 0 ||
        pthread_create(&workers[1], NULL, spinner, (void *)2) != 0 ||
        pthread_create(&workers[2], NULL, blocked_reader, NULL) != 0)
        return 2;
    while (atomic_load_explicit(&workers_ready, memory_order_acquire) != 3)
        sched_yield();

    for (int round = 0; round < ROUNDS; ++round) {
        int kind = round % 3;
        int expected = kind == 2 ? (round * 7 + 3) & 63 : child_value(round);
        pid_t child;
        if (kind == 1) {
            struct clone_args arguments;
            memset(&arguments, 0, sizeof arguments);
            arguments.exit_signal = SIGCHLD;
            child = (pid_t)syscall(SYS_clone3, &arguments, sizeof arguments);
        } else {
            child = fork();
        }
        if (child == 0) {
            if (kind == 2) {
                char value[16];
                snprintf(value, sizeof value, "%d", round);
                execl(argv[0], argv[0], "--exec-child", value, (char *)NULL);
                _exit(125);
            }
            if (round == 0) {
                pid_t nested = fork();
                if (nested == 0) _exit(19);
                if (!reap(nested, 19)) _exit(124);
            }
            child_done(kind == 0 ? "fork" : "clone3", child_value(round));
        }
        made[kind] += child > 0;
        if (reap(child, expected)) {
            reaped++;
            if (round == 0) nested_ok = 1;
        } else {
            checksum_ok = 0;
        }
    }

    atomic_store_explicit(&stop_workers, 1, memory_order_release);
    if (write(blocked_pipe[1], "x", 1) != 1) checksum_ok = 0;
    for (size_t index = 0; index < 3; ++index) {
        void *result = NULL;
        if (pthread_join(workers[index], &result) != 0 || result == NULL) {
            checksum_ok = 0;
        } else if (index == 2) {
            blocked_ok = (uintptr_t)result == 1;
            if (!blocked_ok) checksum_ok = 0;
        }
    }
    close(blocked_pipe[0]);
    close(blocked_pipe[1]);

    dprintf(STDERR_FILENO, "[cache-reuse] kind=parent\n");
    printf("chrome-cache-storm fork=%d clone3=%d exec=%d reaped=%d checksum=%d nested=%d blocked=%d\n", made[0],
           made[1], made[2], reaped, checksum_ok, nested_ok, blocked_ok);
    return made[0] == 4 && made[1] == 4 && made[2] == 4 && reaped == ROUNDS && checksum_ok && nested_ok && blocked_ok
               ? 0
               : 3;
}
