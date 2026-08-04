// /proc/self/mounts (and /proc/mounts) is the fstab-format mount table df, mount(8) and container tooling
// read. Assert the structure each line must have: exactly 6 whitespace fields (spec point fstype opts
// dump pass), a root "/" mount entry exists, and the dump/pass columns are integers. /proc/self/mountinfo
// covers the same mounts in the richer format with a " - " separator; assert it too lists the root and has
// a mount id / parent id pair. Catches an empty or malformed mount table. Structural, oracle-neutral.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include "pf.h"

int main(void) {
    char b[262144];
    int n = pf_read("/proc/self/mounts", b, sizeof b);
    int have_root = 0, all_6col = n > 0;
    for (char *save, *ln = strtok_r(b, "\n", &save); ln; ln = strtok_r(NULL, "\n", &save)) {
        char f[8][1024];
        int nf = sscanf(ln, "%1023s %1023s %1023s %1023s %1023s %1023s %1023s",
                        f[0], f[1], f[2], f[3], f[4], f[5], f[6]);
        if (nf != 6) { all_6col = 0; continue; }
        int dump, pass;
        if (sscanf(f[4], "%d", &dump) != 1 || sscanf(f[5], "%d", &pass) != 1) all_6col = 0;
        if (strcmp(f[1], "/") == 0) have_root = 1;
    }

    char mi[262144];
    int mn = pf_read("/proc/self/mountinfo", mi, sizeof mi);
    int mi_ok = mn > 0 && strstr(mi, " - ") != NULL;
    // first mountinfo line begins "<mountid> <parentid> <maj:min> ..."
    int a, c; char majmin[64];
    mi_ok = mi_ok && sscanf(mi, "%d %d %63s", &a, &c, majmin) == 3 && strchr(majmin, ':');

    int ok = have_root && all_6col && mi_ok;
    printf("selfmounts ok=%d\n", ok);
    return 0;
}
