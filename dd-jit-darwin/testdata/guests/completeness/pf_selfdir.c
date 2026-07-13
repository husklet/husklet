#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <dirent.h>

/* Files that direct open/stat already serve must ALSO appear in `ls /proc/self` -- tools discover proc
   files via readdir before opening them, so a listing that omits openable files breaks discovery. */

static int listed(const char *name) {
  DIR *d = opendir("/proc/self");
  if (!d) return 0;
  struct dirent *e; int found = 0;
  while ((e = readdir(d))) if (!strcmp(e->d_name, name)) { found = 1; break; }
  closedir(d);
  return found;
}

int main(void) {
  const char *names[] = {"mountinfo", "limits", "environ", "smaps", "pagemap"};
  int ok = 1;
  for (int i = 0; i < 5; i++) if (!listed(names[i])) ok = 0;
  printf("selfdir all_listed=%d\n", ok);
  return 0;
}
