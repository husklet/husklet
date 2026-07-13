// #348: the /proc + /proc/sys surface real software reads. boot_id must be a valid UUID that is STABLE
// across reads (systemd/dbus/libuuid/curl key machine state off it -> a missing file made curl print
// "cannot find current boot id"); uuid must be a valid UUID that is FRESH on every read; and the broad
// sysctl/config surface (pid_max, cap_last_cap, somaxconn, max_map_count, file-max, self/limits, ...)
// must be present + non-empty. Self-checking (no oracle: the exact values are host/boot-specific, but the
// FORMAT + stability/freshness/presence invariants are universal).
#define _GNU_SOURCE
#include <ctype.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

// Read a whole small file into buf (NUL-terminated). Returns byte count, -1 on error.
static int slurp(const char *p, char *buf, int cap) {
    FILE *f = fopen(p, "r");
    if (!f) return -1;
    int n = (int)fread(buf, 1, (size_t)cap - 1, f);
    fclose(f);
    if (n < 0) return -1;
    buf[n] = 0;
    return n;
}

// A canonical Linux UUID line: "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx\n" (36 hex/dashes + newline).
static int is_uuid(const char *s) {
    static const int dash[36] = {[8] = 1, [13] = 1, [18] = 1, [23] = 1};
    if (strlen(s) != 37 || s[36] != '\n') return 0;
    for (int i = 0; i < 36; i++) {
        if (dash[i]) { if (s[i] != '-') return 0; }
        else if (!isxdigit((unsigned char)s[i])) return 0;
    }
    return 1;
}

int main(void) {
    char a[4096], b[4096];
    // boot_id: valid UUID, identical across two reads.
    int ra = slurp("/proc/sys/kernel/random/boot_id", a, sizeof a);
    int rb = slurp("/proc/sys/kernel/random/boot_id", b, sizeof b);
    int boot_ok = ra > 0 && rb > 0 && is_uuid(a) && !strcmp(a, b);
    if (boot_ok) printf("BOOTID_OK\n"); else printf("BOOTID_FAIL ra=%d valid=%d stable=%d\n", ra, ra > 0 && is_uuid(a), ra > 0 && rb > 0 && !strcmp(a, b));
    // uuid: valid UUID, DIFFERS between two reads.
    int ua = slurp("/proc/sys/kernel/random/uuid", a, sizeof a);
    int ub = slurp("/proc/sys/kernel/random/uuid", b, sizeof b);
    int uuid_ok = ua > 0 && ub > 0 && is_uuid(a) && is_uuid(b) && strcmp(a, b);
    if (uuid_ok) printf("UUID_OK\n"); else printf("UUID_FAIL ua=%d valid=%d fresh=%d\n", ua, ua > 0 && is_uuid(a), ua > 0 && ub > 0 && strcmp(a, b) != 0);
    // Broad surface: every path present + non-empty (representative of the full /proc/sys set).
    static const char *const files[] = {
        "/proc/sys/kernel/ostype", "/proc/sys/kernel/osrelease", "/proc/sys/kernel/pid_max",
        "/proc/sys/kernel/cap_last_cap", "/proc/sys/kernel/threads-max", "/proc/sys/kernel/ngroups_max",
        "/proc/sys/kernel/random/entropy_avail", "/proc/sys/vm/max_map_count", "/proc/sys/vm/mmap_min_addr",
        "/proc/sys/vm/overcommit_memory", "/proc/sys/vm/swappiness", "/proc/sys/net/core/somaxconn",
        "/proc/sys/net/ipv4/ip_local_port_range", "/proc/sys/net/ipv4/tcp_keepalive_time",
        "/proc/sys/fs/file-max", "/proc/sys/fs/nr_open", "/proc/sys/fs/inotify/max_user_watches",
        "/proc/cmdline", "/proc/filesystems", "/proc/self/limits", "/proc/self/oom_score_adj", 0};
    int surface_ok = 1;
    for (int i = 0; files[i]; i++) {
        int n = slurp(files[i], a, sizeof a);
        if (n <= 0) { surface_ok = 0; printf("MISSING %s (rc=%d)\n", files[i], n); }
    }
    // self/limits must actually carry the rlimit rows tools grep for.
    int ln = slurp("/proc/self/limits", a, sizeof a);
    if (ln <= 0 || !strstr(a, "Max open files")) { surface_ok = 0; printf("LIMITS_BAD\n"); }
    if (surface_ok) printf("SURFACE_OK\n"); else printf("SURFACE_FAIL\n");
    return 0;
}
