// /proc/self/status `Umask:` must reflect the guest's LIVE file-mode creation mask, not a constant. The engine
// hardcoded "Umask:\t0022", so a guest umask(2) changed real inode modes yet the status line stayed 0022 -- a
// syscall-vs-/proc disagreement. Assert the round-trip: umask(2) returns the previous mask (== the value the
// status line showed) and each change is mirrored in the next status read.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
#include "pf.h"

static long status_umask(void) {
    char b[8192], v[32];
    if (pf_read("/proc/self/status", b, sizeof b) <= 0) return -1;
    if (!pf_line_val(b, "Umask:", v, sizeof v)) return -1;
    return strtol(v, 0, 8); // rendered as octal (e.g. "0022")
}

int main(void) {
    long before = status_umask();
    // umask(2) returns the previous mask; it must equal what /proc reported a moment ago.
    mode_t prev = umask(0077);
    long after = status_umask();
    // change again and confirm both the returned previous mask and the next status read track it.
    mode_t prev2 = umask(0026);
    long after2 = status_umask();
    (void)umask((mode_t)before); // restore

    int ok = before >= 0 && (long)prev == before && after == 0077 && (long)prev2 == 0077 && after2 == 0026;
    if (ok)
        printf("selfumask ok=1\n");
    else
        printf("selfumask ok=0 before=%04lo prev=%04o after=%04lo prev2=%04o after2=%04lo\n", before, prev, after,
               prev2, after2);
    return 0;
}
