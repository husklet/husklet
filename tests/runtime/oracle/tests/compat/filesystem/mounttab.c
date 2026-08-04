// Mount-table structure that df/mount/findmnt parse, inside the alpine overlay rootfs. Guards the shape of
// /proc/mounts (fstab form, 6 fields, overlay root + proc/sysfs/tmpfs pseudo lines) and /proc/self/mountinfo
// (the 11+ field form with the "-" separator that findmnt/runc require). Verdict byte-identical to a correct
// container; a stub/empty/misfielded synth fails.
#include <stdio.h>
#include <string.h>

static int slurp(const char *f, char *buf, size_t n) {
    FILE *fp = fopen(f, "r");
    if (!fp) return -1;
    size_t got = fread(buf, 1, n - 1, fp);
    buf[got] = 0;
    fclose(fp);
    return (int)got;
}

// every non-empty line of `text` must split into at least `min` whitespace fields
static int min_fields(const char *text, int min) {
    char tmp[8192];
    strncpy(tmp, text, sizeof tmp - 1);
    tmp[sizeof tmp - 1] = 0;
    for (char *line = strtok(tmp, "\n"); line; line = strtok(NULL, "\n")) {
        int f = 0;
        for (char *p = line; *p;) {
            while (*p == ' ' || *p == '\t') p++;
            if (!*p) break;
            f++;
            while (*p && *p != ' ' && *p != '\t') p++;
        }
        if (f && f < min) return 0;
    }
    return 1;
}

int main(void) {
    char mounts[8192], minfo[8192];
    if (slurp("/proc/mounts", mounts, sizeof mounts) < 0 ||
        slurp("/proc/self/mountinfo", minfo, sizeof minfo) < 0) {
        printf("mounttab ok=0 (read failed)\n");
        return 0;
    }
    int ok =
        // /proc/mounts: overlay root + the pseudo filesystems df/mount enumerate; 6 fields per line
        strstr(mounts, "overlay / overlay") != NULL &&
        strstr(mounts, " /proc proc ") != NULL &&
        strstr(mounts, " /sys sysfs ") != NULL &&
        strstr(mounts, " /dev tmpfs ") != NULL &&
        min_fields(mounts, 6) &&
        // /proc/self/mountinfo: the "-" separator + overlay root mount; findmnt needs >=10 fields
        strstr(minfo, " / / rw") != NULL &&
        strstr(minfo, " - overlay overlay ") != NULL &&
        strstr(minfo, " - proc proc ") != NULL &&
        min_fields(minfo, 10);
    printf("mounttab ok=%d\n", ok ? 1 : 0);
    return 0;
}
