// inet_pton / inet_ntop round-trips and rejection of malformed addresses.
#include <arpa/inet.h>
#include <netinet/in.h>
#include <string.h>
#include <stdio.h>

int main(void) {
    struct in_addr a4;
    int p4 = inet_pton(AF_INET, "10.1.2.3", &a4) == 1 && ntohl(a4.s_addr) == 0x0a010203u;
    char b4[INET_ADDRSTRLEN];
    int n4 = inet_ntop(AF_INET, &a4, b4, sizeof b4) != NULL && strcmp(b4, "10.1.2.3") == 0;

    // Malformed v4 rejected.
    struct in_addr bad;
    int rej4 = inet_pton(AF_INET, "256.0.0.1", &bad) == 0;
    int rej4b = inet_pton(AF_INET, "1.2.3", &bad) == 0;

    struct in6_addr a6;
    int p6 = inet_pton(AF_INET6, "fe80::1", &a6) == 1;
    char b6[INET6_ADDRSTRLEN];
    int n6 = inet_ntop(AF_INET6, &a6, b6, sizeof b6) != NULL && strcmp(b6, "fe80::1") == 0;

    // Canonicalization: leading zeros / compressible form normalize.
    struct in6_addr a6b;
    inet_pton(AF_INET6, "2001:0db8:0000:0000:0000:0000:0000:0007", &a6b);
    char b6b[INET6_ADDRSTRLEN];
    int canon = inet_ntop(AF_INET6, &a6b, b6b, sizeof b6b) != NULL && strcmp(b6b, "2001:db8::7") == 0;

    int rej6 = inet_pton(AF_INET6, "fe80:::1", &a6) == 0;

    printf("inetpton p4=%d n4=%d rej4=%d rej4b=%d p6=%d n6=%d canon=%d rej6=%d\n",
           p4, n4, rej4, rej4b, p6, n6, canon, rej6);
    return 0;
}
