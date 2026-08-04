// poll over a large pollfd array: only the written pipe reports POLLIN; negative fds ignored.
#include <poll.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>

#define N 512

int main(void) {
    int fds[2];
    if (pipe(fds) != 0) { printf("pollbig pipe=0\n"); return 0; }

    struct pollfd *pf = calloc(N, sizeof *pf);
    for (int i = 0; i < N; i++) { pf[i].fd = -1; pf[i].events = POLLIN; } // negative -> ignored
    pf[N - 1].fd = fds[0];
    pf[N - 1].events = POLLIN;

    // No data: times out.
    int r0 = poll(pf, N, 50);
    int timed_out = r0 == 0 && pf[N - 1].revents == 0;

    write(fds[1], "hi", 2);
    int r1 = poll(pf, N, 1000);
    int one_ready = r1 == 1 && (pf[N - 1].revents & POLLIN);

    // Ensure the ignored (-1) slots never set revents.
    int clean = 1;
    for (int i = 0; i < N - 1; i++) if (pf[i].revents != 0) clean = 0;

    free(pf);
    close(fds[0]);
    close(fds[1]);
    printf("pollbig timeout=%d ready=%d clean=%d\n", timed_out, one_ready, clean);
    return 0;
}
