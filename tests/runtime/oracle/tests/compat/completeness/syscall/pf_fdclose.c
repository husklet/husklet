#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

/* After close(fd), Linux makes /proc/self/fd/N disappear -- lstat/access/readlink all ENOENT. hl reported
   the closed fd as still live via lstat/access (only readlink ENOENT'd), creating stale fd state. */

int main(void) {
  int fd = open("/dev/null", O_RDONLY);
  if (fd < 0) { printf("fdclose open_failed\n"); return 0; }
  close(fd); /* no intervening open -> fd stays free */
  char p[64];
  snprintf(p, sizeof p, "/proc/self/fd/%d", fd);
  struct stat st;
  int lstat_enoent = (lstat(p, &st) == -1 && errno == ENOENT);
  int access_enoent = (access(p, F_OK) == -1 && errno == ENOENT);
  char lb[128];
  int readlink_enoent = (readlink(p, lb, sizeof lb) == -1 && errno == ENOENT);
  printf("fdclose lstat_enoent=%d access_enoent=%d readlink_enoent=%d\n",
         lstat_enoent, access_enoent, readlink_enoent);
  return 0;
}
