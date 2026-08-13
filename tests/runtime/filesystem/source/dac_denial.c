// The guest DAC layer must be able to DENY, not merely to allow. Root prepares one closed directory
// (0755, root-owned) and one open directory (0777), then two children at different uids exercise
// the owner and parent-permission checks. Every field below is a denial that must actually happen,
// so a bypass pinned on anywhere in the authorize path turns one of them to 0.
// `distinct` is the non-vacuity guard: without it, two children that both failed to drop privilege
// would agree with every expectation trivially and the case would pass against no DAC at all.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define FIRST_UID 2000
#define SECOND_UID 3000

struct report {
    unsigned live_uid;
    // Denials: creation under a directory this uid cannot write, and metadata changes on a file
    // owned by the other uid.
    unsigned closed_denied, chmod_denied, chown_denied, times_denied, now_denied, high_uid_denied;
    // Allowances that must survive: the open directory and this uid's own file.
    unsigned open_allowed, own_chmod, supplementary_chown, nofollow_preserved, own_file_uid;
    // Probe of the closed directory as the guest sees it, so a failure here is readable.
    unsigned closed_mode, closed_uid;
};

static int drop(unsigned uid) {
    if (setresgid(uid, uid, uid) != 0) return 1;
    return setresuid(uid, uid, uid) != 0;
}

static void first_child(struct report *out) {
    struct stat status;
    if (lstat("closed", &status) == 0) {
        out->closed_mode = status.st_mode & 07777;
        out->closed_uid = status.st_uid;
    }
    out->closed_denied = mkdir("closed/nope", 0755) != 0 && errno == EACCES;
    out->open_allowed = mkdir("open/mine-dir", 0755) == 0;
    int fd = open("open/mine", O_CREAT | O_RDWR, 0644);
    if (fd >= 0) close(fd);
    out->own_chmod = chmod("open/mine", 0600) == 0;
    out->supplementary_chown = chown("open/mine", (uid_t)-1, 4000) == 0;
    out->high_uid_denied = chown("open/mine", (uid_t)UINT32_C(0x80000000), (gid_t)-1) != 0 && errno == EPERM;
    if (lstat("open/mine", &status) == 0) out->own_file_uid = status.st_uid;
    out->live_uid = getuid();
}

static void second_child(struct report *out) {
    // A different unprivileged uid owns nothing here, so every metadata change must be refused.
    out->chmod_denied = chmod("open/mine", 0666) != 0 && errno == EPERM;
    out->chown_denied = chown("open/mine", SECOND_UID, SECOND_UID) != 0 && errno == EPERM;
    struct timespec times[2];
    times[0].tv_sec = 1;
    times[0].tv_nsec = 0;
    times[1] = times[0];
    // Explicit times, so Linux answers EPERM rather than the EACCES of the now-or-omit form.
    out->times_denied = utimensat(AT_FDCWD, "open/mine", times, 0) != 0 && errno == EPERM;
    out->now_denied = utimensat(AT_FDCWD, "open/mine", NULL, 0) != 0 && errno == EACCES;
    out->closed_denied = mkdir("closed/nope2", 0755) != 0 && errno == EACCES;
    out->live_uid = getuid();
}

static int run(unsigned uid, void (*body)(struct report *), struct report *out) {
    int channel[2];
    if (pipe(channel) != 0) return 1;
    pid_t child = fork();
    if (child < 0) return 2;
    if (child == 0) {
        close(channel[0]);
        struct report report;
        memset(&report, 0, sizeof report);
        gid_t supplementary = 4000;
        if (setgroups(1, &supplementary) != 0 || drop(uid) != 0) _exit(21);
        body(&report);
        if (write(channel[1], &report, sizeof report) != (ssize_t)sizeof report) _exit(22);
        _exit(0);
    }
    close(channel[1]);
    ssize_t read_bytes = read(channel[0], out, sizeof *out);
    close(channel[0]);
    int status = 0;
    if (waitpid(child, &status, 0) != child) return 3;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return 4;
    return read_bytes == (ssize_t)sizeof *out ? 0 : 5;
}

int main(void) {
    char directory[] = "/tmp/hl-dac-denial-XXXXXX";
    if (!mkdtemp(directory)) return 1;
    // mkdtemp gives 0700, which no dropped uid could even traverse.
    if (chmod(directory, 0755) != 0) return 2;
    if (chdir(directory) != 0) return 1;
    // Explicit modes, because the umask would otherwise decide what this case is testing.
    if (mkdir("closed", 0700) != 0 || chmod("closed", 0755) != 0) return 3;
    if (mkdir("open", 0700) != 0 || chmod("open", 0777) != 0) return 4;
    if (symlink("open", "open-link") != 0) return 7;
    errno = 0;
    int nofollow = syscall(452, AT_FDCWD, "open-link", 0700, AT_SYMLINK_NOFOLLOW);
    struct stat open_status;
    unsigned nofollow_preserved = nofollow == -1 && errno == EOPNOTSUPP && stat("open", &open_status) == 0 &&
                                  (open_status.st_mode & 07777) == 0777;

    struct report first, second;
    memset(&first, 0, sizeof first);
    memset(&second, 0, sizeof second);
    if (run(FIRST_UID, first_child, &first) != 0) return 5;
    if (run(SECOND_UID, second_child, &second) != 0) return 6;
    first.nofollow_preserved = nofollow_preserved;

    // Non-vacuity: both children really became the uid they asked for, the two differ from each
    // other and from root, and the file the second child was refused really belongs to the first.
    unsigned distinct = first.live_uid == FIRST_UID && second.live_uid == SECOND_UID
                        && first.live_uid != second.live_uid && first.live_uid != (unsigned)getuid()
                        && (unsigned)getuid() == 0 && first.own_file_uid == FIRST_UID;

    printf(
        "dac-denial closed=%04o:%u denied=%u:%u chmod=%u chown=%u times=%u now=%u high-uid=%u "
        "open=%u own-chmod=%u supplementary-chown=%u nofollow=%u "
        "distinct=%u\n",
        first.closed_mode,
        first.closed_uid,
        first.closed_denied,
        second.closed_denied,
        second.chmod_denied,
        second.chown_denied,
        second.times_denied,
        second.now_denied,
        first.high_uid_denied,
        first.open_allowed,
        first.own_chmod,
        first.supplementary_chown,
        first.nofollow_preserved,
        distinct);
    return 0;
}
