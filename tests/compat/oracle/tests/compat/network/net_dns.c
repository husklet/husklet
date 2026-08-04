// Container DNS interception (task #261): the guest sends a DNS A query for "localhost" to the embedded
// nameserver 127.0.0.11:53 -- exactly as glibc/musl's resolver does after reading /etc/resolv.conf. The
// engine parses the query, resolves it via the macOS HOST resolver (getaddrinfo), and synthesizes the
// wire-format answer; recvfrom reports the source as 127.0.0.11:53 (the anti-spoofing check real resolvers
// make). "localhost" resolves to 127.0.0.1 on every host, so the golden output is deterministic and needs
// no external network. Linux-only: macOS has no 127.0.0.11 responder, so there is no native oracle.
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>
#include <sys/socket.h>
#include <netinet/in.h>

// Encode a dotted name into DNS label format; returns bytes written.
static int enc(unsigned char *o, const char *name) {
    int p = 0;
    const char *s = name;
    while (*s) {
        const char *d = strchr(s, '.');
        int l = d ? (int)(d - s) : (int)strlen(s);
        o[p++] = (unsigned char)l;
        memcpy(o + p, s, l);
        p += l;
        if (!d) break;
        s = d + 1;
    }
    o[p++] = 0;
    return p;
}

int main(void) {
    unsigned char q[256];
    int p = 0;
    q[p++] = 0x12; q[p++] = 0x34; // id
    q[p++] = 0x01; q[p++] = 0x00; // flags: RD
    q[p++] = 0; q[p++] = 1;       // qdcount
    q[p++] = 0; q[p++] = 0;       // ancount
    q[p++] = 0; q[p++] = 0;       // nscount
    q[p++] = 0; q[p++] = 0;       // arcount
    p += enc(q + p, "localhost");
    q[p++] = 0; q[p++] = 1; // qtype A
    q[p++] = 0; q[p++] = 1; // qclass IN

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in ns;
    memset(&ns, 0, sizeof ns);
    ns.sin_family = AF_INET;
    ns.sin_port = htons(53);
    ns.sin_addr.s_addr = inet_addr("127.0.0.11");
    if (connect(fd, (struct sockaddr *)&ns, sizeof ns) < 0) { printf("dns connect fail\n"); return 1; }
    if (send(fd, q, p, 0) != p) { printf("dns send fail\n"); return 1; }

    unsigned char r[1500];
    struct sockaddr_in from;
    socklen_t fl = sizeof from;
    int n = recvfrom(fd, r, sizeof r, 0, (struct sockaddr *)&from, &fl);
    if (n < 12) { printf("dns recv short n=%d\n", n); return 1; }

    int src_ok = (from.sin_addr.s_addr == inet_addr("127.0.0.11")) && (ntohs(from.sin_port) == 53);
    int rcode = r[3] & 0xf;
    int anc = (r[6] << 8) | r[7];
    // Walk past the header + echoed question to the first answer's A rdata.
    int o = 12;
    while (o < n && r[o]) o += r[o] + 1;
    o += 1 + 4; // qname terminator + qtype + qclass
    char ip[64] = "";
    if (anc >= 1 && o + 12 <= n) {
        if ((r[o] & 0xc0) == 0xc0) o += 2; else { while (o < n && r[o]) o += r[o] + 1; o++; }
        int type = (r[o] << 8) | r[o + 1];
        o += 8; // type(2) class(2) ttl(4)
        int rdl = (r[o] << 8) | r[o + 1];
        o += 2;
        if (type == 1 && rdl == 4) snprintf(ip, sizeof ip, "%d.%d.%d.%d", r[o], r[o + 1], r[o + 2], r[o + 3]);
    }
    printf("dns localhost=%s rcode=%d an=%d src_ok=%d\n", ip, rcode, anc, src_ok);
    return 0;
}
