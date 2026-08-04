// A socket the guest bind()+listen()s MUST appear in /proc/net/tcp with state 0A (TCP_LISTEN) -- this is
// what `ss -l` / `netstat -ln` inside the container parse to list listening services. On real Linux (and
// real docker) the kernel adds the row; a hl that synthesizes only the header made every listener invisible.
// Verdict is host-independent: we look ONLY for OUR OWN fixed port, so it holds on both native Linux and a
// correct hl. Fixed uncommon port 47251 (0xB893) + SO_REUSEADDR to avoid a stray collision.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>

#define PORT 47251

int main(void) {
    int s = socket(AF_INET, SOCK_STREAM, 0);
    int on = 1;
    setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK); // 127.0.0.1
    a.sin_port = htons(PORT);
    int br = bind(s, (struct sockaddr *)&a, sizeof a);
    int lr = listen(s, 8);

    // Scan /proc/net/tcp for a LISTEN row whose local port == PORT.
    int seen = 0, st_listen = 0;
    FILE *f = fopen("/proc/net/tcp", "r");
    if (f) {
        char line[512];
        int first = 1;
        while (fgets(line, sizeof line, f)) {
            if (first) { first = 0; continue; } // header
            unsigned sl, la, lp, ra, rp, st;
            if (sscanf(line, " %u: %x:%x %x:%x %x", &sl, &la, &lp, &ra, &rp, &st) >= 6) {
                if (lp == PORT) { seen = 1; if (st == 0x0A) st_listen = 1; }
            }
        }
        fclose(f);
    }
    printf("listen_tcp bind=%d listen=%d seen=%d st_listen=%d\n", br == 0, lr == 0, seen, st_listen);
    close(s);
    return 0;
}
