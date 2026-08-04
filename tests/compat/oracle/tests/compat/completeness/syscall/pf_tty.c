#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <dirent.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

/* Linux exposes /proc/tty and readable metadata such as /proc/tty/drivers; hl returned ENOENT. */

int main(void) {
  struct stat st;
  int dir_ok = (stat("/proc/tty", &st) == 0 && S_ISDIR(st.st_mode));
  DIR *d = opendir("/proc/tty"); if (d) closedir(d); else dir_ok = 0;
  int drivers_ok = 0;
  int fd = open("/proc/tty/drivers", O_RDONLY);
  if (fd >= 0) { char b[64]; drivers_ok = read(fd, b, sizeof b) > 0; close(fd); }
  printf("proctty dir_ok=%d drivers_ok=%d\n", dir_ok, drivers_ok);
  return 0;
}
