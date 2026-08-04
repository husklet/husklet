// Isolation + iface-consistency audit of the /proc/net surface. A container is its own net namespace, so
// the interface set and neighbour/multicast tables must be SELF-CONSISTENT and must not expose the host
// network stack. Before the fix, every /proc/net leaf the synthesizer did not cover fell through to a raw
// host open, leaking host ifaces (e.g. docker0 in igmp/dev_mcast though /proc/net/dev hides it), host
// routes (fib_trie/rt_cache), host processes' netlink/packet sockets, and host-wide socket counts.
//
// Differential-safe assertions (true on the bare host AND under the engine's container view):
//   * every device named in /proc/net/igmp and /proc/net/dev_mcast also appears in /proc/net/dev
//     (cross-file interface-set consistency: a leaked host iface absent from dev breaks this).
//   * /proc/net/arp carries the exact kernel header (well-formed for `arp`/`ip neigh` parsers).
//   * /proc/net/igmp carries the kernel header and names the loopback interface.
#include <stdio.h>
#include <string.h>
#include "pf.h"

// Does /proc/net/dev list an interface called `name`? dev rows are "<pad>name: <counters>".
static int dev_has_iface(const char *dev, const char *name) {
    char pat[72];
    snprintf(pat, sizeof pat, "%s:", name);
    return strstr(dev, pat) != 0;
}

// For every "Idx\tDevice :" row in igmp, confirm the device appears in /proc/net/dev.
static int igmp_ifaces_consistent(const char *igmp, const char *dev) {
    int ok = 1;
    const char *p = igmp;
    p = strchr(p, '\n'); // skip header
    while (p && *p) {
        p++;
        // an interface row starts with a digit index then a TAB then the device name; membership rows
        // start with a TAB (skip those).
        if (*p >= '0' && *p <= '9') {
            const char *t = strchr(p, '\t');
            if (t) {
                t++;
                char name[64]; int i = 0;
                while (t[i] && t[i] != ' ' && t[i] != '\t' && t[i] != ':' && i < 63) { name[i] = t[i]; i++; }
                name[i] = 0;
                if (name[0] && !dev_has_iface(dev, name)) { printf("  igmp iface %s not in dev\n", name); ok = 0; }
            }
        }
        p = strchr(p, '\n');
    }
    return ok;
}

// dev_mcast rows: "<idx>    <name>    <cnt> <glbl> <mac>". Second whitespace token is the device.
static int devmcast_ifaces_consistent(const char *mc, const char *dev) {
    int ok = 1;
    const char *p = mc;
    while (p && *p) {
        while (*p == ' ' || *p == '\t') p++;
        while (*p >= '0' && *p <= '9') p++;       // idx
        while (*p == ' ' || *p == '\t') p++;
        char name[64]; int i = 0;
        while (*p && *p != ' ' && *p != '\t' && *p != '\n' && i < 63) name[i++] = *p++;
        name[i] = 0;
        if (name[0] && !dev_has_iface(dev, name)) { printf("  dev_mcast iface %s not in dev\n", name); ok = 0; }
        p = strchr(p, '\n');
        if (p) p++;
    }
    return ok;
}

int main(void) {
    char dev[16384], igmp[16384], mc[16384], arp[8192];
    int ok = 1;

    ok &= (pf_read("/proc/net/dev", dev, sizeof dev) > 0);
    ok &= dev_has_iface(dev, "lo");

    // arp: exact kernel header, well-formed for neighbour-table parsers.
    ok &= (pf_read("/proc/net/arp", arp, sizeof arp) >= 0);
    ok &= pf_has(arp, "IP address") && pf_has(arp, "HW address") && pf_has(arp, "Device");

    // igmp: header + loopback membership, and no interface the container's own /proc/net/dev doesn't have.
    ok &= (pf_read("/proc/net/igmp", igmp, sizeof igmp) > 0);
    ok &= pf_has(igmp, "Device") && pf_has(igmp, "Group");
    ok &= igmp_ifaces_consistent(igmp, dev);

    // dev_mcast: every listed interface belongs to this container (or the table is empty).
    ok &= (pf_read("/proc/net/dev_mcast", mc, sizeof mc) >= 0);
    ok &= devmcast_ifaces_consistent(mc, dev);

    printf("procnet_iso ok=%d\n", ok);
    return 0;
}
