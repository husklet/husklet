// statfs(2) f_type + pseudo-fs geometry inside a container. Runs in the alpine overlay rootfs; asserts the
// SAME invariants real docker (runc) presents so `stat -f -c %T` and `df -h` behave. Pre-fix dd stamped
// TMPFS_MAGIC + real host geometry on EVERY path -> `/` looked like tmpfs, /proc & /sys reported a huge
// bogus size (so df listed them). ok=1 iff every mount classifies correctly and the pseudo-fs report zero.
#include <stdio.h>
#include <sys/vfs.h>

#define OVERLAYFS 0x794c7630
#define PROCFS    0x9fa0
#define SYSFS     0x62656572
#define TMPFS     0x01021994

static int consistent(const struct statfs *s) {
    return s->f_bavail <= s->f_bfree && s->f_bfree <= s->f_blocks && s->f_namelen == 255;
}

int main(void) {
    struct statfs root, proc, sys, dev;
    if (statfs("/", &root) || statfs("/proc", &proc) || statfs("/sys", &sys) || statfs("/dev", &dev)) {
        printf("statfs-type ok=0 (statfs failed)\n");
        return 0;
    }
    int ok =
        (unsigned long)root.f_type == OVERLAYFS && root.f_blocks > 0 && consistent(&root) &&
        (unsigned long)proc.f_type == PROCFS && proc.f_blocks == 0 && proc.f_files == 0 &&
        (unsigned long)sys.f_type == SYSFS && sys.f_blocks == 0 &&
        (unsigned long)dev.f_type == TMPFS && consistent(&dev);
    printf("statfs-type ok=%d\n", ok ? 1 : 0);
    return 0;
}
