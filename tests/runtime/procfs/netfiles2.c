// Round 2 of the /proc/net audit (networking + process-listing tool class). Asserts the STRUCTURE that
// ss/netstat/ip parse for files the first pass missed or got wrong vs real docker:
//   * tcp6/udp6 use the WIDE v6 header ("remote_address" + 32-hex address columns), NOT the v4 header.
//   * /proc/net/netstat exposes the TcpExt + IpExt extended-counter sections (netstat -s).
//   * /proc/net/snmp6 exposes the IPv6 counter table (netstat -s IPv6 section).
//   * /proc/net/ipv6_route exists with the loopback route table (ip -6 route / netstat -6 -r).
//   * the namespaced mirrors /proc/self/net/* and /proc/<pid>/net/* resolve to the shared table.
// Each was a hl-only divergence (missing file / wrong header) before the fix.
#include <stdio.h>
#include <string.h>
#include <dirent.h>
#include "pf.h"

// first line of a pseudo-file into `out`
static void first_line(const char *hay, char *out, int cap) {
    int i = 0;
    while (hay[i] && hay[i] != '\n' && i < cap - 1) { out[i] = hay[i]; i++; }
    out[i] = 0;
}

int main(void) {
    char b[16384], hdr[512];
    int ok = 1;

    // tcp6: DISTINCT wide header from tcp4 (v6 addr columns are 32 hex; label is "remote_address").
    pf_read("/proc/net/tcp6", b, sizeof b);
    first_line(b, hdr, sizeof hdr);
    ok &= pf_has(hdr, "local_address") && pf_has(hdr, "remote_address");

    // udp6: same wide header + the udp-only "ref pointer drops" tail.
    pf_read("/proc/net/udp6", b, sizeof b);
    first_line(b, hdr, sizeof hdr);
    ok &= pf_has(hdr, "remote_address") && pf_has(hdr, "drops");

    // netstat: the TcpExt / IpExt sections `netstat -s` parses.
    ok &= (pf_read("/proc/net/netstat", b, sizeof b) > 0);
    ok &= pf_has(b, "TcpExt:") && pf_has(b, "IpExt:") && pf_has(b, "TCPFastRetrans") && pf_has(b, "InOctets");

    // snmp6: the IPv6 counter table.
    ok &= (pf_read("/proc/net/snmp6", b, sizeof b) > 0);
    ok &= pf_has(b, "Ip6InReceives") && pf_has(b, "Udp6InDatagrams");

    // ipv6_route: loopback route table (the ::1/128 host route names lo, 32-hex dest column).
    ok &= (pf_read("/proc/net/ipv6_route", b, sizeof b) > 0);
    ok &= pf_has(b, "00000000000000000000000000000001") && pf_has(b, "lo");

    // namespaced mirrors: /proc/self/net/* and /proc/<pid>/net/* == the shared /proc/net/* table.
    ok &= (pf_read("/proc/self/net/tcp", b, sizeof b) > 0) && pf_has(b, "local_address");
    ok &= (pf_read("/proc/1/net/dev", b, sizeof b) > 0) && pf_has(b, "eth0:");

    // /sys/class/net/eth0/statistics/* (node_exporter/ifstat read these per-interface counters directly).
    // A digit-leading numeric value on both real Linux (live counter) and hl (zero counter).
    ok &= (pf_read("/sys/class/net/eth0/statistics/rx_bytes", b, sizeof b) > 0) && b[0] >= '0' && b[0] <= '9';
    ok &= (pf_read("/sys/class/net/eth0/statistics/tx_packets", b, sizeof b) > 0) && b[0] >= '0' && b[0] <= '9';
    // the statistics/ subdir is enumerable (opendir/getdents) and lists rx_bytes.
    {
        DIR *d = opendir("/sys/class/net/eth0/statistics");
        int saw = 0;
        if (d) {
            struct dirent *e;
            while ((e = readdir(d)))
                if (!strcmp(e->d_name, "rx_bytes")) saw = 1;
            closedir(d);
        }
        ok &= saw;
    }

    printf("netfiles2 ok=%d\n", ok);
    return 0;
}
