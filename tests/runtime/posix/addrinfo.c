// getaddrinfo numeric-only (AI_NUMERICHOST|AI_NUMERICSERV): no DNS, deterministic parse of v4/v6 + service.
#include <sys/types.h>
#include <sys/socket.h>
#include <netdb.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <string.h>
#include <stdio.h>

int main(void) {
    struct addrinfo hints = {0}, *res = NULL;
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_NUMERICHOST | AI_NUMERICSERV;

    int rc = getaddrinfo("192.0.2.55", "8080", &hints, &res);
    int v4_ok = 0;
    if (rc == 0 && res) {
        struct sockaddr_in *sa = (struct sockaddr_in *)res->ai_addr;
        char buf[64];
        inet_ntop(AF_INET, &sa->sin_addr, buf, sizeof buf);
        v4_ok = strcmp(buf, "192.0.2.55") == 0 && ntohs(sa->sin_port) == 8080 &&
                res->ai_family == AF_INET;
        freeaddrinfo(res);
    }

    // A hostname with AI_NUMERICHOST must NOT resolve (EAI_NONAME), never a DNS lookup.
    struct addrinfo h2 = {0};
    h2.ai_family = AF_INET;
    h2.ai_flags = AI_NUMERICHOST;
    res = NULL;
    int rc2 = getaddrinfo("example.invalid", NULL, &h2, &res);
    int rejected = rc2 == EAI_NONAME;
    if (res) freeaddrinfo(res);

    // IPv6 numeric.
    struct addrinfo h3 = {0}, *r6 = NULL;
    h3.ai_family = AF_INET6;
    h3.ai_flags = AI_NUMERICHOST;
    int rc3 = getaddrinfo("2001:db8::1", NULL, &h3, &r6);
    int v6_ok = 0;
    if (rc3 == 0 && r6) {
        struct sockaddr_in6 *s6 = (struct sockaddr_in6 *)r6->ai_addr;
        char buf[64];
        inet_ntop(AF_INET6, &s6->sin6_addr, buf, sizeof buf);
        v6_ok = strcmp(buf, "2001:db8::1") == 0;
        freeaddrinfo(r6);
    }

    printf("addrinfo v4=%d rejected=%d v6=%d\n", v4_ok, rejected, v6_ok);
    return 0;
}
