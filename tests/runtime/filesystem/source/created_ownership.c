// Every guest-created node must record the CREATING guest uid, not the host uid the engine runs as.
// Root creates one node, a forked child drops to uid 2000/gid 2000 and creates one of each kind, and
// both then stat what they made. The case is only meaningful if the two creators really differ, so
// `distinct` asserts uid 0 != uid 2000 and that the child's drop actually took effect -- without it
// every field below would agree trivially and the case would pass against no ownership at all.
// A setgid directory owned by gid 3000 must lend that group to the child's file instead of gid 2000.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHILD_UID 2000
#define CHILD_GID 2000
#define SETGID_GID 3000

struct report {
    unsigned dir, file, link, fifo, setgid_group, live_uid, live_gid;
};

static int owner(const char *path, unsigned *user, unsigned *group) {
    struct stat status;
    if (lstat(path, &status) != 0) return -1;
    *user = status.st_uid;
    *group = status.st_gid;
    return 0;
}

static int create_all(struct report *out) {
    unsigned group = 0;
    if (mkdir("d", 0755) != 0 || owner("d", &out->dir, &group) != 0) return 1;
    int fd = open("f", O_CREAT | O_RDWR, 0644);
    if (fd < 0) return 2;
    close(fd);
    if (owner("f", &out->file, &group) != 0) return 3;
    if (symlink("f", "s") != 0 || owner("s", &out->link, &group) != 0) return 4;
    if (mknod("p", S_IFIFO | 0644, 0) != 0 || owner("p", &out->fifo, &group) != 0) return 5;
    // The setgid parent, not the creator's fsgid, decides the new file's group.
    if (open("sg/inherited", O_CREAT | O_RDWR, 0644) < 0) return 6;
    if (owner("sg/inherited", &group, &out->setgid_group) != 0) return 7;
    out->live_uid = getuid();
    out->live_gid = getgid();
    return 0;
}

int main(void) {
    char directory[] = "/tmp/hl-created-owner-XXXXXX";
    // mkdtemp gives 0700 and mkdir below is masked by the umask; the child must be able to
    // traverse and write here, which is a precondition of the case rather than part of it.
    if (!mkdtemp(directory) || chmod(directory, 0755) != 0 || chdir(directory) != 0) return 1;

    // Root's own creation, for the contrast the whole case rests on.
    int fd = open("root-file", O_CREAT | O_RDWR, 0644);
    if (fd < 0) return 2;
    close(fd);
    unsigned root_user = 0, root_group = 0;
    if (owner("root-file", &root_user, &root_group) != 0) return 3;

    if (mkdir("child", 0777) != 0 || chmod("child", 0777) != 0) return 4;
    if (mkdir("child/sg", 0777) != 0) return 5;
    if (chown("child/sg", CHILD_UID, SETGID_GID) != 0) return 6;
    if (chmod("child/sg", 02777) != 0) return 7;

    int channel[2];
    if (pipe(channel) != 0) return 8;
    pid_t child = fork();
    if (child < 0) return 9;
    if (child == 0) {
        close(channel[0]);
        struct report report;
        memset(&report, 0, sizeof report);
        if (chdir("child") != 0) _exit(20);
        if (setresgid(CHILD_GID, CHILD_GID, CHILD_GID) != 0) _exit(21);
        if (setresuid(CHILD_UID, CHILD_UID, CHILD_UID) != 0) _exit(22);
        int failure = create_all(&report);
        if (failure != 0) _exit(30 + failure);
        if (write(channel[1], &report, sizeof report) != (ssize_t)sizeof report) _exit(23);
        _exit(0);
    }
    close(channel[1]);
    struct report report;
    memset(&report, 0, sizeof report);
    ssize_t read_bytes = read(channel[0], &report, sizeof report);
    int status = 0;
    if (waitpid(child, &status, 0) != child) return 10;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return 11;
    if (read_bytes != (ssize_t)sizeof report) return 12;

    // Non-vacuity: the child really became a different uid, and root really stayed root.
    unsigned distinct = report.live_uid == CHILD_UID && root_user == 0 && report.live_uid != root_user;

    printf(
        "created-ownership root=%u child=%u dir=%u file=%u symlink=%u fifo=%u setgid-group=%u distinct=%u\n",
        root_user,
        report.live_uid,
        report.dir,
        report.file,
        report.link,
        report.fifo,
        report.setgid_group,
        distinct);
    return 0;
}
