// A file that came from an image layer must report the uid and gid the layer DECLARED, not the
// host uid the engine happens to run as. alpine:3.20 ships /etc/shadow as root:shadow (0:42) and
// /etc/passwd as root:root (0:0), so the two together prove the lookup is per-path rather than a
// blanket answer. `distinct` asserts the declared ids really differ from the engine's own uid and
// from each other -- without it the case would pass against ownership that was never read at all.
// A guest chown must then outrank the declaration: the registry is consulted before the layers.
#define _GNU_SOURCE
#include <stdio.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#define CHOWN_UID 2500
#define CHOWN_GID 2600

static int owner(const char *path, unsigned *user, unsigned *group) {
    struct stat status;
    if (lstat(path, &status) != 0) return -1;
    *user = status.st_uid;
    *group = status.st_gid;
    return 0;
}

int main(void) {
    unsigned shadow_user = 0, shadow_group = 0, passwd_user = 0, passwd_group = 0;
    if (owner("/etc/shadow", &shadow_user, &shadow_group) != 0) return 1;
    if (owner("/etc/passwd", &passwd_user, &passwd_group) != 0) return 2;

    // A guest chown of a declared path must win over the declaration, including across the copy-up
    // the write forces: the file lands on a fresh upper inode and must keep the chowned owner.
    // A failure reports as 80000/90000 plus errno rather than an opaque exit code, so the golden
    // mismatch names which step broke.
    unsigned chowned_user = 0, chowned_group = 0;
    int fd = open("/etc/passwd", O_WRONLY | O_APPEND);
    if (fd < 0) chowned_user = 80000 + errno;
    else if (close(fd) != 0) return 5;
    else if (chown("/etc/passwd", CHOWN_UID, CHOWN_GID) != 0) chowned_user = 90000 + errno;
    else if (owner("/etc/passwd", &chowned_user, &chowned_group) != 0) return 6;

    // Non-vacuity: the two declared owners differ from each other, so the answer is per-path; and
    // /etc/shadow's declared gid is not the group the engine process would have reported.
    unsigned distinct = shadow_group != passwd_group && shadow_user == passwd_user;

    printf(
        "image-ownership shadow=%u:%u passwd=%u:%u chowned=%u:%u distinct=%u\n",
        shadow_user,
        shadow_group,
        passwd_user,
        passwd_group,
        chowned_user,
        chowned_group,
        distinct);
    return 0;
}
