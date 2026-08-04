// The macOS engine backs Linux SOCK_SEQPACKET with DGRAM. DGRAM does not wake a peer when its endpoint
// disappears, so this stresses the hardest lifetime edge: a fork child closes before its first write.
// Linux returns EOF immediately after the final inherited/duplicated owner closes.
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int ok = 1;
    for (int round = 0; round < 200; round++) {
        int sv[2];
        if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv) != 0) return 2;
        int alias = dup(sv[1]);
        if (alias < 0) return 3;
        pid_t child = fork();
        if (child < 0) return 4;
        if (child == 0) {
            close(sv[0]);
            close(sv[1]);
            close(alias);
            _exit(0);
        }
        close(sv[1]);
        close(alias);
        struct pollfd p = {.fd = sv[0], .events = POLLIN | POLLHUP};
        char byte;
        int ready = poll(&p, 1, 2000);
        ssize_t n = ready > 0 ? read(sv[0], &byte, 1) : -1;
        int status = 0;
        while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
        if (ready <= 0 || n != 0 || !WIFEXITED(status) || WEXITSTATUS(status) != 0) ok = 0;
        close(sv[0]);
        if (!ok) break;
    }
    printf("seqpacket_lastclose rounds=200 ok=%d\n", ok);
    return ok ? 0 : 1;
}
