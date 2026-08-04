// Process groups & sessions: setpgid/getpgid/setsid/getsid consistency across fork. A child that
// setpgid(0,0) forms its own group (pgid==pid); a child that setsid() becomes a session+group leader
// with getsid==pid and no controlling terminal. Reported as booleans (no raw pid/pgid/sid values).
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    // child A: forms its own process group
    pid_t a = fork();
    if (a == 0) {
        if (setpgid(0, 0) != 0) _exit(1);
        pid_t me = getpid();
        _exit(getpgid(0) == me ? 0 : 2);   // leader of its own group
    }
    int sa = 0; waitpid(a, &sa, 0);
    int own_group = WIFEXITED(sa) && WEXITSTATUS(sa) == 0;

    // parent-driven setpgid: create B, then move B into its own new group by pgid==pid from the parent
    // side (the other classic job-control path: the shell, not the child, sets the group).
    pid_t b = fork();
    if (b == 0) {
        while (getpgid(0) == getppid() ) usleep(2 * 1000); // block until moved out of parent's group
        _exit(getpgid(0) == getpid() ? 0 : 3);
    }
    setpgid(b, b);                          // parent places B in a fresh group named by B's own pid
    int sb = 0; waitpid(b, &sb, 0);
    int joined_group = WIFEXITED(sb) && WEXITSTATUS(sb) == 0;

    // child C: new session -> session leader, getsid==pid==pgid
    pid_t c = fork();
    if (c == 0) {
        pid_t sid = setsid();
        pid_t me = getpid();
        int leader = sid == me && getsid(0) == me && getpgid(0) == me;
        _exit(leader ? 0 : 4);
    }
    int sc = 0; waitpid(c, &sc, 0);
    int new_session = WIFEXITED(sc) && WEXITSTATUS(sc) == 0;

    printf("pgroup own_group=%d joined_group=%d new_session=%d\n",
           own_group, joined_group, new_session);
    return 0;
}
