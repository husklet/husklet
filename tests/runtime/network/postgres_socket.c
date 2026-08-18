#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

struct child_result {
    uint8_t aliases;
    uint8_t identity;
};

static int socket_name_is(int descriptor, const char *path) {
    struct sockaddr_un address = {0};
    socklen_t length = sizeof address;
    return getsockname(descriptor, (struct sockaddr *)&address, &length) == 0 && address.sun_family == AF_UNIX &&
           strcmp(address.sun_path, path) == 0;
}

static int setup_failure(const char *stage, int code) {
    fprintf(stderr, "postgres-socket setup=%s errno=%d (%s)\n", stage, errno, strerror(errno));
    return code;
}

int main(void) {
    const char *directory = "/var/run/postgresql";
    const char *path = "/var/run/postgresql/.s.PGSQL.5432";
    struct sockaddr_un address = {0};
    struct stat before = {0}, after = {0};
    int start[2] = {-1, -1}, report[2] = {-1, -1};
    int listener = -1, duplicate = -1, collision = -1;
    pid_t child = -1;
    struct child_result child_report = {0};

    (void)unlink(path);
    if (mkdir("/var", 0755) != 0 && errno != EEXIST) return setup_failure("mkdir-var", 20);
    if (unlink("/var/run") != 0 && errno != ENOENT) return setup_failure("unlink-var-run", 27);
    if (mkdir("/var/run", 0755) != 0 && errno != EEXIST) return setup_failure("mkdir-var-run", 21);
    if (mkdir(directory, 0775) != 0 && errno != EEXIST) return setup_failure("mkdir-postgresql", 26);
    if (chown(directory, 70, 70) != 0) return setup_failure("chown-postgresql", 22);
    if (chmod(directory, 0775) != 0) return setup_failure("chmod-postgresql", 23);
    if (setresgid(70, 70, 70) != 0) return setup_failure("setresgid", 24);
    if (setresuid(70, 70, 70) != 0) return setup_failure("setresuid", 25);

    listener = socket(AF_UNIX, SOCK_STREAM, 0);
    duplicate = listener >= 0 ? dup(listener) : -1;
    if (listener < 0 || duplicate < 0 || pipe(start) != 0 || pipe(report) != 0) return 3;

    child = fork();
    if (child < 0) return 4;
    if (child == 0) {
        char byte = 0;
        close(start[1]);
        close(report[0]);
        if (read(start[0], &byte, 1) == 1) {
            struct stat first = {0}, second = {0};
            child_report.aliases = (uint8_t)(socket_name_is(listener, path) && socket_name_is(duplicate, path));
            child_report.identity = (uint8_t)(fstat(listener, &first) == 0 && fstat(duplicate, &second) == 0 &&
                                              first.st_dev == second.st_dev && first.st_ino == second.st_ino);
        }
        if (write(report[1], &child_report, sizeof child_report) != (ssize_t)sizeof child_report) _exit(6);
        _exit(child_report.aliases && child_report.identity ? 0 : 5);
    }

    close(start[0]);
    close(report[1]);
    address.sun_family = AF_UNIX;
    memcpy(address.sun_path, path, strlen(path) + 1);
    int bind_ok = bind(listener, (struct sockaddr *)&address, sizeof address) == 0;
    int chmod_ok = bind_ok && chmod(path, 0777) == 0;
    int stat_ok = chmod_ok && lstat(path, &before) == 0;
    int owner_ok = stat_ok && before.st_uid == 70 && before.st_gid == 70;
    int mode_ok = stat_ok && (before.st_mode & 07777) == 0777;
    int dup_ok = bind_ok && socket_name_is(listener, path) && socket_name_is(duplicate, path);

    collision = socket(AF_UNIX, SOCK_STREAM, 0);
    errno = 0;
    int collision_ok =
        collision >= 0 && bind(collision, (struct sockaddr *)&address, sizeof address) == -1 && errno == EADDRINUSE;
    int rollback_ok = collision_ok && lstat(path, &after) == 0 && before.st_dev == after.st_dev &&
                      before.st_ino == after.st_ino && after.st_uid == 70 && after.st_gid == 70 &&
                      (after.st_mode & 07777) == 0777 && socket_name_is(listener, path) &&
                      socket_name_is(duplicate, path);

    int started = write(start[1], "x", 1) == 1;
    close(start[1]);
    int report_ok = read(report[0], &child_report, sizeof child_report) == (ssize_t)sizeof child_report;
    close(report[0]);
    int status = 0;
    int waited = waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int fork_ok = started && report_ok && waited && child_report.aliases && child_report.identity;

    const char *format = "postgres-socket bind=%d chmod=%d owner=%d mode=%d dup=%d fork=%d collision=%d rollback=%d\n";
    printf(format, bind_ok, chmod_ok, owner_ok, mode_ok, dup_ok, fork_ok, collision_ok, rollback_ok);
    if (!(bind_ok && chmod_ok && owner_ok && mode_ok && dup_ok && fork_ok && collision_ok && rollback_ok)) {
        fprintf(stderr, format, bind_ok, chmod_ok, owner_ok, mode_ok, dup_ok, fork_ok, collision_ok, rollback_ok);
    }
    close(collision);
    close(duplicate);
    close(listener);
    (void)unlink(path);
    (void)rmdir(directory);
    return bind_ok && chmod_ok && owner_ok && mode_ok && dup_ok && fork_ok && collision_ok && rollback_ok ? 0 : 1;
}
