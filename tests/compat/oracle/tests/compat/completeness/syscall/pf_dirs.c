#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <dirent.h>
#include <sys/stat.h>
#include <unistd.h>

/* Synthetic directories whose direct leaves exist but whose DIRECTORY was not enumerable under hl:
   /proc/net, cpuN/topology, /sys/fs/cgroup, /sys/class/block, /sys/block, /proc/self/ns, /dev/fd.
   A tool walks these before opening leaves, so each must opendir + stat like real Linux. */

static int dir_ok(const char *p) { /* opendir succeeds AND stat says directory */
  struct stat st; if (stat(p, &st) != 0 || !S_ISDIR(st.st_mode)) return 0;
  DIR *d = opendir(p); if (!d) return 0; closedir(d); return 1;
}
static int dir_has(const char *p, const char *name) {
  DIR *d = opendir(p); if (!d) return 0;
  struct dirent *e; int f = 0;
  while ((e = readdir(d))) if (!strcmp(e->d_name, name)) { f = 1; break; }
  closedir(d); return f;
}

int main(void) {
  int procnet = dir_ok("/proc/net") && dir_has("/proc/net", "tcp") && dir_has("/proc/net", "dev");
  int topo = dir_ok("/sys/devices/system/cpu/cpu0/topology") &&
             dir_has("/sys/devices/system/cpu/cpu0/topology", "core_id");
  int cgroup = dir_ok("/sys/fs/cgroup") && dir_has("/sys/fs/cgroup", "cgroup.controllers");
  int sysblock = dir_ok("/sys/class/block") && dir_ok("/sys/block");
  char lb[64];
  int ns = dir_ok("/proc/self/ns") && dir_has("/proc/self/ns", "net") &&
           readlink("/proc/self/ns/net", lb, sizeof lb) > 0;
  int devfd = 0; { DIR *d = opendir("/dev/fd"); if (d) { devfd = 1; closedir(d); } }

  printf("dirs procnet=%d topo=%d cgroup=%d sysblock=%d ns=%d devfd=%d\n",
         procnet, topo, cgroup, sysblock, ns, devfd);
  return 0;
}
