#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

enum { REPORT_FD = 90, ACK_FD = 91, FILE_A = 100, FILE_B = 101, PIPE_A = 110, PIPE_B = 111, DUP_FD = 112 };

#define MAX_FDS 512
#define TARGETS 5

struct snapshot {
    int pid;
    int phase;
    int count;
    int fds[MAX_FDS];
    char target[TARGETS][256];
};

static const int target_fd[TARGETS] = {FILE_A, FILE_B, PIPE_A, PIPE_B, DUP_FD};

static int full_io(int fd, void *buffer, size_t size, int writing) {
    unsigned char *bytes = buffer;
    while (size) {
        ssize_t done = writing ? write(fd, bytes, size) : read(fd, bytes, size);
        if (done < 0 && errno == EINTR) continue;
        if (done <= 0) return -1;
        bytes += done;
        size -= (size_t)done;
    }
    return 0;
}

static int compare_int(const void *a, const void *b) {
    return (*(const int *)a > *(const int *)b) - (*(const int *)a < *(const int *)b);
}

/* Keep DIR open until the observer acknowledges: its descriptor is then legitimately visible in both views. */
static int send_snapshot(int phase) {
    struct snapshot shot = {.pid = (int)getpid(), .phase = phase};
    for (int i = 0; i < TARGETS; ++i) {
        char path[64];
        snprintf(path, sizeof path, "/proc/self/fd/%d", target_fd[i]);
        ssize_t n = readlink(path, shot.target[i], sizeof shot.target[i] - 1);
        if (n >= 0) shot.target[i][n] = 0;
    }
    DIR *directory = opendir("/proc/self/fd");
    if (!directory) return -1;
    struct dirent *entry;
    while ((entry = readdir(directory)) != NULL && shot.count < MAX_FDS) {
        char *end;
        long fd = strtol(entry->d_name, &end, 10);
        if (*entry->d_name && *end == 0 && fd >= 0 && fd <= INT32_MAX) shot.fds[shot.count++] = (int)fd;
    }
    qsort(shot.fds, (size_t)shot.count, sizeof shot.fds[0], compare_int);
    int result = full_io(REPORT_FD, &shot, sizeof shot, 1);
    char ack;
    if (result == 0) result = full_io(ACK_FD, &ack, 1, 0);
    closedir(directory);
    return result;
}

static int pin(int source, int target) {
    if (source == target) return target;
    int result = dup2(source, target);
    close(source);
    return result;
}

static int child_main(const char *program) {
    char first[64], second[64];
    snprintf(first, sizeof first, "/tmp/.hlpeerfd_a_%d", (int)getpid());
    snprintf(second, sizeof second, "/tmp/.hlpeerfd_b_%d", (int)getpid());
    int a = open(first, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (a < 0 || pin(a, FILE_A) != FILE_A) return 2;
    a = open(first, O_RDWR);
    if (a < 0 || pin(a, FILE_B) != FILE_B || dup2(FILE_A, DUP_FD) != DUP_FD) return 3;
    int pipes[2];
    if (pipe(pipes) != 0 || pin(pipes[0], PIPE_A) != PIPE_A || pin(pipes[1], PIPE_B) != PIPE_B) return 4;
    if (send_snapshot(1) != 0) return 5;

    close(DUP_FD);
    close(FILE_A);
    int replacement = open(second, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (replacement < 0 || pin(replacement, FILE_A) != FILE_A) return 6;
    long closed = syscall(SYS_close_range, PIPE_A, PIPE_B, 0);
    if (closed < 0 && errno == ENOSYS) {
        close(PIPE_A);
        close(PIPE_B);
    } else if (closed < 0)
        return 7;
    if (send_snapshot(2) != 0) return 8;

    pid_t grandchild = fork();
    if (grandchild < 0) return 9;
    if (grandchild == 0) _exit(send_snapshot(3) == 0 ? 0 : 10);
    int status;
    if (waitpid(grandchild, &status, 0) != grandchild || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 11;

    if (fcntl(FILE_A, F_SETFD, FD_CLOEXEC) != 0) return 12;
    char report[16], ack[16];
    snprintf(report, sizeof report, "%d", REPORT_FD);
    snprintf(ack, sizeof ack, "%d", ACK_FD);
    char *args[] = {(char *)program, (char *)"--after-exec", report, ack, NULL};
    execv(program, args);
    return 13;
}

static int collect_peer(const struct snapshot *shot, int fds[MAX_FDS], char target[TARGETS][256]) {
    char directory_path[64];
    snprintf(directory_path, sizeof directory_path, "/proc/%d/fd", shot->pid);
    DIR *directory = opendir(directory_path);
    if (!directory) return -1;
    int count = 0;
    struct dirent *entry;
    while ((entry = readdir(directory)) != NULL && count < MAX_FDS) {
        char *end;
        long fd = strtol(entry->d_name, &end, 10);
        if (*entry->d_name && *end == 0 && fd >= 0 && fd <= INT32_MAX) fds[count++] = (int)fd;
    }
    closedir(directory);
    qsort(fds, (size_t)count, sizeof fds[0], compare_int);
    for (int i = 0; i < TARGETS; ++i) {
        char path[80];
        snprintf(path, sizeof path, "/proc/%d/fd/%d", shot->pid, target_fd[i]);
        ssize_t n = readlink(path, target[i], 255);
        if (n >= 0) target[i][n] = 0;
    }
    return count;
}

static void report_set_mismatch(const struct snapshot *shot, const int *peer, int peer_count) {
    fprintf(stderr, "peerfd phase=%d self=", shot->phase);
    for (int i = 0; i < shot->count; ++i)
        fprintf(stderr, "%s%d", i ? "," : "", shot->fds[i]);
    fprintf(stderr, " peer=");
    for (int i = 0; i < peer_count; ++i)
        fprintf(stderr, "%s%d", i ? "," : "", peer[i]);
    fputc('\n', stderr);
    for (int i = 0; i < peer_count; ++i) {
        char path[80], target[256];
        snprintf(path, sizeof path, "/proc/%d/fd/%d", shot->pid, peer[i]);
        ssize_t length = readlink(path, target, sizeof target - 1);
        if (length >= 0) target[length] = 0;
        fprintf(stderr, "peerfd phase=%d peer-fd=%d target='%s'\n", shot->phase, peer[i], length >= 0 ? target : "");
    }
}

int main(int argc, char **argv) {
    if (argc == 4 && !strcmp(argv[1], "--after-exec")) {
        if (dup2(atoi(argv[2]), REPORT_FD) < 0 || dup2(atoi(argv[3]), ACK_FD) < 0) return 20;
        return send_snapshot(4) == 0 ? 0 : 21;
    }
    int reports[2], acks[2];
    if (pipe(reports) != 0 || pipe(acks) != 0) return 1;
    pid_t child = fork();
    if (child == 0) {
        close(reports[0]);
        close(acks[1]);
        if (pin(reports[1], REPORT_FD) != REPORT_FD || pin(acks[0], ACK_FD) != ACK_FD) _exit(30);
        _exit(child_main(argv[0]));
    }
    close(reports[1]);
    close(acks[0]);
    int set_ok = 1, same_ok = 1, reuse_ok = 1, close_ok = 1, fork_ok = 1, exec_ok = 1;
    char first_target[256] = {0};
    for (int expected = 1; expected <= 4; ++expected) {
        struct snapshot shot;
        if (full_io(reports[0], &shot, sizeof shot, 0) != 0 || shot.phase != expected) {
            set_ok = 0;
            break;
        }
        int peer[MAX_FDS];
        char targets[TARGETS][256] = {{0}};
        int count = collect_peer(&shot, peer, targets);
        if (count != shot.count || count < 0 || memcmp(peer, shot.fds, (size_t)shot.count * sizeof peer[0]) != 0) {
            set_ok = 0;
            report_set_mismatch(&shot, peer, count > 0 ? count : 0);
        }
        for (int i = 0; i < TARGETS; ++i) {
            if (!strcmp(targets[i], shot.target[i])) continue;
            set_ok = 0;
            fprintf(stderr, "peerfd phase=%d fd=%d self='%s' peer='%s'\n", shot.phase, target_fd[i], shot.target[i],
                    targets[i]);
        }
        if (expected == 1) {
            same_ok = shot.target[0][0] && !strcmp(shot.target[0], shot.target[1]) &&
                      !strcmp(shot.target[0], shot.target[4]) && shot.target[2][0] &&
                      !strcmp(shot.target[2], shot.target[3]);
            snprintf(first_target, sizeof first_target, "%s", shot.target[0]);
        } else if (expected == 2) {
            reuse_ok = shot.target[0][0] && strcmp(first_target, shot.target[0]) && !shot.target[4][0];
            close_ok = !shot.target[2][0] && !shot.target[3][0];
        } else if (expected == 3)
            fork_ok = shot.pid != child && shot.target[0][0] && shot.target[1][0];
        else
            exec_ok = !shot.target[0][0] && shot.target[1][0];
        if (full_io(acks[1], "A", 1, 1) != 0) {
            set_ok = 0;
            break;
        }
    }
    int status = 0;
    waitpid(child, &status, 0);
    unlink(first_target);
    char second_marker[64];
    snprintf(second_marker, sizeof second_marker, "/tmp/.hlpeerfd_b_%d", (int)child);
    unlink(second_marker);
    int ok = set_ok && same_ok && reuse_ok && close_ok && fork_ok && exec_ok && WIFEXITED(status) &&
             WEXITSTATUS(status) == 0;
    printf("peerfd ok=%d set=%d private=%d same=%d reuse=%d close=%d fork=%d exec=%d\n", ok, set_ok, set_ok, same_ok,
           reuse_ok, close_ok, fork_ok, exec_ok);
    return 0;
}
