#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int fails(int result, int error) {
    return result == -1 && errno == error;
}

static int exclusive_existing(const char *path) {
    return fails(mkdirat(9876, path, 0700), EEXIST) && fails(mknodat(9876, path, S_IFIFO | 0600, 0), EEXIST) &&
           fails(symlinkat("target", 9876, path), EEXIST);
}

int main(void) {
    int closed_setup = mkdir("/closed", 0700) == 0;
    int closed_fd = open("/closed/name", O_CREAT | O_WRONLY, 0600);
    closed_setup &= closed_fd >= 0;
    if (closed_fd >= 0) close(closed_fd);
    closed_setup &= setgid(1000) == 0 && setuid(1000) == 0 && geteuid() == 1000;

    const char *roots[] = {"/", "///", "/./"};
    int mkdir_root = 1, mknod_root = 1, symlink_root = 1;
    for (size_t i = 0; i < sizeof roots / sizeof roots[0]; ++i) {
        mkdir_root &= fails(mkdirat(9876, roots[i], 0700), EEXIST);
        mknod_root &= fails(mknodat(9876, roots[i], S_IFIFO | 0600, 0), EEXIST);
        symlink_root &= fails(symlinkat("target", 9876, roots[i]), EEXIST);
    }
    int merged_existing = exclusive_existing("/var") && exclusive_existing("/var/lib");
    int closed_search = fails(mkdirat(9876, "/closed/name", 0700), EACCES) &&
                        fails(mknodat(9876, "/closed/name", S_IFIFO | 0600, 0), EACCES) &&
                        fails(symlinkat("target", 9876, "/closed/name"), EACCES);
    int mknod_dir_relative_badfd = fails(mknodat(9876, "child", S_IFDIR | 0700, 0), EPERM);
    int mknod_dir_root = fails(mknodat(AT_FDCWD, "/", S_IFDIR | 0700, 0), EPERM);
    int mknod_dir_existing = fails(mknodat(AT_FDCWD, "/tmp", S_IFDIR | 0700, 0), EPERM);

    char missing[96];
    snprintf(missing, sizeof missing, "/hl-create-root-missing-%ld", (long)getpid());
    int missing_child = fails(mkdir(missing, 0700), EACCES);

    char base[96], left[128], right[128], link_path[160], existing[160], raw[256];
    snprintf(base, sizeof base, "/tmp/hl-create-walk-%ld", (long)getpid());
    snprintf(left, sizeof left, "%s/left", base);
    snprintf(right, sizeof right, "%s/right", base);
    int layout_setup = mkdir(base, 0700) == 0 && mkdir(left, 0700) == 0 && mkdir(right, 0700) == 0;
    snprintf(link_path, sizeof link_path, "%s/jump", left);
    layout_setup &= symlink("../right/deep", link_path) == 0;
    char deep[160];
    snprintf(deep, sizeof deep, "%s/deep", right);
    layout_setup &= mkdir(deep, 0700) == 0;
    snprintf(existing, sizeof existing, "%s/existing", right);
    layout_setup &= mkdir(existing, 0700) == 0;
    snprintf(raw, sizeof raw, "%s/jump/../existing", left);
    int symlink_before_dotdot_existing = fails(mkdir(raw, 0700), EEXIST);
    rmdir(existing);
    snprintf(existing, sizeof existing, "%s/existing", left);
    layout_setup &= mkdir(existing, 0700) == 0;
    int symlink_before_dotdot_missing = mkdir(raw, 0700) == 0;

    char dangling[160];
    snprintf(dangling, sizeof dangling, "%s/dangling", base);
    layout_setup &= symlink("absent", dangling) == 0;
    snprintf(raw, sizeof raw, "%s/dangling/", base);
    int dangling_trailing = fails(mkdir(raw, 0700), EEXIST);

    int pipes[2];
    int busybox_path = 0;
    if (pipe(pipes) == 0) {
        pid_t child = fork();
        if (child == 0) {
            close(pipes[0]);
            dup2(pipes[1], STDERR_FILENO);
            close(pipes[1]);
            execl("/bin/busybox", "busybox", "mkdir", "-p", "/var/run/postgresql", (char *)0);
            _exit(127);
        }
        close(pipes[1]);
        char output[512] = {0};
        ssize_t length = read(pipes[0], output, sizeof output - 1);
        close(pipes[0]);
        int status = 0;
        if (child > 0 && waitpid(child, &status, 0) == child && length >= 0)
            busybox_path = WIFEXITED(status) && WEXITSTATUS(status) == 0 && output[0] == 0;
    }

    unlink(dangling);
    rmdir(existing);
    snprintf(existing, sizeof existing, "%s/existing", right);
    rmdir(existing);
    rmdir(deep);
    unlink(link_path);
    rmdir(left);
    rmdir(right);
    rmdir(base);

    printf("create_root closed_setup=%d closed_search=%d mkdir=%d mknod=%d symlink=%d merged_existing=%d "
           "mknod_dir_badfd=%d mknod_dir_root=%d "
           "mknod_dir_existing=%d layout_setup=%d symlink_dotdot_existing=%d symlink_dotdot_missing=%d "
           "dangling_trailing=%d "
           "missing_child=%d busybox_pg_path=%d\n",
           closed_setup, closed_search, mkdir_root, mknod_root, symlink_root, merged_existing, mknod_dir_relative_badfd,
           mknod_dir_root, mknod_dir_existing, layout_setup, symlink_before_dotdot_existing,
           symlink_before_dotdot_missing, dangling_trailing, missing_child, busybox_path);
    return !(closed_setup && closed_search && mkdir_root && mknod_root && symlink_root && mknod_dir_relative_badfd &&
             mknod_dir_root && merged_existing && mknod_dir_existing && layout_setup &&
             symlink_before_dotdot_existing && symlink_before_dotdot_missing && dangling_trailing && missing_child &&
             busybox_path);
}
