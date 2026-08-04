// syscall-compat coverage: sysinfo/uname/times report coherent Linux facts. sysinfo: totalram>0,
// mem_unit>0, procs>0. uname: sysname is "Linux", release/version/machine are non-empty. times(): returns a
// valid tick count and the clock tick rate is positive. Arch-neutral: no raw sizes/strings printed (the
// machine string differs by ISA), only stable booleans and the literal sysname.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/sysinfo.h>
#include <sys/times.h>
#include <sys/utsname.h>
#include <unistd.h>

int main(void) {
    struct sysinfo si;
    sysinfo(&si);
    printf("totalram_pos=%d memunit_pos=%d procs_pos=%d\n", si.totalram > 0, si.mem_unit > 0, si.procs > 0);

    struct utsname u;
    uname(&u);
    printf("sysname=%s\n", u.sysname);
    printf("release_ne=%d version_ne=%d machine_ne=%d\n", u.release[0] != 0, u.version[0] != 0, u.machine[0] != 0);

    struct tms t;
    clock_t c = times(&t);
    printf("times_valid=%d clk_pos=%d\n", c != (clock_t)-1, sysconf(_SC_CLK_TCK) > 0);
    return 0;
}
