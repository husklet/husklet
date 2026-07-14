#include "compat.h"
#include <stdio.h>
#include <string.h>

/* Static-PIE binaries expose read-only (RELRO/rodata) load segments in /proc/self/maps -- an r--p row for
   the executable image. hl was reported to emit only r-xp and rw-p. Check for an r--p executable-image row. */

int main(void) {
  FILE *f = fopen("/proc/self/maps", "r"); if (!f) { printf("maps_relro open_failed\n"); return 0; }
  char line[512]; int has_ro = 0;
  while (fgets(line, sizeof line, f)) {
    /* a private read-only mapping: perms field (col 2) == "r--p" */
    char *sp = strchr(line, ' ');
    if (sp && !strncmp(sp + 1, "r--p", 4)) has_ro = 1;
  }
  fclose(f);
  printf("maps_relro has_ro=%d\n", has_ro);
  return 0;
}
