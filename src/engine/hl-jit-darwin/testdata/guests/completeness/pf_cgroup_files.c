#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <fcntl.h>

/* cgroup v2 advertises controllers (cpuset cpu io memory pids); the standard controller files those
   controllers imply must exist and be readable. hl previously advertised the controllers but omitted many
   files, so a cgroup walk/probe failed. Print a native-vs-hl stable readable-boolean per file. */

static int readable(const char *p) {
  int fd = open(p, O_RDONLY);
  if (fd < 0) return 0;
  char b[256];
  int r = (int)read(fd, b, sizeof b);
  close(fd);
  return r >= 0; /* opens + reads without error (content is host-variant) */
}

int main(void) {
  const char *base = "/sys/fs/cgroup/";
  const char *files[] = {
    "cpuset.cpus.effective", "cpuset.mems.effective", "pids.peak",
    "memory.oom.group", "pids.events", "pids.events.local",
    "memory.swap.events", "memory.swap.peak", "cpu.stat.local",
  };
  int nfiles = (int)(sizeof files / sizeof files[0]);
  int ok = 1;
  char path[128];
  for (int i = 0; i < nfiles; i++) {
    snprintf(path, sizeof path, "%s%s", base, files[i]);
    if (!readable(path)) ok = 0;
  }
  printf("cgroup_files all_readable=%d n=%d\n", ok, nfiles);
  return 0;
}
