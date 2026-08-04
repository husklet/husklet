#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <dirent.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <pthread.h>
#include <time.h>

/* /proc/self/task must enumerate EVERY live thread (not just the main tid), and each listed task dir plus
   its per-tid files (stat/status) must be stat/openable -- Linux lets a task walker descend and read them.
   Prints native-vs-hl stable verdicts. */

static volatile int g_started = 0, g_stop = 0;
static void *worker(void *a) {
  (void)a; __sync_add_and_fetch(&g_started, 1);
  while (!g_stop) { struct timespec ts = {0, 1000000}; nanosleep(&ts, 0); }
  return 0;
}

int main(void) {
  pthread_t th[3];
  for (int i = 0; i < 3; i++) pthread_create(&th[i], 0, worker, 0);
  while (g_started < 3) { struct timespec ts = {0, 1000000}; nanosleep(&ts, 0); }

  int entries = 0, all_files_ok = 1;
  DIR *d = opendir("/proc/self/task");
  if (d) {
    struct dirent *e;
    while ((e = readdir(d))) {
      if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
      entries++;
      char p[128]; struct stat st;
      snprintf(p, sizeof p, "/proc/self/task/%s", e->d_name);
      if (stat(p, &st) != 0) all_files_ok = 0;
      snprintf(p, sizeof p, "/proc/self/task/%s/status", e->d_name);
      if (stat(p, &st) != 0) all_files_ok = 0;
      int fd = open(p, O_RDONLY);
      if (fd < 0) all_files_ok = 0; else close(fd);
    }
    closedir(d);
  }
  g_stop = 1;
  for (int i = 0; i < 3; i++) pthread_join(th[i], 0);

  int entries_ge4 = entries >= 4;
  printf("taskdir entries_ge4=%d files_ok=%d\n", entries_ge4, all_files_ok);
  return 0;
}
