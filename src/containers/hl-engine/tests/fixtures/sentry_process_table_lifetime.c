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
    if (strcmp(program, "bound") == 0) return bound();
    if (strcmp(program, "collide") == 0 && argc > 2) return collide(argv[2]);
    return 30;
}
