// Container cgroup-v2 fidelity (the surface real runtimes SIZE THEMSELVES from). The JVM
// (-XX:+UseContainerSupport) reads memory.max + cpu.max to pick MaxHeap and availableProcessors; the Go
// runtime derives GOMAXPROCS from cpu.max's quota/period; systemd/Node detect the unified hierarchy via
// the cgroup2 statfs magic + cgroup.controllers/cgroup.type. If hl's cgroup files are absent or wrong,
// these apps mis-size heaps and thread pools (OOM or under-parallelism).
//
// This probe prints a NORMALIZED, host-INDEPENDENT verdict that is byte-identical between real Linux
// (docker/runc) and a correct hl: the v2 markers plus the sizing-critical file CONTENTS (cpu.max /
// memory.max / memory.swap.max / memory.high / cpu.weight / pids.max), which reflect the --cpus/--memory
// caps exactly. Live accounting counters (memory.current, cpu.stat) are host-variant and NOT asserted.
// Linux-only by construction: cgroup is a kernel concept with no darwin analogue.
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>

// Read a cgroup file, trim the trailing newline(s), return "ENOENT" if absent.
static const char *slurp(const char *path, char *buf, int cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return "ENOENT";
    int n = (int)read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) return "ERR";
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == '\r')) n--;
    buf[n] = 0;
    return buf;
}

int main(void) {
    char b1[256], b2[256], b3[256], b4[256], b5[256], b6[256], b7[256];

    // v2 unified-hierarchy detection the way the JVM's CgroupSubsystemFactory actually does it: parse
    // /proc/self/mountinfo for a "cgroup2" filesystem mounted at /sys/fs/cgroup. (The statfs CGROUP2 magic
    // is asserted separately by fs-statfs-type, which runs in the real overlay rootfs.) This signal works
    // for a bare guest AND a container, and is exactly what a real runtime reads to pick the v2 code path.
    int cgv2 = 0;
    {
        char mi[16384];
        const char *m = slurp("/proc/self/mountinfo", mi, sizeof mi);
        for (const char *p = m; (p = strstr(p, " cgroup2 ")); p++) { cgv2 = 1; break; }
    }

    // cgroup.controllers must advertise at least cpu + memory (what runtimes gate on).
    const char *ctl = slurp("/sys/fs/cgroup/cgroup.controllers", b1, sizeof b1);
    int ctl_ok = (strstr(ctl, "cpu") && strstr(ctl, "memory")) ? 1 : 0;

    const char *type = slurp("/sys/fs/cgroup/cgroup.type", b2, sizeof b2);

    // v1 fallback must be ABSENT on a pure-v2 host (runtimes that probe it must fall through to v2, not
    // read a fabricated v1 limit). Real docker/OrbStack is pure-v2 -> ENOENT.
    int v1_absent = (access("/sys/fs/cgroup/memory/memory.limit_in_bytes", F_OK) != 0) ? 1 : 0;

    printf("cgv2=%d controllers=%s type=%s v1absent=%d "
           "cpu.max=[%s] mem.max=[%s] mem.swap.max=[%s] mem.high=[%s] cpu.weight=[%s] pids.max=[%s]\n",
           cgv2, ctl_ok ? "OK" : ctl, type, v1_absent,
           slurp("/sys/fs/cgroup/cpu.max", b3, sizeof b3),
           slurp("/sys/fs/cgroup/memory.max", b4, sizeof b4),
           slurp("/sys/fs/cgroup/memory.swap.max", b5, sizeof b5),
           slurp("/sys/fs/cgroup/memory.high", b6, sizeof b6),
           slurp("/sys/fs/cgroup/cpu.weight", b7, sizeof b7),
           slurp("/sys/fs/cgroup/pids.max", b1, sizeof b1));
    return 0;
}
