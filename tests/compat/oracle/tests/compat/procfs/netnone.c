// --network none: eth0 must be ABSENT on EVERY access path, not just readdir. A tool that probes
// /sys/class/net/eth0 directly (stat, or open of an attribute file) must also see ENOENT -- otherwise
// direct lookup contradicts the readdir listing that already hides eth0 under isolation. lo stays.
// Runs under HL_NET_ISOLATE=1 (docker --network none). Verdict ok=1 iff isolation is path-consistent.
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    int ok = 1;
    // 1) readdir: lo present, eth0 hidden.
    DIR *d = opendir("/sys/class/net");
    if (!d) {
        printf("netnone ok=0 opendir\n");
        return 0;
    }
    int saw_lo = 0, saw_eth = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!strcmp(e->d_name, "lo")) saw_lo = 1;
        if (!strcmp(e->d_name, "eth0")) saw_eth = 1;
    }
    closedir(d);
    ok &= saw_lo && !saw_eth;
    // 2) direct stat: lo resolves, eth0 must ENOENT (before the fix it stat'd as a directory).
    struct stat st;
    ok &= (stat("/sys/class/net/lo", &st) == 0);
    ok &= (stat("/sys/class/net/eth0", &st) == -1 && errno == ENOENT);
    // 3) direct open of an attribute file: lo/address serves, eth0/address must fail (before the fix it
    //    returned the eth0 MAC through the content path even though readdir hid the interface).
    int f = open("/sys/class/net/lo/address", O_RDONLY);
    ok &= (f >= 0);
    if (f >= 0) close(f);
    f = open("/sys/class/net/eth0/address", O_RDONLY);
    ok &= (f < 0);
    if (f >= 0) close(f);
    printf("netnone ok=%d\n", ok);
    return 0;
}
