// The kernel identity strings must be self-consistent across the three surfaces software reads them from:
// uname(2), /proc/sys/kernel/{ostype,osrelease,hostname}, and /proc/version. Assert derived agreements
// (host-value-neutral): ostype == uname().sysname == "Linux"; osrelease == uname().release and that exact
// release string appears inside /proc/version; hostname file == uname().nodename == gethostname(2). A
// synthesized /proc that hardcodes a version banner inconsistent with uname fails.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/utsname.h>
#include <unistd.h>
#include "pf.h"

static void strip_nl(char *s) { char *n = strchr(s, '\n'); if (n) *n = 0; }

int main(void) {
    struct utsname u;
    int uok = uname(&u) == 0;

    char b[512];
    pf_read("/proc/sys/kernel/ostype", b, sizeof b); strip_nl(b);
    int ostype_ok = strcmp(b, "Linux") == 0 && strcmp(b, u.sysname) == 0;

    pf_read("/proc/sys/kernel/osrelease", b, sizeof b); strip_nl(b);
    int osrel_ok = strcmp(b, u.release) == 0;

    char ver[4096];
    pf_read("/proc/version", ver, sizeof ver);
    int ver_ok = strstr(ver, u.release) != NULL && !strncmp(ver, "Linux version ", 14);

    char host[256], gh[256];
    pf_read("/proc/sys/kernel/hostname", host, sizeof host); strip_nl(host);
    int gh_ok = gethostname(gh, sizeof gh) == 0;
    int host_ok = gh_ok && strcmp(host, u.nodename) == 0 && strcmp(host, gh) == 0;

    int ok = uok && ostype_ok && osrel_ok && ver_ok && host_ok;
    printf("unamehost ok=%d\n", ok);
    return 0;
}
