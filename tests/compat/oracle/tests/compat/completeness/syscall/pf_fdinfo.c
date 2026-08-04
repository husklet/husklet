#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <dirent.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

/* Linux exposes /proc/self/fdinfo (a dir) and /proc/self/fdinfo/<N> (pos/flags/mnt_id per fd). hl omitted
   it entirely while exposing /proc/self/fd. Check the dir + an entry for a known-open fd. Oracle-diff. */

int main(void) {
  int fd = open("/dev/null", O_RDONLY);
  struct stat st;
  int dir_ok = (stat("/proc/self/fdinfo", &st) == 0 && S_ISDIR(st.st_mode));
  DIR *d = opendir("/proc/self/fdinfo"); if (d) closedir(d); else dir_ok = 0;

  char p[64]; snprintf(p, sizeof p, "/proc/self/fdinfo/%d", fd);
  int entry_ok = 0, listed = 0;
  int f = open(p, O_RDONLY);
  if (f >= 0) { char b[256]; int r = (int)read(f, b, sizeof b - 1); if (r > 0) { b[r] = 0; entry_ok = strstr(b, "pos:") && strstr(b, "flags:"); } close(f); }
  char want[16]; snprintf(want, sizeof want, "%d", fd);
  d = opendir("/proc/self/fdinfo");
  if (d) { struct dirent *e; while ((e = readdir(d))) if (!strcmp(e->d_name, want)) { listed = 1; break; } closedir(d); }
  close(fd);
  printf("fdinfo dir_ok=%d entry_ok=%d listed=%d\n", dir_ok, entry_ok, listed);
  return 0;
}
