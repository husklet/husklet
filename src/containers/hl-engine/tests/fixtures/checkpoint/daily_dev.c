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
#include <sys/mman.h>
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

static int helper(int cycle) {
    return cycle + 10;
}

#if defined(__x86_64__)
extern long checkpoint_link_target(long, long, long, long);
extern long checkpoint_link_source(long, long, long, long);
extern long checkpoint_mixed_sse(long);
__asm__(".pushsection .text.checkpoint_jcc_link,\"ax\",@progbits\n"
        ".balign 4096\n"
        ".global checkpoint_link_target\n.type checkpoint_link_target,@function\n"
        "checkpoint_link_target:\n lea 7(%rsi,%rcx),%rax\n ret\n"
        ".size checkpoint_link_target,.-checkpoint_link_target\n"
        ".balign 16\n"
        ".global checkpoint_link_source\n.type checkpoint_link_source,@function\n"
        "checkpoint_link_source:\n test %rdi,%rdi\n jnz checkpoint_link_target\n"
        "lea 3(%rsi,%rcx),%rax\n ret\n"
        ".size checkpoint_link_source,.-checkpoint_link_source\n"
        ".popsection\n");

__asm__(".text\n"
        ".global checkpoint_mixed_sse\n.type checkpoint_mixed_sse,@function\n"
        "checkpoint_mixed_sse:\n"
        "mov %rdi,%rax\n"
        "movq %rax,%xmm0\n"
        "lea 42(%rax),%rax\n"
        "movq %rax,%xmm1\n"
        "movq %xmm1,%rax\n"
        "ret\n"
        ".size checkpoint_mixed_sse,.-checkpoint_mixed_sse\n");

static long checkpoint_link_check(int phase) {
    volatile long warm = checkpoint_link_target(0, 31 + phase, 0, 4);
    long linked = checkpoint_link_source(1, 31 + phase, 0, 4);
    return warm == linked ? linked : -1;
}

static long checkpoint_mixed_check(int phase) {
    return checkpoint_mixed_sse(phase);
}
#else
static long checkpoint_link_check(int phase) {
    return 42 + phase;
}


static long checkpoint_mixed_check(int phase) {
    return 42 + phase;
}
#endif

static int profile_resources(const char *directory) {
    char scale_path[1024], data_path[1024];
    if (snprintf(scale_path, sizeof scale_path, "%s/profile-scale", directory) >= (int)sizeof scale_path ||
        snprintf(data_path, sizeof data_path, "%s/profile-mappings", directory) >= (int)sizeof data_path)
        return -1;
    FILE *scale_file = fopen(scale_path, "r");
    if (scale_file == NULL) return errno == ENOENT ? 0 : -1;
    unsigned scale = 0;
    if (fscanf(scale_file, "%u", &scale) != 1 || fclose(scale_file) != 0 || scale == 0 || scale > 2048) return -1;
    int data = open(data_path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (data < 0 || ftruncate(data, (off_t)scale * 4096) != 0) return -1;
    int *descriptors = calloc(scale, sizeof *descriptors);
    void **mappings = calloc(scale, sizeof *mappings);
    if (descriptors == NULL || mappings == NULL) return -1;
    for (unsigned index = 0; index < scale; ++index) {
        descriptors[index] = dup(data);
        mappings[index] = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE, data, (off_t)index * 4096);
        if (descriptors[index] < 0 || mappings[index] == MAP_FAILED) return -1;
        ((volatile unsigned char *)mappings[index])[0] = (unsigned char)index;
    }
    dprintf(STDOUT_FILENO, "PROFILE-RESOURCES mappings=%u descriptors=%u\n", scale, scale);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    const char *directory = argv[1];
    char output_path[1024];
    if (snprintf(output_path, sizeof output_path, "%s/output", directory) >= (int)sizeof output_path) return 2;
    int output = open(output_path, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (output < 0 || dup2(output, STDOUT_FILENO) < 0 || dup2(output, STDERR_FILENO) < 0) return 3;
    if (output > STDERR_FILENO) close(output);
    if (profile_resources(directory) != 0) return 15;

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
        dprintf(STDOUT_FILENO, "SLEEP-READY pid=%ld ppid=%ld pgid=%ld sid=%ld\n", (long)getpid(), (long)getppid(),
                (long)getpgrp(), (long)getsid(0));
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
    long initial_link = checkpoint_link_check(0);
    long initial_mixed = checkpoint_mixed_check(0);
    if (initial_link != 42 || initial_mixed != 42) return 16;
    dprintf(STDOUT_FILENO, "JCC-LINK phase=0 value=%ld\n", initial_link);
    dprintf(STDOUT_FILENO, "MIXED-SSE phase=0 value=%ld\n", initial_mixed);
    dprintf(STDOUT_FILENO, "READY leader=%ld sleeper=%ld worker=%ld pgid=%ld sid=%ld fg=%ld\n", (long)leader,
            (long)sleeper, (long)worker, (long)group, (long)session, (long)foreground);

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

        long restored_link = checkpoint_link_check(next_cycle);
        long restored_mixed = checkpoint_mixed_check(next_cycle);
        if (restored_link != 42 + next_cycle || restored_mixed != 42 + next_cycle) return 17;
        dprintf(STDOUT_FILENO, "JCC-LINK phase=%d value=%ld\n", next_cycle, restored_link);
        dprintf(STDOUT_FILENO, "MIXED-SSE phase=%d value=%ld\n", next_cycle, restored_mixed);

        pid_t child = fork();
        if (child < 0) return 9;
        if (child == 0) return helper(next_cycle);
        int child_status = 0;
        if (waitpid(child, &child_status, 0) != child || !WIFEXITED(child_status) ||
            WEXITSTATUS(child_status) != helper(next_cycle))
            return 10;
        if (getpid() != leader || getpgrp() != group || getsid(0) != session || tcgetpgrp(STDIN_FILENO) != foreground ||
            getpgid(sleeper) != group || getsid(sleeper) != session || getpgid(worker) != group ||
            getsid(worker) != session) {
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
