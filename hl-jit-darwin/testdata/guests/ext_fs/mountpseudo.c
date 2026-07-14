// Pseudo-mount COMPLETENESS of /proc/mounts + /proc/self/mountinfo inside the alpine overlay rootfs. The
// storage audit found hl omitted the /dev/shm, /dev/pts and /dev/mqueue mounts, listed no cgroup2 line in
// /proc/mounts, and marked sysfs rw where real docker (runc) marks it ro. This asserts each line docker's
// oracle shows, in BOTH the fstab form (/proc/mounts) and the "-"-separated mountinfo form. Verdict is a
// normalized ok=1 (host-variant fields like sizes/ids are not asserted); a stub/incomplete synth fails.
#include <stdio.h>
#include <string.h>

static int slurp(const char *f, char *buf, size_t n) {
    FILE *fp = fopen(f, "r");
    if (!fp) return -1;
    size_t got = fread(buf, 1, n - 1, fp);
    buf[got] = 0;
    fclose(fp);
    return (int)got;
}

int main(void) {
    char mounts[8192], minfo[8192];
    if (slurp("/proc/mounts", mounts, sizeof mounts) < 0 ||
        slurp("/proc/self/mountinfo", minfo, sizeof minfo) < 0) {
        printf("mountpseudo ok=0 (read failed)\n");
        return 0;
    }
    int ok =
        // /proc/mounts (fstab form): the pseudo-mounts df/mount/findmnt enumerate.
        strstr(mounts, "shm /dev/shm tmpfs ") != NULL &&
        strstr(mounts, "devpts /dev/pts devpts ") != NULL &&
        strstr(mounts, "mqueue /dev/mqueue mqueue ") != NULL &&
        strstr(mounts, "cgroup /sys/fs/cgroup cgroup2 ") != NULL &&
        // sysfs is READ-ONLY under runc (ro immediately after the mountpoint), never rw.
        strstr(mounts, "sysfs /sys sysfs ro,") != NULL &&
        strstr(mounts, "sysfs /sys sysfs rw") == NULL &&
        // /proc/self/mountinfo ("-"-separated form) lists the same set.
        strstr(minfo, " - tmpfs shm ") != NULL &&
        strstr(minfo, " - devpts devpts ") != NULL &&
        strstr(minfo, " - mqueue mqueue ") != NULL &&
        strstr(minfo, " - cgroup2 cgroup ") != NULL &&
        // the /sys mountinfo line is ro (both the per-mount flags and the sysfs superblock opts).
        strstr(minfo, " /sys ro,") != NULL &&
        strstr(minfo, " - sysfs sysfs ro") != NULL;
    printf("mountpseudo ok=%d\n", ok ? 1 : 0);
    return 0;
}
