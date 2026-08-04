// The process command name is exposed identically through three /proc surfaces; after prctl(PR_SET_NAME)
// they must ALL reflect the new name: /proc/self/comm reads "<name>\n", /proc/self/status "Name:" column
// equals it, and /proc/self/stat field 2 is "(<name>)". This is the one place the comm value is knowable
// and stable (we set it ourselves), so it is asserted exactly. A synthesized comm that ignores prctl or
// caches a stale name (self-identity staleness) fails. Also verify prctl(PR_GET_NAME) round-trips.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include "pf.h"

int main(void) {
    const char *name = "probecomm";
    if (prctl(PR_SET_NAME, name, 0, 0, 0) != 0) { printf("selfcomm ok=0\n"); return 0; }
    char got[32] = {0};
    prctl(PR_GET_NAME, got, 0, 0, 0);
    int prctl_ok = strcmp(got, name) == 0;

    char b[8192], want[64];
    pf_read("/proc/self/comm", b, sizeof b);
    snprintf(want, sizeof want, "%s\n", name);
    int comm_ok = strcmp(b, want) == 0;

    pf_read("/proc/self/status", b, sizeof b);
    char v[64];
    int status_ok = pf_line_val(b, "Name:", v, sizeof v) && strcmp(v, name) == 0;

    pf_read("/proc/self/stat", b, sizeof b);
    snprintf(want, sizeof want, "(%s)", name);
    int stat_ok = pf_has(b, want);

    int ok = prctl_ok && comm_ok && status_ok && stat_ok;
    printf("selfcomm ok=%d\n", ok);
    return 0;
}
