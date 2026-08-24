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
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *self = "/opt/husklet/cwd-relative-resolution";
static pid_t owner;
static char root_path[128], image_path[128], mounted_path[128];

static void cleanup(void) {
    if (getpid() != owner) return;
    unlink(root_path);
    unlink(image_path);
    unlink(mounted_path);
}

static void interrupted(int signal) {
    cleanup();
    _exit(128 + signal);
}

static int write_tag(const char *path, const char *tag) {
    int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY | O_CLOEXEC, 0600);
    size_t length = strlen(tag);
    int ok = fd >= 0 && write(fd, tag, length) == (ssize_t)length && close(fd) == 0;
    if (fd >= 0 && !ok) close(fd);
    return ok ? 0 : -1;
}

static int read_tag(int fd, const char *tag, struct stat *status) {
    char bytes[32] = {0};
    size_t length = strlen(tag);
    if (fd < 0 || length >= sizeof bytes || read(fd, bytes, sizeof bytes) != (ssize_t)length ||
        memcmp(bytes, tag, length) != 0 || fstat(fd, status) != 0 || close(fd) != 0)
        return -1;
    return 0;
}

static int listed(const char *leaf) {
    int fd = open(".", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (fd < 0) return 0;
    char bytes[1024];
    int found = 0;
    for (;;) {
        int count = (int)syscall(SYS_getdents64, fd, bytes, sizeof bytes);
        if (count <= 0) break;
        for (int offset = 0; offset < count;) {
            struct dirent64 *entry = (struct dirent64 *)(bytes + offset);
            if (entry->d_reclen == 0 || offset + entry->d_reclen > count) {
                close(fd);
                return 0;
            }
            if (strcmp(entry->d_name, leaf) == 0) found++;
            offset += entry->d_reclen;
        }
    }
    close(fd);
    return found == 1;
}

static int check_here(const char *directory, const char *absolute, const char *leaf, const char *tag) {
    char cwd[512];
    struct stat relative_open, at_open, relative_stat, absolute_stat, dot, directory_stat;
    if (!getcwd(cwd, sizeof cwd) || strcmp(cwd, directory) != 0) return 10;
    if (read_tag(open(leaf, O_RDONLY | O_CLOEXEC), tag, &relative_open) != 0) return 11;
    if (read_tag(openat(AT_FDCWD, leaf, O_RDONLY | O_CLOEXEC), tag, &at_open) != 0) return 12;
    if (stat(leaf, &relative_stat) != 0 || stat(absolute, &absolute_stat) != 0 || stat(".", &dot) != 0 ||
        stat(directory, &directory_stat) != 0)
        return 13;
    if (relative_open.st_dev != absolute_stat.st_dev || relative_open.st_ino != absolute_stat.st_ino ||
        at_open.st_dev != absolute_stat.st_dev || at_open.st_ino != absolute_stat.st_ino ||
        relative_stat.st_dev != absolute_stat.st_dev || relative_stat.st_ino != absolute_stat.st_ino ||
        dot.st_dev != directory_stat.st_dev || dot.st_ino != directory_stat.st_ino)
        return 14;
    return listed(leaf) ? 0 : 15;
}

static int check(const char *directory, const char *absolute, const char *leaf, const char *tag) {
    return chdir(directory) == 0 ? check_here(directory, absolute, leaf, tag) : 10;
}

static int child_status(pid_t child) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status) ? WEXITSTATUS(status) : 99;
}

static int exercise(const char *directory, const char *absolute, const char *leaf, const char *tag) {
    int result = check(directory, absolute, leaf, tag);
    if (result != 0) return result;
    pid_t child = fork();
    if (child == 0) _exit(check_here(directory, absolute, leaf, tag));
    if (child < 0 || (result = child_status(child)) != 0) return 20 + result;
    child = fork();
    if (child == 0) {
        execl(self, self, "reexec", directory, absolute, leaf, tag, (char *)NULL);
        _exit(98);
    }
    if (child < 0 || (result = child_status(child)) != 0) return 120 + result;
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 6 && strcmp(argv[1], "reexec") == 0) return check_here(argv[2], argv[3], argv[4], argv[5]);
    if (argc != 1) return 2;
    volatile uint64_t native_warmup = 1;
    for (uint64_t index = 0; index < 100000; ++index) native_warmup = native_warmup * 33u + index;
    if (native_warmup == 0) return 2;
    char leaf[96];
    snprintf(leaf, sizeof leaf, ".husklet-cwd-%ld", (long)getpid());
    snprintf(root_path, sizeof root_path, "/%s", leaf);
    snprintf(image_path, sizeof image_path, "/etc/%s", leaf);
    snprintf(mounted_path, sizeof mounted_path, "/mnt/%s", leaf);
    owner = getpid();
    struct sigaction action = {.sa_handler = interrupted};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGINT, &action, NULL) != 0 || sigaction(SIGTERM, &action, NULL) != 0) return 3;
    int result = 3;
    if (write_tag(root_path, "ROOT-TAG") != 0 || write_tag(image_path, "IMAGE-TAG") != 0 ||
        write_tag(mounted_path, "MOUNT-TAG") != 0)
        goto done;
    result = exercise("/etc", image_path, leaf, "IMAGE-TAG");
    if (result == 0) result = exercise("/mnt", mounted_path, leaf, "MOUNT-TAG");
done:
    cleanup();
    return result;
}
