/* Daily-development checkpoint fixture: an interactive-shell-shaped leader
 * owns a real sleep(1000) child while a sibling continuously performs
 * package-manager-shaped durable file replacement and socket traffic. */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

static int exists(const char *directory, const char *name) {
    char path[1024];
    if (snprintf(path, sizeof path, "%s/%s", directory, name) >= (int)sizeof path) return -1;
    return access(path, F_OK) == 0;
}

static int durable_progress(const char *directory, uint64_t value) {
    char temporary[1024], state[1024];
    if (snprintf(temporary, sizeof temporary, "%s/state.new", directory) >= (int)sizeof temporary ||
        snprintf(state, sizeof state, "%s/state", directory) >= (int)sizeof state)
        return -1;
    int descriptor = open(temporary, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (descriptor < 0 || dprintf(descriptor, "%llu\n", (unsigned long long)value) < 0 || fsync(descriptor) != 0 ||
        close(descriptor) != 0 || rename(temporary, state) != 0)
        return -1;
    descriptor = open(directory, O_RDONLY | O_DIRECTORY);
    if (descriptor < 0 || fsync(descriptor) != 0 || close(descriptor) != 0) return -1;
    return 0;
}

static int workload(const char *directory, int transport) {
    uint64_t progress = 0;
    struct timespec pause = {.tv_nsec = 2000000};
    for (;;) {
        if (exists(directory, "stop") == 1) return 0;
        if (durable_progress(directory, ++progress) != 0) return 20;
        if (write(transport, &progress, sizeof progress) != (ssize_t)sizeof progress) return 21;
        if (nanosleep(&pause, NULL) != 0 && errno != EINTR) return 22;
    }
}

static int helper(int cycle) { return cycle + 10; }

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    const char *directory = argv[1];
    char output_path[1024];
    if (snprintf(output_path, sizeof output_path, "%s/output", directory) >= (int)sizeof output_path) return 2;
    int output = open(output_path, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (output < 0 || dup2(output, STDOUT_FILENO) < 0 || dup2(output, STDERR_FILENO) < 0) return 3;
    if (output > STDERR_FILENO) close(output);

    pid_t leader = getpid();
    pid_t group = getpgrp();
    pid_t session = getsid(0);
    pid_t foreground = tcgetpgrp(STDIN_FILENO);
    int transport[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, transport) != 0) return 4;

    pid_t sleeper = fork();
    if (sleeper < 0) return 5;
    if (sleeper == 0) {
        close(transport[0]);
        close(transport[1]);
        dprintf(STDOUT_FILENO, "SLEEP-READY pid=%ld ppid=%ld pgid=%ld sid=%ld\n", (long)getpid(),
                (long)getppid(), (long)getpgrp(), (long)getsid(0));
        unsigned remaining = sleep(1000);
        dprintf(STDOUT_FILENO, "SLEEP-RETURN remaining=%u\n", remaining);
        return 30;
    }

    pid_t worker = fork();
    if (worker < 0) return 6;
    if (worker == 0) {
        close(transport[0]);
        int status = workload(directory, transport[1]);
        close(transport[1]);
        return status;
    }
    close(transport[1]);
    dprintf(STDOUT_FILENO,
            "READY leader=%ld sleeper=%ld worker=%ld pgid=%ld sid=%ld fg=%ld\n", (long)leader, (long)sleeper,
            (long)worker, (long)group, (long)session, (long)foreground);

    uint64_t progress = 0, previous = 0;
    if (foreground <= 0) return 14;
    int next_cycle = 1;
    while (next_cycle <= 2) {
        struct pollfd descriptor = {.fd = transport[0], .events = POLLIN};
        int ready = poll(&descriptor, 1, 100);
        if (ready < 0 && errno != EINTR) return 7;
        if (ready > 0 && (descriptor.revents & POLLIN)) {
            uint64_t observed;
            ssize_t count = read(transport[0], &observed, sizeof observed);
            if (count != (ssize_t)sizeof observed || observed <= progress) return 8;
            progress = observed;
        }
        char marker[32];
        snprintf(marker, sizeof marker, "cycle%d", next_cycle);
        if (progress < previous + 5 || exists(directory, marker) != 1) continue;

        pid_t child = fork();
        if (child < 0) return 9;
        if (child == 0) return helper(next_cycle);
        int child_status = 0;
        if (waitpid(child, &child_status, 0) != child || !WIFEXITED(child_status) ||
            WEXITSTATUS(child_status) != helper(next_cycle))
            return 10;
        if (getpid() != leader || getpgrp() != group || getsid(0) != session || tcgetpgrp(STDIN_FILENO) != foreground ||
            getpgid(sleeper) != group || getsid(sleeper) != session || getpgid(worker) != group || getsid(worker) != session) {
            dprintf(STDOUT_FILENO,
                    "IDENTITY-ERROR expected=%ld/%ld/%ld/%ld actual=%ld/%ld/%ld/%ld sleeper=%ld/%ld worker=%ld/%ld\n",
                    (long)leader, (long)group, (long)session, (long)foreground, (long)getpid(), (long)getpgrp(),
                    (long)getsid(0), (long)tcgetpgrp(STDIN_FILENO), (long)getpgid(sleeper), (long)getsid(sleeper),
                    (long)getpgid(worker), (long)getsid(worker));
            return 11;
        }
        dprintf(STDOUT_FILENO,
                "CYCLE %d progress=%llu leader=%ld sleeper=%ld worker=%ld pgid=%ld sid=%ld fg=%ld helper=%ld\n",
                next_cycle, (unsigned long long)progress, (long)leader, (long)sleeper, (long)worker, (long)group,
                (long)session, (long)foreground, (long)child);
        previous = progress;
        ++next_cycle;
    }

    if (kill(sleeper, SIGTERM) != 0) return 12;
    int sleeper_status = 0, worker_status = 0;
    if (waitpid(sleeper, &sleeper_status, 0) != sleeper || !WIFSIGNALED(sleeper_status) ||
        WTERMSIG(sleeper_status) != SIGTERM || waitpid(worker, &worker_status, 0) != worker ||
        !WIFEXITED(worker_status) || WEXITSTATUS(worker_status) != 0)
        return 13;
    close(transport[0]);
    dprintf(STDOUT_FILENO, "DONE progress=%llu\n", (unsigned long long)progress);
    return 0;
}
