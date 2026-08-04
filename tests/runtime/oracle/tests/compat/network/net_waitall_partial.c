// MSG_WAITALL blocks until the full requested length arrives: a peer that writes in
// two chunks still yields a single fully-filled recv, while a plain recv returns
// whatever chunk is available first. Deterministic byte counts -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    pid_t pid = fork();
    if (pid == 0) {
        // deterministic ordering: peek forces the reader to wait for the first chunk
        write(sv[0], "AAAA", 4);
        char sync[1];
        recv(sv[0], sync, 1, 0); // wait for reader ack before second chunk
        write(sv[0], "BBBB", 4);
        _exit(0);
    }
    char first[4];
    recv(sv[1], first, 4, MSG_WAITALL); // consume chunk 1
    write(sv[1], "k", 1);               // ack -> releases chunk 2
    char all[4] = {0};
    ssize_t n = recv(sv[1], all, 4, MSG_WAITALL);
    int st = 0;
    waitpid(pid, &st, 0);
    printf("waitall_len=%zd content=%.*s\n", n, (int)(n > 0 ? n : 0), all);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
