#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/un.h>

/* A bound AF_UNIX PATH socket must appear in /proc/net/unix (socket-inventory tools rely on it). hl emitted
   only the header. Bind a uniquely-named socket, then check its path shows up. Oracle-diffed vs native. */

int main(void) {
  char path[80];
  snprintf(path, sizeof path, "/tmp/pfnu_%d.sock", (int)getpid());
  unlink(path);
  int fd = socket(AF_UNIX, SOCK_STREAM, 0);
  struct sockaddr_un un; memset(&un, 0, sizeof un);
  un.sun_family = AF_UNIX;
  snprintf(un.sun_path, sizeof un.sun_path, "%s", path);
  int b = bind(fd, (struct sockaddr *)&un, sizeof un);
  listen(fd, 1);

  int has_path = 0;
  FILE *f = fopen("/proc/net/unix", "r");
  if (f) { char line[512]; while (fgets(line, sizeof line, f)) if (strstr(line, path)) { has_path = 1; break; } fclose(f); }
  close(fd);
  unlink(path);
  printf("net_unix bind_ok=%d has_path=%d\n", b == 0, has_path);
  return 0;
}
