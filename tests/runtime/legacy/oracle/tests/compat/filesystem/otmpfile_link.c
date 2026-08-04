// O_TMPFILE: an anonymous unnamed inode, later materialized with linkat via /proc/self/fd.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_otmpfile_%d", (int)getpid());
    mkdir(dir, 0755);

    int fd = open(dir, O_TMPFILE | O_RDWR, 0644);
    int supported = fd >= 0;
    int wrote = 0, has_no_name = 0, materialized = 0, content_ok = 0, nlink_ok = 0;
    if (supported) {
        wrote = write(fd, "tmpfile-body", 12) == 12;
        // Before linking the inode has zero links.
        struct stat st;
        has_no_name = fstat(fd, &st) == 0 && st.st_nlink == 0;

        char proc[64], target[192];
        snprintf(proc, sizeof proc, "/proc/self/fd/%d", fd);
        snprintf(target, sizeof target, "%s/materialized", dir);
        materialized = linkat(AT_FDCWD, proc, AT_FDCWD, target, AT_SYMLINK_FOLLOW) == 0;

        if (materialized) {
            char buf[16] = {0};
            int rf = open(target, O_RDONLY);
            content_ok = rf >= 0 && read(rf, buf, 12) == 12 && memcmp(buf, "tmpfile-body", 12) == 0;
            if (rf >= 0) close(rf);
            struct stat ls;
            nlink_ok = stat(target, &ls) == 0 && ls.st_nlink == 1;
            unlink(target);
        }
        close(fd);
    }
    rmdir(dir);
    printf("otmpfile-link supported=%d wrote=%d nlink0=%d materialized=%d content=%d nlink1=%d\n",
           supported, wrote, has_no_name, materialized, content_ok, nlink_ok);
    return 0;
}
