// Deterministic contract adapter for ext_iso/cpusysfs.c.
// The legacy source remains byte-identical; this derived probe checks htop's cpuN-directory algorithm
// against both glibc CPU-count APIs without printing the host-dependent count.
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sysinfo.h>
#include <unistd.h>

static int htop_cpucount(void) {
    DIR *dir = opendir("/sys/devices/system/cpu");
    if (!dir) return 1;
    int active = 0;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strncmp(entry->d_name, "cpu", 3) != 0) continue;
        char *end;
        (void)strtoul(entry->d_name + 3, &end, 10);
        if (end == entry->d_name + 3 || *end != 0) continue;
        int cpu = openat(dirfd(dir), entry->d_name, O_RDONLY | O_DIRECTORY);
        if (cpu < 0) continue;
        char buf[8];
        int online = openat(cpu, "online", O_RDONLY);
        ssize_t n = online >= 0 ? read(online, buf, sizeof buf) : -1;
        if (online >= 0) close(online);
        if (n < 1 || buf[0] != '0') active++;
        close(cpu);
    }
    closedir(dir);
    return active < 1 ? 1 : active;
}
int main(void) {
    int htop = htop_cpucount(), online = get_nprocs(), configured = get_nprocs_conf();
    printf("cpu-sysfs-dirs consistent=%d multicore=%d\n",
           htop == online && online == configured && htop >= 1, htop >= 2);
    return 0;
}
