#include "compat.h"
#include <stdio.h>
#include <sys/vfs.h>
#include <sys/statfs.h>

/* Linux reports pseudo-fs magic + mount flags for procfs/sysfs paths. hl returned ENOENT for path statfs
   of a synthetic proc leaf and zeroed f_flags. Check the magic + nonzero flags; oracle-diffed vs native. */

int main(void) {
  struct statfs a, b, s;
  int proc_magic = (statfs("/proc/meminfo", &a) == 0 && (unsigned long)a.f_type == 0x9fa0UL);
  int sys_magic  = (statfs("/sys", &s) == 0 && (unsigned long)s.f_type == 0x62656572UL);
  int proc_flags = (statfs("/proc", &b) == 0 && b.f_flags != 0);
  printf("statfs proc_magic=%d sys_magic=%d proc_flags_nz=%d\n", proc_magic, sys_magic, proc_flags);
  return 0;
}
