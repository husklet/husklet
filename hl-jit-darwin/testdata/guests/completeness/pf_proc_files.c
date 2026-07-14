#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

/* Common procfs leaves that must exist / carry their standard fields, matching a real Linux container.
   Prints existence/field booleans so the oracle diff (native/qemu vs hl) catches an absent or sparse leaf. */

static int slurp(const char *p, char *b, int n) {
  FILE *f = fopen(p, "r"); if (!f) return -1;
  int r = (int)fread(b, 1, (size_t)n - 1, f); fclose(f); if (r < 0) r = 0; b[r] = 0; return r;
}

int main(void) {
  char b[16384];

  int io_ok = slurp("/proc/self/io", b, sizeof b) >= 0 && strstr(b, "rchar") && strstr(b, "read_bytes");

  int sockstat_ok = slurp("/proc/net/sockstat", b, sizeof b) >= 0 && strstr(b, "sockets:") && strstr(b, "TCP:");

  int devblock_loop = 0;
  if (slurp("/proc/devices", b, sizeof b) >= 0) {
    char *blk = strstr(b, "Block devices:");
    if (blk && strstr(blk, "loop")) devblock_loop = 1;
  }

  int mem_fields = 0;
  if (slurp("/proc/meminfo", b, sizeof b) >= 0)
    mem_fields = strstr(b, "AnonPages:") && strstr(b, "Active:") && strstr(b, "Dirty:") &&
                 strstr(b, "Inactive:") ? 1 : 0;

  int stat_counters = 0;
  if (slurp("/proc/stat", b, sizeof b) >= 0) {
    char *ip = strstr(b, "\nintr "), *cp = strstr(b, "\nctxt ");
    long iv = ip ? strtol(ip + 6, 0, 10) : 0, cv = cp ? strtol(cp + 6, 0, 10) : 0;
    stat_counters = (iv > 0 && cv > 0);
  }

  printf("proc_files io=%d sockstat=%d devblock_loop=%d mem_fields=%d stat_counters=%d\n",
         io_ok, sockstat_ok, devblock_loop, mem_fields, stat_counters);
  return 0;
}
