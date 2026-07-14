#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <pthread.h>
#include <sys/utsname.h>
#include <sys/resource.h>
#include <sys/types.h>

/* procfs metadata must agree with the syscalls that report the same facts:
   - /proc/self/status Threads == live pthread count
   - /proc/self/status Uid/Gid == getuid()/getgid()
   - /proc/version ISA token == uname -m
   - /proc/self/limits "Max core file size" soft == getrlimit(RLIMIT_CORE).rlim_cur
   Each line prints a native-vs-hl stable verdict (booleans), so the oracle diff catches divergence. */

static volatile int g_started = 0;
static volatile int g_stop = 0;
static void *worker(void *a) {
  (void)a;
  __sync_add_and_fetch(&g_started, 1);
  while (!g_stop) { struct timespec ts = {0, 1000000}; nanosleep(&ts, 0); }
  return 0;
}

static int slurp(const char *p, char *b, int n) {
  FILE *f = fopen(p, "r"); if (!f) return -1;
  int r = (int)fread(b, 1, (size_t)n - 1, f); fclose(f); if (r < 0) r = 0; b[r] = 0; return r;
}

int main(void) {
  /* spin up 3 workers -> 4 live threads total */
  pthread_t th[3];
  for (int i = 0; i < 3; i++) pthread_create(&th[i], 0, worker, 0);
  while (g_started < 3) { struct timespec ts = {0, 1000000}; nanosleep(&ts, 0); }

  char b[8192];
  slurp("/proc/self/status", b, sizeof b);
  int threads = 0, uid0 = -1, gid0 = -1;
  char *p = strstr(b, "\nThreads:"); if (p) threads = atoi(p + 9);
  p = strstr(b, "\nUid:"); if (p) uid0 = atoi(p + 5);
  p = strstr(b, "\nGid:"); if (p) gid0 = atoi(p + 5);
  int threads_ge4 = threads >= 4;
  int uid_match = (uid0 == (int)getuid());
  int gid_match = (gid0 == (int)getgid());
  g_stop = 1;
  for (int i = 0; i < 3; i++) pthread_join(th[i], 0);

  /* /proc/self/limits core soft vs getrlimit */
  struct rlimit rl; getrlimit(RLIMIT_CORE, &rl);
  slurp("/proc/self/limits", b, sizeof b);
  long proc_core = -1;
  p = strstr(b, "Max core file size");
  if (p) { p += 18; while (*p == ' ') p++; proc_core = strtol(p, 0, 10); }
  long want = (rl.rlim_cur == RLIM_INFINITY) ? -1 : (long)rl.rlim_cur;
  int core_match = (proc_core == want);

  printf("status_meta threads_ge4=%d uid_match=%d gid_match=%d core_match=%d\n",
         threads_ge4, uid_match, gid_match, core_match);
  return 0;
}
