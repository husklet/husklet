// Orphaned process group with a STOPPED member: when the pgroup becomes orphaned (its last
// out-of-group, same-session parent exits), the kernel sends SIGHUP then SIGCONT to it so a
// stopped job cannot hang forever. Deterministic via a grandparent pipe (no ctty needed).
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <sys/wait.h>

static volatile sig_atomic_t got_hup, got_cont;
static void on_hup(int s){ (void)s; got_hup=1; }
static void on_cont(int s){ (void)s; got_cont=1; }

int main(void) {
    int out[2]; if (pipe(out) < 0) return 1;
    pid_t c = fork();
    if (c == 0) {
        // C: own session so W's group is same-session but parented across groups.
        setsid();
        close(out[0]);
        pid_t w = fork();
        if (w == 0) {
            struct sigaction sa; memset(&sa,0,sizeof sa);
            sa.sa_handler = on_hup;  sigaction(SIGHUP, &sa, 0);
            sa.sa_handler = on_cont; sigaction(SIGCONT, &sa, 0);
            setpgid(0, 0);                 // W in its own (background) group of C's session
            raise(SIGSTOP);                // stopped member
            for (int i=0;i<3000 && !got_hup;i++) usleep(1000);
            unsigned char res = (got_hup?1:0)|(got_cont?2:0);
            write(out[1], &res, 1);
            _exit(0);
        }
        setpgid(w, w);
        int st; waitpid(w, &st, WUNTRACED); // returns once W is stopped
        _exit(0);                           // C exit orphans W's group -> SIGHUP+SIGCONT to stopped W
    }
    close(out[1]);
    unsigned char res = 0xff;
    read(out[0], &res, 1);
    int st; waitpid(c, &st, 0);
    printf("orphan stopped_sighup=%d stopped_sigcont=%d\n", (res&1)!=0, (res&2)!=0);
    return 0;
}
