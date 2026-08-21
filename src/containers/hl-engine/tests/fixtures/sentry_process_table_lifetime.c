// Guest programs for the sentry's per-process virtual descriptor table lifetime.
//
// Under `HL_UNTRUSTED` the sentry keys one table per worker PROCESS, by the HOST pid every request from
// that worker carries. The table must live exactly as long as the process: an entry that outlives its
// process holds that process's duplicated descriptors forever, consumes one of the sentry's bounded
// process slots, and -- because the host kernel reissues a freed pid -- can be found by a completely
// different process that is later handed the same number.
//
// argv[1] selects the program:
//   rounds-waitid     one child at a time, ended by a signal and collected with waitid(2)
//   rounds-nocldwait  one child at a time, ended by a signal and collected by the SA_NOCLDWAIT auto-reap
//   blocked           children die while the guest is parked inside a FORWARDED blocking read
//   batch-nocldwait   a whole batch dies at once, so their SIGCHLDs coalesce into one delivery
//   bound             the simultaneously-live bound: fork until the sentry refuses
//   collide <dir>     leak one entry, then fork again onto that same host pid (see the harness)
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define ROUNDS 200
#define ATTEMPTS 70

static void arm_nocldwait(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_handler = SIG_DFL;
    action.sa_flags = SA_NOCLDWAIT;
    if (sigaction(SIGCHLD, &action, NULL) != 0) exit(20);
}

// One child alive at a time, for ROUNDS rounds. Every round ends its child with a signal, so the child
// never publishes its own exit, and collects it by a route other than wait4(2). A sentry that releases the
// entry when the host pid is freed runs this to completion; one that does not exhausts its slots instead.
static int rounds(void) {
    int completed = 0, failure = 0;
    for (int i = 0; i < ROUNDS; i++) {
        pid_t child = fork();
        if (child == 0)
            for (;;) pause();
        if (child < 0) { failure = errno; break; }
        kill(child, SIGKILL);
        siginfo_t info;
        memset(&info, 0, sizeof info);
        while (waitid(P_PID, (id_t)child, &info, WEXITED) < 0 && errno == EINTR) {}
        completed++;
    }
    printf("rounds completed=%d failure=%d\n", completed, failure);
    return completed == ROUNDS ? 0 : 1;
}

// One child alive at a time, for ROUNDS rounds, collected by NOBODY: SA_NOCLDWAIT tells Linux to leave no
// zombie, so the kernel frees the host pid without the guest ever calling a wait syscall. That is the third
// route by which the pid a virtual descriptor table is keyed on is freed, and it is the only one that
// happens entirely inside a host signal handler. A sentry that hears about it runs to completion; one that
// does not exhausts its bounded process slots instead.
//
// The child is gone -- not merely a zombie -- exactly when its pid stops naming a process, because
// SA_NOCLDWAIT is what leaves no corpse to name. Polling that is what keeps ONE child alive at a time, so
// the only thing ROUNDS rounds can exhaust is entries that outlived their processes.
static int rounds_nocldwait(void) {
    int completed = 0, failure = 0;
    arm_nocldwait();
    for (int i = 0; i < ROUNDS; i++) {
        pid_t child = fork();
        if (child == 0)
            for (;;) pause();
        if (child < 0) { failure = errno; break; }
        kill(child, SIGKILL);
        int gone = 0;
        for (int spin = 0; spin < 10000 && !gone; spin++) {
            if (kill(child, 0) < 0 && errno == ESRCH) gone = 1;
            else usleep(1000);
        }
        if (!gone) { failure = -1; break; }
        completed++;
    }
    printf("nocldwait completed=%d failure=%d\n", completed, failure);
    return completed == ROUNDS ? 0 : 1;
}

#define BATCH 8
#define BATCH_ROUNDS 20

// Linux coalesces a standard signal that is already pending, so killing a batch of children delivers ONE
// SIGCHLD, not one per child. The auto-reap has to collect every corpse that delivery stands for, and the
// sentry has to hear about every one of them -- which is what makes the pending record an ARRAY rather
// than a single slot. A batch is alive at a time here, far inside the sentry's 63-slot bound, and the
// rounds outlive it several times over, so the only thing that can exhaust the sentry is entries that
// outlived their processes. A child that is only ever killed must not make a forwarded syscall first
// (see `blocked`), so these spin.
static int batch_nocldwait(void) {
    arm_nocldwait();
    for (int round = 0; round < BATCH_ROUNDS; round++) {
        pid_t children[BATCH];
        for (int i = 0; i < BATCH; i++) {
            pid_t child = fork();
            if (child == 0)
                for (;;) {
                    volatile int alive = 1;
                    (void)alive;
                }
            if (child < 0) {
                printf("batch round=%d created=%d errno=%d\n", round, i, errno);
                for (int done = 0; done < i; done++) kill(children[done], SIGKILL);
                return 1;
            }
            children[i] = child;
        }
        for (int i = 0; i < BATCH; i++)
            kill(children[i], SIGKILL);
        for (int i = 0; i < BATCH; i++) {
            int gone = 0;
            for (int spin = 0; spin < 10000 && !gone; spin++) {
                if (kill(children[i], 0) < 0 && errno == ESRCH) gone = 1;
                else usleep(1000);
            }
            if (!gone) {
                printf("batch round=%d uncollected=%d\n", round, i);
                for (int rest = i; rest < BATCH; rest++) kill(children[rest], SIGKILL);
                return 1;
            }
        }
    }
    printf("batch rounds=%d children=%d\n", BATCH_ROUNDS, BATCH_ROUNDS * BATCH);
    return 0;
}

#define BLOCKED_ROUNDS 70

// The SA_NOCLDWAIT auto-reap runs in a host signal handler, and the thread it interrupts is a worker thread
// that may be half-way through a forwarded round-trip -- it has taken the ring's producer flag and is
// parked waiting for the sentry to answer. A release published from the handler through the ordinary
// control path would ask that same thread for that same flag, and the thread cannot give it back until the
// handler returns: the guest wedges, it does not race.
//
// Each round parks this process in a forwarded blocking read() and has a SIBLING kill the round's other
// child while it is parked -- a signal death, so that child never publishes its own exit and its table can
// only be released by the auto-reap route. BLOCKED_ROUNDS therefore outlives the sentry's 63 process slots
// as well, and one program answers both questions: the guest is not wedged, and the table was released.
static int blocked(void) {
    int pipefd[2];
    if (pipe(pipefd) != 0) return 22;
    arm_nocldwait();
    for (int i = 0; i < BLOCKED_ROUNDS; i++) {
        pid_t dying = fork();
        if (dying == 0)
            // Spin rather than pause(): this child exists to be killed, and a killed child never runs the
            // sentry's own teardown, so it must not have claimed a sentry ring lane by making a forwarded
            // syscall first. A lane leaked that way is a separate bound this program is not measuring.
            for (;;) {
                volatile int alive = 1;
                (void)alive;
            }
        if (dying < 0) { printf("blocked round=%d fork-dying errno=%d\n", i, errno); return 1; }
        pid_t writer = fork();
        if (writer < 0) {
            printf("blocked round=%d fork-writer errno=%d\n", i, errno);
            kill(dying, SIGKILL);
            return 1;
        }
        if (writer == 0) {
            usleep(25000);
            kill(dying, SIGKILL);
            usleep(35000);
            char one = 'x';
            ssize_t put = write(pipefd[1], &one, 1);
            _exit(put == 1 ? 0 : 1);
        }
        char taken;
        ssize_t got;
        while ((got = read(pipefd[0], &taken, 1)) < 0 && errno == EINTR) {}
        if (got != 1) {
            printf("blocked round=%d read=%zd errno=%d\n", i, got, errno);
            kill(dying, SIGKILL);
            kill(writer, SIGKILL);
            return 1;
        }
    }
    printf("blocked rounds=%d\n", BLOCKED_ROUNDS);
    return 0;
}

// The bound on tables the sentry holds for SIMULTANEOUSLY live children. This is fail-closed behaviour and
// must not move: a fork past it is refused, not served from a recycled slot.
static int bound(void) {
    pid_t children[ATTEMPTS];
    int created = 0, failure = 0;
    for (int i = 0; i < ATTEMPTS; i++) {
        pid_t child = fork();
        if (child == 0)
            for (;;) pause();
        if (child < 0) { failure = errno; break; }
        children[created++] = child;
    }
    int reaped = 0;
    for (int i = 0; i < created; i++) {
        kill(children[i], SIGKILL);
        int status;
        if (waitpid(children[i], &status, 0) == children[i]) reaped++;
    }
    printf("bound created=%d failure=%d reaped=%d\n", created, failure, reaped);
    return created == 63 && failure == EAGAIN && reaped == created ? 0 : 1;
}

static void publish(const char *path, const char *text) {
    FILE *file = fopen(path, "w");
    if (!file) exit(21);
    fputs(text, file);
    fclose(file);
}

static int await(const char *path, int seconds) {
    for (int i = 0; i < seconds * 1000; i++) {
        FILE *file = fopen(path, "r");
        if (file) { fclose(file); return 1; }
        usleep(1000);
    }
    return 0;
}

// Leak exactly one entry, then ask the harness to re-arm the kernel's pid allocator on the leaked number
// and fork again. The fork must be SERVED: the pid names a child this process was just handed, so any
// entry filed under it is a corpse. Refusing it (-EEXIST, an errno clone(2) cannot return) fails an
// ordinary container fork on a number the kernel was free to reissue.
//
// The leak is arranged with SA_NOCLDWAIT because that is the release path the sentry still does not see;
// if that path is ever closed this program no longer leaks, the fork is served for the plainer reason,
// and the assertion below stays true.
static int collide(const char *directory) {
    char victim_path[512], go_path[512], text[128];
    snprintf(victim_path, sizeof victim_path, "%s/victim", directory);
    snprintf(go_path, sizeof go_path, "%s/go", directory);
    arm_nocldwait();
    pid_t victim = fork();
    if (victim == 0)
        for (;;) pause();
    if (victim < 0) return 2;
    kill(victim, SIGKILL);
    usleep(20000);
    for (int attempt = 1; attempt <= 60; attempt++) {
        snprintf(text, sizeof text, "%d %d\n", (int)victim, attempt);
        publish(victim_path, text);
        if (!await(go_path, 30)) return 3;
        pid_t child = fork();
        int error = errno;
        if (child == 0) _exit(0);
        remove(go_path);
        if (child < 0) {
            printf("collide refused attempt=%d errno=%d\n", attempt, error);
            return 7;
        }
        if (child == victim) {
            printf("collide served victim=%d attempt=%d\n", (int)victim, attempt);
            return 0;
        }
    }
    printf("collide no-collision\n");
    return 6;
}

int main(int argc, char **argv) {
    const char *program = argc > 1 ? argv[1] : "";
    if (strcmp(program, "rounds-waitid") == 0) return rounds();
    if (strcmp(program, "rounds-nocldwait") == 0) return rounds_nocldwait();
    if (strcmp(program, "blocked") == 0) return blocked();
    if (strcmp(program, "batch-nocldwait") == 0) return batch_nocldwait();
    if (strcmp(program, "bound") == 0) return bound();
    if (strcmp(program, "collide") == 0 && argc > 2) return collide(argv[2]);
    return 30;
}
