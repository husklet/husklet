#include "compat.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Linux carries far more than 255 argv entries (up to ARG_MAX bytes) across execve. hl truncated at 255,
   silently running a different command and diverging /proc/self/cmdline. Re-exec self with 300 extra args
   and, in the child, print the observed argc + the /proc/self/cmdline arg count. Oracle-diff vs native. */

#define NEXTRA 300

static int cmdline_count(void) {
  FILE *f = fopen("/proc/self/cmdline", "r"); if (!f) return -1;
  char b[65536]; int r = (int)fread(b, 1, sizeof b, f); fclose(f);
  if (r <= 0) return 0;
  int n = 0; for (int i = 0; i < r; i++) if (b[i] == 0) n++;
  return n;
}

int main(int argc, char **argv) {
  if (argc >= 2 && !strcmp(argv[1], "child")) {
    /* re-exec'd form: argc should be 2 + NEXTRA (self + "child" + NEXTRA args) */
    int cc = cmdline_count();
    /* threshold well above the old 255 truncation / 1-arg fallback, tolerant of a ±1 oracle counting quirk */
    printf("argv re_argc=%d cmdline_ge=%d\n", argc, cc >= 290);
    return 0;
  }
  /* parent: exec self with many args */
  char self[4096];
  ssize_t sl = readlink("/proc/self/exe", self, sizeof self - 1);
  if (sl <= 0) { /* fall back to argv[0] */ snprintf(self, sizeof self, "%s", argv[0]); }
  else self[sl] = 0;

  char **nv = calloc(2 + NEXTRA + 1, sizeof(char *));
  int k = 0;
  nv[k++] = self;
  nv[k++] = (char *)"child";
  for (int i = 0; i < NEXTRA; i++) { char *a = malloc(16); snprintf(a, 16, "arg%d", i); nv[k++] = a; }
  nv[k] = NULL;
  execv(self, nv);
  perror("execv");
  return 1;
}
