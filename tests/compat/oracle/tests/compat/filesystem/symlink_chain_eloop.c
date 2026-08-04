// Symlink resolution depth: a long chain resolves; a chain past the kernel cap and a
// self-referential loop both fail with ELOOP rather than hanging. An outer alarm guards
// against a runaway resolver.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    alarm(20);   // outer timeout: a hang must not pass

    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_eloop_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    // Terminal real file.
    close(openat(dfd, "end", O_CREAT | O_WRONLY, 0644));

    // Short chain of 8 links terminating at the real file resolves fine.
    symlinkat("end", dfd, "s0");
    for (int i = 1; i < 8; i++) {
        char cur[16], prev[16];
        snprintf(cur, sizeof cur, "s%d", i);
        snprintf(prev, sizeof prev, "s%d", i - 1);
        symlinkat(prev, dfd, cur);
    }
    int short_ok = faccessat(dfd, "s7", F_OK, 0) == 0;

    // Long chain of 60 links exceeds the 40-hop cap -> ELOOP.
    symlinkat("end", dfd, "L0");
    for (int i = 1; i < 60; i++) {
        char cur[16], prev[16];
        snprintf(cur, sizeof cur, "L%d", i);
        snprintf(prev, sizeof prev, "L%d", i - 1);
        symlinkat(prev, dfd, cur);
    }
    errno = 0;
    int deep = openat(dfd, "L59", O_RDONLY);
    int deep_eloop = deep < 0 && errno == ELOOP;
    if (deep >= 0) close(deep);

    // Self-referential loop -> ELOOP, never a hang.
    symlinkat("self", dfd, "self");
    errno = 0;
    int self = openat(dfd, "self", O_RDONLY);
    int self_eloop = self < 0 && errno == ELOOP;
    if (self >= 0) close(self);

    // O_NOFOLLOW on the final symlink component -> ELOOP.
    errno = 0;
    int nofollow = openat(dfd, "s0", O_RDONLY | O_NOFOLLOW);
    int nofollow_eloop = nofollow < 0 && errno == ELOOP;
    if (nofollow >= 0) close(nofollow);

    // Cleanup.
    for (int i = 0; i < 8; i++) { char n[16]; snprintf(n, sizeof n, "s%d", i); unlinkat(dfd, n, 0); }
    for (int i = 0; i < 60; i++) { char n[16]; snprintf(n, sizeof n, "L%d", i); unlinkat(dfd, n, 0); }
    unlinkat(dfd, "self", 0);
    unlinkat(dfd, "end", 0);
    close(dfd);
    rmdir(dir);
    printf("symlink-chain-eloop short=%d deep-eloop=%d self-eloop=%d nofollow-eloop=%d\n",
           short_ok, deep_eloop, self_eloop, nofollow_eloop);
    return 0;
}
