#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

/* /proc/self/environ must match getenv(): programs compare the two, and helper processes scrape procfs.
   hl generated environ from raw HL_GUEST_ENV, omitting the engine defaults (HOME/LANG/…) it actually
   places on the stack. Check consistency for a few keys; native is trivially consistent, so is a fixed hl. */

static int environ_has(const char *key) {
  FILE *f = fopen("/proc/self/environ", "rb"); if (!f) return -1;
  char b[65536]; int r = (int)fread(b, 1, sizeof b - 1, f); fclose(f);
  if (r < 0) r = 0; b[r] = 0;
  size_t kl = strlen(key);
  for (int i = 0; i < r;) {
    const char *e = b + i;
    if (!strncmp(e, key, kl) && e[kl] == '=') return 1;
    i += (int)strlen(e) + 1;
  }
  return 0;
}

int main(void) {
  const char *keys[] = {"HOME", "LANG", "PATH", "USER", "TERM"};
  int consistent = 1;
  for (int i = 0; i < 5; i++) {
    int has = environ_has(keys[i]);
    int g = getenv(keys[i]) != NULL;
    if (has != g) consistent = 0;
  }
  printf("environ consistent=%d\n", consistent);
  return 0;
}
