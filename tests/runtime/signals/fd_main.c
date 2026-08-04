#include <string.h>

#include "edges.c"
#include "epoll.c"
#include "fork.c"

int main(int argc, char **argv) {
    if (argc != 2) return 64;
    if (strcmp(argv[1], "edges") == 0) return signalfd_edges();
    if (strcmp(argv[1], "epoll") == 0) return signalfd_epoll();
    if (strcmp(argv[1], "fork") == 0) return signalfd_fork();
    return 64;
}
