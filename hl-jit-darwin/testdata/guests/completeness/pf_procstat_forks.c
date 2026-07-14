#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <sys/wait.h>

/* /proc/stat `processes` is a cumulative fork counter since boot, so forking N children raises it by >= N.
   hl previously reported the live registry count (flat). Print delta_ge_n so the oracle diff matches. */

static long procs_field(void) {
  FILE *f = fopen("/proc/stat", "r"); if (!f) return -1;
  char b[16384]; int r = (int)fread(b, 1, sizeof b - 1, f); fclose(f);
  if (r < 0) r = 0; b[r] = 0;
  char *p = strstr(b, "\nprocesses "); if (!p) return -1;
  return strtol(p + 11, 0, 10);
}

int main(void) {
  const int N = 8;
  long before = procs_field();
  for (int i = 0; i < N; i++) {
    pid_t pid = fork();
    if (pid == 0) _exit(0);
    if (pid > 0) waitpid(pid, 0, 0);
  }
  long after = procs_field();
  int delta_ge_n = (before >= 0 && after >= 0 && (after - before) >= N);
  printf("procstat_forks delta_ge_n=%d\n", delta_ge_n);
  return 0;
}
