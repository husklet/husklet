// Sticky-directory removal policy, the check that only became expressible once CAP_FOWNER stopped
// being permanently held. In a 01777 directory a name may be removed or renamed only by the owner
// of the entry, the owner of the directory, or a privileged task. Two unprivileged uids share the
// directory, so every denial below distinguishes them from each other rather than from root.
// `distinct` is the non-vacuity guard: two children that both failed to drop privilege would own
// each other's files and satisfy every expectation without any sticky check existing at all.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define OWNER_UID 2000
#define OTHER_UID 3000

struct report {
    unsigned live_uid;
    unsigned made, unlink_denied, rename_denied, own_unlink, plain_unlink, file_uid;
};

static int drop(unsigned uid) {
    if (setresgid(uid, uid, uid) != 0) return 1;
    return setresuid(uid, uid, uid) != 0;
}

// The first uid populates both directories; nothing here may be denied.
static void owner_child(struct report *out) {
    int fd = open("sticky/theirs", O_CREAT | O_RDWR, 0644);
    if (fd >= 0) close(fd);
    fd = open("sticky/mine", O_CREAT | O_RDWR, 0644);
    if (fd >= 0) close(fd);
    fd = open("plain/theirs", O_CREAT | O_RDWR, 0644);
    if (fd >= 0) close(fd);
    struct stat status;
    if (lstat("sticky/theirs", &status) == 0) out->file_uid = status.st_uid;
    out->made = access("sticky/theirs", F_OK) == 0 && access("plain/theirs", F_OK) == 0;
    out->live_uid = getuid();
}

// The second uid owns neither the sticky directory nor the files another uid left in it.
static void other_child(struct report *out) {
    out->unlink_denied = unlink("sticky/theirs") != 0 && errno == EPERM;
    out->rename_denied = rename("sticky/theirs", "sticky/moved") != 0 && errno == EPERM;
    // The same file in a non-sticky world-writable directory stays removable, so the denial above
    // is the sticky bit and not a blanket refusal.
    out->plain_unlink = unlink("plain/theirs") == 0;
    // A file this uid created itself is always its own to remove, sticky or not.
    int fd = open("sticky/ours", O_CREAT | O_RDWR, 0644);
    if (fd >= 0) close(fd);
    out->own_unlink = unlink("sticky/ours") == 0;
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
        if (drop(uid) != 0) _exit(21);
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
    char directory[] = "/tmp/hl-sticky-XXXXXX";
    if (!mkdtemp(directory)) return 1;
    if (chmod(directory, 0755) != 0) return 2;
    if (chdir(directory) != 0) return 1;
    // Explicit modes, because the umask would otherwise decide what this case is testing. Both
    // directories stay root-owned, so neither child can pass the directory-owner escape.
    if (mkdir("sticky", 0700) != 0 || chmod("sticky", 01777) != 0) return 3;
    if (mkdir("plain", 0700) != 0 || chmod("plain", 0777) != 0) return 4;

    struct report owner, other;
    memset(&owner, 0, sizeof owner);
    memset(&other, 0, sizeof other);
    if (run(OWNER_UID, owner_child, &owner) != 0) return 5;
    if (run(OTHER_UID, other_child, &other) != 0) return 6;

    // Root holds CAP_FOWNER and owns neither the directory nor the file once the directory is
    // handed to the first uid, so only the privilege exemption can still let it remove the name.
    if (chown("sticky", OWNER_UID, OWNER_UID) != 0) return 7;
    unsigned root_unlink = unlink("sticky/theirs") == 0;

    // Non-vacuity: the two children really became different, unprivileged, distinct uids, and the
    // file the second was refused really belongs to the first.
    unsigned distinct = owner.live_uid == OWNER_UID && other.live_uid == OTHER_UID &&
                        owner.live_uid != other.live_uid && (unsigned)getuid() == 0 && owner.file_uid == OWNER_UID;

    printf("sticky-dir made=%u unlink=%u rename=%u plain=%u own=%u root=%u distinct=%u\n", owner.made,
           other.unlink_denied, other.rename_denied, other.plain_unlink, other.own_unlink, root_unlink, distinct);
    return 0;
}
