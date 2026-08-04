// exec preserves/resets across image replacement:
//   - FD_CLOEXEC descriptors are closed on exec; plain ones survive with their offset.
//   - A signal handler is reset to SIG_DFL, but an explicitly SIG_IGN'd signal stays ignored.
// The child re-execs this binary in "probe" mode and reports what it inherited via exit code bits.
#define _GNU_SOURCE
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

// probe mode: fd numbers passed as argv[2]=keep, argv[3]=cloexec
static int probe_main(char **argv) {
    int keep = atoi(argv[2]);
    int cle = atoi(argv[3]);
    // keep fd should be open and readable; cloexec fd should be closed (EBADF)
    char buf[8];
    int keep_open = read(keep, buf, 1) == 1; // one byte available: we wrote "K"
    int cle_closed = (fcntl(cle, F_GETFD) == -1);
    // SIGTERM disposition must be DFL after exec; SIGUSR1 was SIG_IGN and must remain ignored.
    struct sigaction st, su;
    sigaction(SIGTERM, NULL, &st);
    sigaction(SIGUSR1, NULL, &su);
    int term_dfl = st.sa_handler == SIG_DFL;
    int usr1_ign = su.sa_handler == SIG_IGN;
    return (keep_open ? 1 : 0) | (cle_closed ? 2 : 0) | (term_dfl ? 4 : 0) | (usr1_ign ? 8 : 0);
}

static void handler(int s) { (void)s; }

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "probe") == 0) return probe_main(argv);

    // a surviving pipe fd carrying one byte, and a CLOEXEC pipe fd
    int keep_fd, cle_fd;
    int p1[2], p2[2];
    if (pipe(p1) != 0 || pipe(p2) != 0) { printf("pipe fail\n"); return 1; }
    if (write(p1[1], "K", 1) != 1) { printf("write fail\n"); return 1; }
    keep_fd = p1[0];               // no CLOEXEC -> survives exec
    cle_fd = p2[0];
    fcntl(cle_fd, F_SETFD, FD_CLOEXEC); // survives fork, closed on exec

    signal(SIGTERM, handler);      // custom handler -> reset to DFL on exec
    signal(SIGUSR1, SIG_IGN);      // ignored -> stays ignored on exec

    pid_t pid = fork();
    if (pid == 0) {
        char ka[16], ca[16];
        snprintf(ka, sizeof ka, "%d", keep_fd);
        snprintf(ca, sizeof ca, "%d", cle_fd);
        char *cargv[] = { argv[0], (char *)"probe", ka, ca, NULL };
        execv(argv[0], cargv);
        _exit(100);
    }
    int st = 0;
    waitpid(pid, &st, 0);
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    printf("keep_open=%d cloexec_closed=%d term_reset=%d usr1_ign=%d\n",
           (code & 1) ? 1 : 0, (code & 2) ? 1 : 0, (code & 4) ? 1 : 0, (code & 8) ? 1 : 0);
    printf("exec_cloexec_signal done\n");
    return 0;
}
