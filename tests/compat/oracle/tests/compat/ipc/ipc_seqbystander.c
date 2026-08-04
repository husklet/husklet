// SEQPACKET bystander-EOF guard (the multi-process application IPC child-bootstrap wall). When the coordinator forks a
// worker/service child, that child inherits ALL the coordinator's open fds -- including the coordinator's SEND end of
// a IPC channel meant for a DIFFERENT child. The bystander never uses it, but closes the inherited copy on
// startup. The DGRAM-backed SEQPACKET emulation must NOT synthesize a peer-EOF for an endpoint a process
// merely inherited and never wrote to: the old code injected a zero-length "EOF" datagram into the peer end
// -- the LIVE channel's recv queue -- so the real receiver read a premature 0 and gave up ("Terminating
// after 15 seconds with no connection"). Here: parent creates a SEQPACKET pair, a BYSTANDER child inherits
// both ends and closes them unused, then the parent sends a real record on its retained send end and reads
// it back on the recv end. The first read MUST be the real 4-byte record, never a spurious 0-length EOF.
// Booleans/lengths only -> native (real SEQPACKET refcounts) and guest (DGRAM emulation) print identically.
// Diffed vs the native Linux oracle.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) < 0) { printf("seqbystander socketpair_failed\n"); return 0; }
    pid_t b = fork();
    if (b == 0) {
        // bystander: inherited both ends, uses neither. Close the recv end first, then the SEND end -- the
        // order that made the old per-fd peer-tracking inject an EOF into the send end's live peer queue.
        close(sv[1]);
        close(sv[0]);
        _exit(0);
    }
    waitpid(b, NULL, 0); // let the bystander's closes (and any wrong injection) complete first
    const char *msg = "PING";
    write(sv[0], msg, 4); // parent is the genuine writer on its retained send end
    char buf[64];
    memset(buf, 0, sizeof buf);
    ssize_t n = read(sv[1], buf, sizeof buf); // first read must be the real record, not a bystander EOF(0)
    close(sv[0]);
    close(sv[1]);
    printf("seqbystander first_len=%ld data=%s\n", (long)n, buf); // 4 / PING
    return 0;
}
