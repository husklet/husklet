// /dev/shm as a REAL per-container tmpfs, the way `docker run` presents it: a segment created with
// shm_open() must (1) appear as a REGULAR FILE when /dev/shm is listed (postgres/ipcs/ops enumerate it),
// (2) stat as a regular file at /dev/shm/<name>, and (3) statfs("/dev/shm") reports TMPFS. Pre-fix hl
// backed shm at a FLAT host file under /tmp, so the segment worked but was INVISIBLE in /dev/shm (the
// listing was empty) and shared one global host namespace across containers. Runs in the alpine overlay
// rootfs (container semantics); verdict is a normalized ok=1 verified vs the docker (runc) oracle.
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <unistd.h>

#define TMPFS_MAGIC 0x01021994

int main(void) {
    const char *base = "hl_shm_vis";
    char nm[64];
    snprintf(nm, sizeof nm, "/%s", base);
    shm_unlink(nm);

    int fd = shm_open(nm, O_CREAT | O_RDWR, 0600);
    if (fd < 0 || ftruncate(fd, 4096) != 0) {
        printf("shm_visible ok=0 (shm_open failed)\n");
        return 0;
    }
    char *p = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (p != MAP_FAILED) { p[0] = 'x'; munmap(p, 4096); }
    close(fd);

    // (1) the segment is listed as a regular file in /dev/shm
    int listed = 0;
    DIR *d = opendir("/dev/shm");
    if (d) {
        struct dirent *e;
        while ((e = readdir(d)))
            if (!strcmp(e->d_name, base)) { listed = 1; break; }
        closedir(d);
    }
    // (2) /dev/shm/<name> stats as a regular file
    char full[80];
    snprintf(full, sizeof full, "/dev/shm/%s", base);
    struct stat st;
    int isreg = (stat(full, &st) == 0) && S_ISREG(st.st_mode) && st.st_size == 4096;
    // (3) statfs("/dev/shm") reports tmpfs
    struct statfs sf;
    int istmpfs = (statfs("/dev/shm", &sf) == 0) && (unsigned long)sf.f_type == TMPFS_MAGIC;

    shm_unlink(nm);
    printf("shm_visible ok=%d\n", (listed && isreg && istmpfs) ? 1 : 0);
    return 0;
}
