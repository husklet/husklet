#include "compat.h"
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>

/* Linux accepts writes to /dev/urandom and /dev/random as entropy-pool seeding and returns the byte count;
   hl returned EPERM. Entropy-seeding probes rely on this. Print the write result, oracle-diffed vs native. */

static int seed_ok(const char *p) {
  int fd = open(p, O_WRONLY);
  if (fd < 0) { fd = open(p, O_RDWR); }
  if (fd < 0) return 0;
  const char buf[8] = {1, 2, 3, 4, 5, 6, 7, 8};
  ssize_t w = write(fd, buf, sizeof buf);
  close(fd);
  return w == (ssize_t)sizeof buf;
}

int main(void) {
  printf("devseed urandom=%d random=%d\n", seed_ok("/dev/urandom"), seed_ok("/dev/random"));
  return 0;
}
