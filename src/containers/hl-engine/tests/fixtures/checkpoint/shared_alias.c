#define _GNU_SOURCE
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

struct state {
    unsigned char bytes[256];
};

static int wait_file(const char *path) {
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        if (access(path, F_OK) == 0) return 0;
        struct timespec delay = {0, 1000000};
        nanosleep(&delay, NULL);
    }
    return -1;
}

static int transfer(int descriptor, void *byte, short events) {
    struct pollfd pollfd = {.fd = descriptor, .events = events};
    if (poll(&pollfd, 1, 30000) != 1) return -1;
    return events == POLLIN ? read(descriptor, byte, 1) == 1 : write(descriptor, byte, 1) == 1;
}

static int wait_sleeping(pid_t process) {
    char path[64], line[512];
    snprintf(path, sizeof path, "/proc/%d/stat", (int)process);
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        int descriptor = open(path, O_RDONLY);
        ssize_t count = descriptor < 0 ? -1 : read(descriptor, line, sizeof line - 1);
        if (descriptor >= 0) close(descriptor);
        if (count > 0) {
            line[count] = 0;
            char *end = strrchr(line, ')');
            if (end != NULL && end[1] == ' ' && end[2] == 'S') return 0;
        }
        struct timespec delay = {0, 1000000};
        nanosleep(&delay, NULL);
    }
    return -1;
}

static int reap_bounded(pid_t child) {
    int status = 0;
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        if (waitpid(child, &status, WNOHANG) == child) return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
        struct timespec delay = {0, 1000000};
        nanosleep(&delay, NULL);
    }
    kill(child, SIGKILL);
    for (unsigned attempt = 0; attempt < 5000; ++attempt) {
        if (waitpid(child, &status, WNOHANG) == child) break;
        struct timespec delay = {0, 1000000};
        nanosleep(&delay, NULL);
    }
    return -1;
}

static int fail_child(pid_t child, int result) {
    kill(child, SIGKILL);
    (void)reap_bounded(child);
    return result;
}

int main(int argc, char **argv) {
    if (argc != 2) return 10;
    char output[1024], cycle1[1024], cycle2[1024], finish[1024];
    snprintf(output, sizeof output, "%s/output", argv[1]);
    snprintf(cycle1, sizeof cycle1, "%s/cycle1", argv[1]);
    snprintf(cycle2, sizeof cycle2, "%s/cycle2", argv[1]);
    snprintf(finish, sizeof finish, "%s/finish", argv[1]);
    int log = open(output, O_WRONLY | O_CREAT | O_APPEND, 0600);
    int fd = (int)syscall(SYS_memfd_create, "checkpoint-shared-alias", 0u);
    int identity_collision = open("/dev/null", O_RDONLY);
    int command[2], acknowledgement[2];
    long page = sysconf(_SC_PAGESIZE);
    if (log != 3 || fd != 4 || identity_collision != 5 || page <= 0 || pipe(command) != 0 ||
        pipe(acknowledgement) != 0 || ftruncate(fd, 3 * page) != 0)
        return 11;
    struct state *first = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, page);
    struct state *second = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, page);
    if (first == MAP_FAILED || second == MAP_FAILED || first == second) return 12;
    dprintf(log, "BOOT\n");
    pid_t child = fork();
    if (child < 0) return 13;
    if (child == 0) {
        close(command[1]);
        close(acknowledgement[0]);
        for (unsigned generation = 1; generation <= 2; ++generation) {
            unsigned char token = 0, from_alias = 0, from_fd = 0;
            if (transfer(command[0], &token, POLLIN) != 1 || token != generation ||
                (from_alias = __atomic_load_n(&second->bytes[128], __ATOMIC_ACQUIRE)) !=
                    (unsigned char)(0x20 + generation) ||
                pread(fd, &from_fd, 1, page + offsetof(struct state, bytes) + 130) != 1 ||
                from_fd != (unsigned char)(0x40 + generation))
                _exit(20);
            __atomic_store_n(&second->bytes[129], (unsigned char)(0x60 + generation), __ATOMIC_RELEASE);
            from_fd = (unsigned char)(0x80 + generation);
            if (pwrite(fd, &from_fd, 1, page + offsetof(struct state, bytes) + 131) != 1) _exit(21);
            if (transfer(acknowledgement[1], &token, POLLOUT) != 1) _exit(21);
        }
        if (wait_file(finish) != 0) _exit(22);
        close(identity_collision);
        _exit(0);
    }
    close(command[0]);
    close(acknowledgement[1]);
    if (wait_sleeping(child) != 0) return fail_child(child, 14);
    dprintf(log, "READY\n");
    const char *cycles[2] = {cycle1, cycle2};
    for (unsigned generation = 1; generation <= 2; ++generation) {
        unsigned char token = (unsigned char)generation;
        unsigned char via_fd = (unsigned char)(0x40 + generation), alias_reply = 0, fd_reply = 0;
        if (wait_file(cycles[generation - 1]) != 0) return fail_child(child, 15);
        __atomic_store_n(&first->bytes[128], (unsigned char)(0x20 + generation), __ATOMIC_RELEASE);
        if (pwrite(fd, &via_fd, 1, page + offsetof(struct state, bytes) + 130) != 1 ||
            transfer(command[1], &token, POLLOUT) != 1 || transfer(acknowledgement[0], &token, POLLIN) != 1)
            return fail_child(child, 15);
        alias_reply = __atomic_load_n(&first->bytes[129], __ATOMIC_ACQUIRE);
        if (alias_reply != (unsigned char)(0x60 + generation) ||
            pread(fd, &fd_reply, 1, page + offsetof(struct state, bytes) + 131) != 1 ||
            fd_reply != (unsigned char)(0x80 + generation))
            return fail_child(child, 16);
        if (generation == 1 && wait_sleeping(child) != 0) return fail_child(child, 17);
        dprintf(log, generation == 1 ? "CYCLE 1\n" : "DONE shared-alias-ok\n");
    }
    if (wait_file(finish) != 0) return fail_child(child, 18);
    close(identity_collision);
    return reap_bounded(child) == 0 ? 0 : 19;
}
