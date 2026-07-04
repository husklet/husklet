// Container CPU-count fidelity via the SYSFS-DIRECTORY path htop actually reads (regression #412 part 2).
// The earlier #412 probe (cpudefault.c) cross-checked sysconf / sched_getaffinity / /proc/cpuinfo /
// /sys/.../cpu/online -- all of which derive from the engine's container_online_cpus() and reported the
// host count correctly. But htop's LinuxMachine_updateCPUcount sizes its CPU meters a DIFFERENT way: it
// opendir()s /sys/devices/system/cpu, counts the cpuN SUBDIRECTORIES, and opens each cpuN/online to mark
// it active; finding NO cpuN dir it early-returns and keeps its built-in default of ONE CPU. macOS has no
// /sys, and the engine served only the online/possible/present FILES -- never the directory -- so htop
// showed 1 CPU on a many-core host. This probe reproduces htop's exact algorithm (opendir + per-cpuN
// openat + read of cpuN/online) plus glibc get_nprocs_conf() (which likewise counts cpuN dirs), so a
// missing directory makes it under-report. Run with .oracle(): the JIT count must byte-match the native
// host count. Fails-before (dir absent -> htop_cpus=1), passes-after (dir materialized -> host count).
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <fcntl.h>
#include <dirent.h>
#include <sys/sysinfo.h> // get_nprocs / get_nprocs_conf

// htop's LinuxMachine_updateCPUcount, faithfully: enumerate cpuN dirs, open each as a directory (skip on
// failure), then openat(cpuN,"online") -- a missing/nonzero "online" counts the CPU as active.
static int htop_cpucount(void) {
    DIR *d = opendir("/sys/devices/system/cpu");
    if (!d) return 1; // htop's fallback default when the dir cannot be opened
    int active = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strncmp(e->d_name, "cpu", 3) != 0) continue;
        char *endp;
        unsigned long id = strtoul(e->d_name + 3, &endp, 10);
        (void)id;
        if (endp == e->d_name + 3 || *endp != '\0') continue; // cpufreq/cpuidle/... are not cpuN
        int cfd = openat(dirfd(d), e->d_name, O_RDONLY | O_DIRECTORY);
        if (cfd < 0) continue; // htop: `continue` -- an unopenable cpuN is NOT counted
        char buf[8];
        int of = openat(cfd, "online", O_RDONLY);
        ssize_t r = (of >= 0) ? read(of, buf, sizeof buf) : -1;
        if (of >= 0) close(of);
        if (r < 1 || buf[0] != '0') active++; // absent/"1" -> active (real Linux cpuN has no online file)
        close(cfd);
    }
    closedir(d);
    if (active < 1) active = 1; // htop keeps its default of 1 when it finds no CPU
    return active;
}

int main(void) {
    printf("htop_cpus=%d get_nprocs=%d get_nprocs_conf=%d\n",
           htop_cpucount(), get_nprocs(), get_nprocs_conf());
    return 0;
}
