// statfs(2) f_type + pseudo-fs geometry inside a container. Runs in the alpine overlay rootfs; asserts the
// SAME invariants real docker (runc) presents so `stat -f -c %T` and `df -h` behave. Pre-fix hl stamped
// TMPFS_MAGIC + real host geometry on EVERY path -> `/` looked like tmpfs, /proc & /sys reported a huge
// bogus size (so df listed them). ok=1 iff every mount classifies correctly and the pseudo-fs report zero.
#include <stdio.h>
#include <sys/vfs.h>

#define OVERLAYFS 0x794c7630
#define PROCFS    0x9fa0
#define SYSFS     0x62656572
#define TMPFS     0x01021994
#define CGROUP2   0x63677270   // the JVM/systemd/runc detect a v2 unified hierarchy by this statfs magic

static int consistent(const struct statfs *s) {
    return s->f_bavail <= s->f_bfree && s->f_bfree <= s->f_blocks && s->f_namelen == 255;
}

int main(void) {
    struct statfs root, proc, sys, dev, cg;
    if (statfs("/", &root) || statfs("/proc", &proc) || statfs("/sys", &sys) || statfs("/dev", &dev) ||
        statfs("/sys/fs/cgroup", &cg)) {
        printf("statfs-type ok=0 (statfs failed)\n");
        return 0;
    }
    int ok =
        (unsigned long)root.f_type == OVERLAYFS && root.f_blocks > 0 && consistent(&root) &&
        (unsigned long)proc.f_type == PROCFS && proc.f_blocks == 0 && proc.f_files == 0 &&
        (unsigned long)sys.f_type == SYSFS && sys.f_blocks == 0 &&
        (unsigned long)dev.f_type == TMPFS && consistent(&dev) &&
        // the cgroup2 mount: CGROUP2 magic + zero pseudo-fs geometry (a runtime keys UseContainerSupport
        // v2-detection on exactly this magic; df must hide it like the other pseudo mounts).
        (unsigned long)cg.f_type == CGROUP2 && cg.f_blocks == 0 && cg.f_files == 0;
    printf("statfs-type ok=%d\n", ok ? 1 : 0);
    return 0;
}
