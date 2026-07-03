// /proc/net/{dev,route,tcp,tcp6,udp,unix,snmp,if_inet6}. Assert the header/columns real parsers (ss,
// netstat, glibc getifaddrs, the JVM) expect. snmp in particular must expose the Ip/Tcp/Udp sections
// (a header-only "minimal" snmp is a stub that breaks `netstat -s`/`ss -s`).
#include <stdio.h>
#include <string.h>
#include "pf.h"

int main(void) {
    char b[8192];
    int ok = 1;

    pf_read("/proc/net/dev", b, sizeof b);
    ok &= pf_has(b, "Receive") && pf_has(b, "Transmit") && pf_has(b, "lo:");

    pf_read("/proc/net/route", b, sizeof b);
    ok &= pf_has(b, "Iface") && pf_has(b, "Destination") && pf_has(b, "Gateway");

    pf_read("/proc/net/tcp", b, sizeof b);
    ok &= pf_has(b, "local_address") && pf_has(b, "rem_address") && pf_has(b, "inode");

    pf_read("/proc/net/udp", b, sizeof b);
    ok &= pf_has(b, "local_address") && pf_has(b, "rem_address");

    pf_read("/proc/net/unix", b, sizeof b);
    ok &= pf_has(b, "RefCount") && pf_has(b, "Inode");

    pf_read("/proc/net/if_inet6", b, sizeof b);
    ok &= pf_has(b, "lo"); // ::1 loopback line names lo

    // snmp: the Ip/Tcp/Udp/Icmp protocol counter sections netstat -s parses.
    pf_read("/proc/net/snmp", b, sizeof b);
    ok &= pf_has(b, "Ip:") && pf_has(b, "Tcp:") && pf_has(b, "Udp:") && pf_has(b, "Icmp:");

    printf("netfiles ok=%d\n", ok);
    return 0;
}
