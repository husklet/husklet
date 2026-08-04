// getcpu(2) and sysinfo(2) shape contract: both succeed and populate their out-params with
// well-formed values. getcpu writes a non-garbage cpu and node index (both distinct from the
// poisoned sentinel we pre-seed); sysinfo reports a positive total RAM, a positive mem_unit
// scale, and at least one running process. Only success + positivity booleans are asserted --
// never a concrete count or byte total -- so the line is host-invariant across backends.
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <sys/sysinfo.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    unsigned cpu = 0xffffffu, node = 0xffffffu;
    int gr = (int)syscall(SYS_getcpu, &cpu, &node, 0);
    int getcpu_ok = gr == 0 && cpu != 0xffffffu && node != 0xffffffu;

    struct sysinfo si;
    int sr = sysinfo(&si);
    int sysinfo_ok = sr == 0;
    int ram_pos = si.totalram > 0;
    int unit_pos = si.mem_unit > 0;
    int procs_pos = si.procs > 0;

    printf("getcpu-sysinfo getcpu_ok=%d sysinfo_ok=%d ram_pos=%d unit_pos=%d procs_pos=%d\n",
           getcpu_ok, sysinfo_ok, ram_pos, unit_pos, procs_pos);
    return 0;
}
