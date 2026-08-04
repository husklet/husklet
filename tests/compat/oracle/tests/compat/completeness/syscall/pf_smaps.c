#include "compat.h"
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <fcntl.h>
#include <unistd.h>

/* Reading /proc/self/smaps must return promptly (memory profilers, redis COW self-test). hl was reported to
   hang. Map a few regions, then read the whole file; print a bounded verdict. A hang -> harness timeout. */

int main(void) {
  for (int i = 0; i < 8; i++)
    mmap(0, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  int fd = open("/proc/self/smaps", O_RDONLY);
  if (fd < 0) { printf("smaps open_failed\n"); return 0; }
  long total = 0; char b[4096]; ssize_t r;
  while ((r = read(fd, b, sizeof b)) > 0) { total += r; if (total > 20000000) break; }
  close(fd);
  printf("smaps read_ok=%d\n", total > 0 && total < 20000000);
  return 0;
}
